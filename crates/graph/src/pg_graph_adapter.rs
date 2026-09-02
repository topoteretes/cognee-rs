//! PostgreSQL graph adapter — stores graph data in two tables (`graph_node`, `graph_edge`)
//! using JSONB properties and recursive CTEs for traversal.
//!
//! Ported from the Python `cognee` SDK (`cognee/infrastructure/databases/graph/postgres/`).
//!
//! The adapter assumes the target PostgreSQL database already exists (matching the
//! `cognee-database` crate pattern). Graph-specific tables are created via an inline
//! SeaORM migration that runs on first connection.
//!
//! # Query strategy
//!
//! Simple CRUD operations use SeaORM's `sea_query` builder for type-safe,
//! parameterised queries. Complex graph queries (recursive CTEs, UNION ALL,
//! JOINs with CASE expressions) use raw SQL via [`Statement::from_sql_and_values`]
//! because they exceed the query builder's expressiveness.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Alias, Cond, Expr, Iden, OnConflict, Query};
use sea_orm::{ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use tracing::debug;

use cognee_utils::sanitize::{sanitize_json, sanitize_str, sanitize_string};

use crate::error::{GraphDBError, GraphDBResult};
use crate::traits::GraphDBTrait;
use crate::types::{EdgeData, GraphNode, NodeData, parse_audit_timestamp};

/// Max rows per INSERT batch (6 params per node row, 6 per edge row → 600 params at 100 rows,
/// well within PostgreSQL's limit). Matches `PgVectorAdapter::BATCH_SIZE`.
const BATCH_SIZE: usize = 100;

/// Only these column names may appear in dynamic WHERE clauses to prevent SQL injection.
const ALLOWED_FILTER_ATTRS: &[&str] = &["id", "name", "type"];

// ---------------------------------------------------------------------------
// Table / column identifiers for sea_query
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum GNode {
    Table,
    Id,
    Name,
    Type,
    Properties,
    CreatedAt,
    UpdatedAt,
}

impl Iden for GNode {
    #[allow(
        clippy::expect_used,
        reason = "writing a static &str into the fmt::Write sink is infallible"
    )]
    fn unquoted(&self, s: &mut dyn fmt::Write) {
        write!(
            s,
            "{}",
            match self {
                Self::Table => "graph_node",
                Self::Id => "id",
                Self::Name => "name",
                Self::Type => "type",
                Self::Properties => "properties",
                Self::CreatedAt => "created_at",
                Self::UpdatedAt => "updated_at",
            }
        )
        .expect("write to string cannot fail");
    }
}

#[derive(Clone, Copy)]
enum GEdge {
    Table,
    SourceId,
    TargetId,
    RelationshipName,
    Properties,
    CreatedAt,
    UpdatedAt,
}

impl Iden for GEdge {
    #[allow(
        clippy::expect_used,
        reason = "writing a static &str into the fmt::Write sink is infallible"
    )]
    fn unquoted(&self, s: &mut dyn fmt::Write) {
        write!(
            s,
            "{}",
            match self {
                Self::Table => "graph_edge",
                Self::SourceId => "source_id",
                Self::TargetId => "target_id",
                Self::RelationshipName => "relationship_name",
                Self::Properties => "properties",
                Self::CreatedAt => "created_at",
                Self::UpdatedAt => "updated_at",
            }
        )
        .expect("write to string cannot fail");
    }
}

// ---------------------------------------------------------------------------
// Intermediate row types
// ---------------------------------------------------------------------------

/// Intermediate representation of a node row ready for INSERT.
struct NodeRow {
    id: String,
    name: String,
    node_type: String,
    properties: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// Migration version recorded by this adapter's migrator (see [`migrator`]).
///
/// Older builds tracked this in the default `seaql_migrations`; newer builds use
/// `seaql_migrations_pggraph`. The constant is used to purge the stale legacy row
/// during init — see [`cleanup_legacy_seaql_migrations`].
const GRAPH_MIGRATION_VERSION: &str = "m20250101_000001_create_graph_tables";

/// Remove this adapter's stale bookkeeping row from the *default*
/// `seaql_migrations` table that older builds may have left behind.
///
/// # Why
/// This adapter now tracks its migrations in `seaql_migrations_pggraph`. In an
/// "everything in one Postgres" deployment the core/relational migrator owns the
/// default `seaql_migrations`. If an older build had recorded
/// [`GRAPH_MIGRATION_VERSION`] there, the core migrator would treat it as a
/// foreign "applied but its file is missing" version and abort. We delete only
/// the version this adapter itself defines — never a core/relational version — so
/// the operation is safe and idempotent. Guarded by `to_regclass` so it is a
/// no-op on fresh installs where the default table does not (yet) exist.
///
/// # Residual
/// This only helps when this adapter initialises. If the core migrator runs
/// *first* against a DB that still holds the legacy row it aborts before this
/// cleanup can run; such a DB needs a one-time manual
/// `DELETE FROM seaql_migrations WHERE version = 'm20250101_000001_create_graph_tables'`.
async fn cleanup_legacy_seaql_migrations(db: &DatabaseConnection) -> GraphDBResult<()> {
    // `GRAPH_MIGRATION_VERSION` is a compile-time constant with no user input,
    // so inlining it into the DO block carries no injection risk.
    let sql = format!(
        "DO $$ BEGIN \
             IF to_regclass('seaql_migrations') IS NOT NULL THEN \
                 DELETE FROM seaql_migrations WHERE version = '{GRAPH_MIGRATION_VERSION}'; \
             END IF; \
         END $$;"
    );
    db.execute_unprepared(&sql).await.map_err(|e| {
        GraphDBError::InitializationError(format!("PgGraph legacy migration cleanup failed: {e}"))
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Graph database backed by PostgreSQL with two tables (`graph_node`, `graph_edge`).
///
/// Properties are stored as JSONB columns. Core fields (`id`, `name`, `type`) are
/// promoted to dedicated columns for indexing; everything else goes into `properties`.
pub struct PgGraphAdapter {
    db: DatabaseConnection,
    /// Whether this adapter opened `db` itself and may therefore close it.
    ///
    /// Load-bearing, not defensive: [`Self::from_connection`] wraps a connection
    /// the *caller* owns, and in the single-shared-Postgres layout that caller is
    /// the relational store. Closing it from a graph teardown would turn a leak
    /// fix into an outage — every relational query on the process would start
    /// failing with a closed pool. Neither in-tree factory takes that path today,
    /// but both constructors are public API.
    owns_pool: bool,
}

impl PgGraphAdapter {
    /// Connect to an existing PostgreSQL database and run graph-table migrations.
    ///
    /// The database must already exist. Use [`Self::from_connection`] to share
    /// a connection that was established elsewhere (e.g. by the database crate).
    pub async fn new(database_url: &str) -> GraphDBResult<Self> {
        let db = Database::connect(database_url)
            .await
            .map_err(|e| GraphDBError::ConnectionError(format!("PgGraph connect failed: {e}")))?;

        cleanup_legacy_seaql_migrations(&db).await?;
        migrator::Migrator::up(&db, None).await.map_err(|e| {
            GraphDBError::InitializationError(format!("PgGraph migration failed: {e}"))
        })?;

        debug!("PgGraphAdapter initialised");
        Ok(Self {
            db,
            owns_pool: true,
        })
    }

    /// Wrap an existing SeaORM `DatabaseConnection` (must be Postgres).
    ///
    /// Only the graph tables are created if missing (via migration).
    pub async fn from_connection(db: DatabaseConnection) -> GraphDBResult<Self> {
        cleanup_legacy_seaql_migrations(&db).await?;
        migrator::Migrator::up(&db, None).await.map_err(|e| {
            GraphDBError::InitializationError(format!("PgGraph migration failed: {e}"))
        })?;

        Ok(Self {
            db,
            owns_pool: false,
        })
    }

    /// Close this adapter's **own** Postgres pool, so its server-side backends go
    /// away now rather than whenever the last `Arc` happens to be dropped.
    ///
    /// This adapter opens a pool entirely separate from the relational one (see
    /// the pool-sizing note in `cognee_database::connection`), so a warm
    /// `ComponentManager` on Postgres holds three pools of ten and the relational
    /// close in topoteretes/cognee-rs#135 reached only one of them.
    ///
    /// Measured against a live `pgvector/pg16` with `max_connections = 100`, and
    /// worth stating precisely because the obvious framing of this bug is wrong:
    ///
    /// | teardown | backends afterwards |
    /// |---|---|
    /// | `drop`, pool idle | drained in **4 ms** — a drop is enough here |
    /// | `close`, pool idle | drained in **1 ms**, the call returning in 67 µs |
    /// | `drop`, one `pg_sleep` in flight | **all 10 pinned** for the query's full duration |
    /// | `close`, one `pg_sleep` in flight | **9 of 10 reclaimed** within 500 ms, query unaffected |
    /// | `Arc` retained, never closed | **10 open at 5 s**; `close()` on that same `Arc` drains them in 2 ms |
    ///
    /// So the two things this method buys are the last two rows:
    /// - **A retained `Arc` has no drop to wait for.** The HTTP server keeps
    ///   `lib.graph_db` as an `Arc` clone in `AppState` and an in-flight pipeline
    ///   holds its own, so there is no owner to drop; closing through `&self` is
    ///   the only teardown available, and without it the backends stay for the
    ///   lifetime of the process. Against `max_connections = 100`, a handful of
    ///   long-lived closed handles exhausts the server.
    /// - **Under contention a drop is far worse.** A checked-out `PoolConnection`
    ///   holds an `Arc<PoolInner>` (sqlx-core `pool/connection.rs`), so one slow
    ///   query keeps the whole pool alive; [`sea_orm::DatabaseConnection::close_by_ref`]
    ///   reclaims the idle connections immediately and lets the running query
    ///   finish normally.
    ///
    /// A third difference is read from sqlx's source rather than measured:
    /// `close_by_ref` sends the protocol `Terminate` message
    /// (sqlx-postgres `connection/mod.rs`), where the drop path just closes the
    /// socket — which is what makes a server log `unexpected EOF on client
    /// connection with an open transaction`. The behaviour behind a connection
    /// pooler (pgbouncer, RDS Proxy) follows from that, but has not been measured.
    ///
    /// A **no-op when the connection came from [`Self::from_connection`]**, and
    /// idempotent (a closed pool closes again harmlessly). Surviving clones of
    /// this adapter fail their next query against the closed pool rather than
    /// reconnecting.
    pub async fn close(&self) -> GraphDBResult<()> {
        if !self.owns_pool {
            debug!("PgGraphAdapter::close is a no-op for a caller-owned connection");
            return Ok(());
        }
        self.db
            .close_by_ref()
            .await
            .map_err(|e| GraphDBError::ConnectionError(format!("PgGraph pool close failed: {e}")))
    }

    /// The sea-orm connection (and therefore the pool) this adapter runs on.
    ///
    /// Exposed for diagnostics and for the teardown regression tests, which have
    /// to observe the pool's own `is_closed()` flag and check a connection out of
    /// *this* pool to exercise the in-flight case. Not an escape hatch for
    /// issuing graph queries — use the typed adapter methods for that.
    #[doc(hidden)]
    pub fn connection(&self) -> &DatabaseConnection {
        &self.db
    }
    // -- helpers -------------------------------------------------------------

    /// Build a SeaORM [`Statement`] from a `sea_query` query.
    fn build<S: sea_orm::StatementBuilder>(&self, query: &S) -> Statement {
        self.db.get_database_backend().build(query)
    }

    /// Extract core fields from a JSON value and build a [`NodeRow`].
    ///
    /// `created_at` / `updated_at` populate the dedicated `TIMESTAMPTZ` columns
    /// *and* stay in the JSONB `properties`, mirroring Python's `add_nodes`
    /// (`postgres/adapter.py:266-289`) whose `core_keys` is exactly
    /// `{"id", "name", "type"}`. The columns record when the row was written; the
    /// blob preserves the `DataPoint`'s own epoch-ms `created_at`, which is what
    /// [`GraphDBTrait::get_graph_data`] returns and the visualization turns into
    /// `t_created`.
    fn serialize_node_to_row(node: &Value) -> GraphDBResult<NodeRow> {
        let obj = node
            .as_object()
            .ok_or_else(|| GraphDBError::NodeError("Expected JSON object for node".into()))?;

        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let name = obj
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Accept both "type" and "data_type" (compat with Ladybug adapter).
        let node_type = obj
            .get("type")
            .or_else(|| obj.get("data_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let now = Utc::now();
        let created_at = parse_audit_timestamp(obj.get("created_at")).unwrap_or(now);
        let updated_at = parse_audit_timestamp(obj.get("updated_at")).unwrap_or(now);

        // Everything that isn't a core field goes into `properties`. `created_at`
        // and `updated_at` are deliberately *not* core keys — see the doc comment.
        let core_keys = ["id", "name", "type", "data_type"];
        let extra: serde_json::Map<String, Value> = obj
            .iter()
            .filter(|(k, _)| !core_keys.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // Strip NUL bytes before they reach `varchar` columns and the `jsonb`
        // blob. PDF-extracted chunk text routinely carries literal `0x00`, which
        // Postgres rejects outright — see `cognee_utils::sanitize`. This is the
        // single choke point for every node write on this adapter.
        //
        // `id` is sanitized here but *not* on the read and delete paths
        // (`get_node`, `has_node`, `get_nodes`, `delete_node`, `delete_nodes`),
        // so a node written under a NUL-bearing id is not retrievable by that
        // raw id. That asymmetry is deliberate: Python does exactly the same
        // thing — `postgres_demo/adapter.py:56` sanitizes the id on write while
        // every read there passes `node_id` through untouched — and diverging
        // would make the two SDKs disagree about whether such a lookup hits.
        // Node ids are UUIDs or normalized identifiers in practice, so the
        // corner is unreachable from the real pipeline.
        Ok(NodeRow {
            id: sanitize_string(id),
            name: sanitize_string(name),
            node_type: sanitize_string(node_type),
            properties: sanitize_json(Value::Object(extra)),
            created_at,
            updated_at,
        })
    }

    /// Convert a query result row (`id, name, type, properties`) into a [`NodeData`]
    /// HashMap, merging JSONB properties back into the top level.
    fn parse_node_row(row: &sea_orm::QueryResult) -> GraphDBResult<NodeData> {
        let id: String = row
            .try_get("", "id")
            .map_err(|e| GraphDBError::QueryError(format!("missing id column: {e}")))?;
        let name: String = row
            .try_get("", "name")
            .map_err(|e| GraphDBError::QueryError(format!("missing name column: {e}")))?;
        let node_type: String = row
            .try_get("", "type")
            .map_err(|e| GraphDBError::QueryError(format!("missing type column: {e}")))?;
        let properties: Option<Value> = row.try_get("", "properties").unwrap_or(None);

        let mut data = NodeData::new();
        data.insert(Cow::Borrowed("id"), json!(id));
        data.insert(Cow::Borrowed("name"), json!(name));
        data.insert(Cow::Borrowed("type"), json!(node_type));

        // Merge extra properties back into the top-level map.
        if let Some(Value::Object(extra)) = properties {
            for (k, v) in extra {
                data.insert(Cow::Owned(k), v);
            }
        }
        Ok(data)
    }

    /// Parse an edge row into [`EdgeData`].
    fn parse_edge_row(row: &sea_orm::QueryResult) -> GraphDBResult<EdgeData> {
        Self::parse_edge_row_cols(
            row,
            "source_id",
            "target_id",
            "relationship_name",
            "properties",
        )
    }

    /// Parse an edge row with custom column aliases.
    fn parse_edge_row_cols(
        row: &sea_orm::QueryResult,
        src_col: &str,
        tgt_col: &str,
        rel_col: &str,
        props_col: &str,
    ) -> GraphDBResult<EdgeData> {
        let source_id: String = row.try_get("", src_col).unwrap_or_default();
        let target_id: String = row.try_get("", tgt_col).unwrap_or_default();
        let rel_name: String = row.try_get("", rel_col).unwrap_or_default();
        let props: Option<Value> = row.try_get("", props_col).unwrap_or(None);
        let props_map = match props {
            Some(Value::Object(m)) => m.into_iter().map(|(k, v)| (Cow::Owned(k), v)).collect(),
            _ => HashMap::new(),
        };
        Ok((source_id, target_id, rel_name, props_map))
    }
}

// ---------------------------------------------------------------------------
// GraphDBTrait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl GraphDBTrait for PgGraphAdapter {
    async fn initialize(&self) -> GraphDBResult<()> {
        // Migration already ran in the constructor; calling again is idempotent.
        migrator::Migrator::up(&self.db, None).await.map_err(|e| {
            GraphDBError::InitializationError(format!("PgGraph migration failed: {e}"))
        })?;
        Ok(())
    }

    /// Delegates to the inherent [`PgGraphAdapter::close`], so a holder of an
    /// `Arc<dyn GraphDBTrait>` can release the pool without downcasting.
    async fn close(&self) -> GraphDBResult<()> {
        PgGraphAdapter::close(self).await
    }

    async fn is_empty(&self) -> GraphDBResult<bool> {
        let query = Query::select()
            .expr(Expr::val(1))
            .from(GNode::Table)
            .limit(1)
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        Ok(row.is_none())
    }

    async fn query(
        &self,
        _query: &str,
        _params: Option<HashMap<Cow<'static, str>, Value>>,
    ) -> GraphDBResult<Vec<Vec<Value>>> {
        Err(GraphDBError::QueryError(
            "The PostgreSQL graph backend does not support raw Cypher queries. \
             Use a graph-native backend (Ladybug, Neo4j) for raw query support, \
             or use the typed adapter methods (add_nodes, get_neighbors, etc.)."
                .into(),
        ))
    }

    async fn delete_graph(&self) -> GraphDBResult<()> {
        // TRUNCATE CASCADE is not expressible via sea_query.
        self.db
            .execute_unprepared("TRUNCATE graph_edge, graph_node CASCADE")
            .await
            .map_err(|e| GraphDBError::QueryError(format!("Failed to truncate graph: {e}")))?;
        Ok(())
    }

    // -- node operations (sea_query) -----------------------------------------

    async fn has_node(&self, node_id: &str) -> GraphDBResult<bool> {
        let inner = Query::select()
            .expr(Expr::val(1))
            .from(GNode::Table)
            .and_where(Expr::col(GNode::Id).eq(node_id))
            .to_owned();

        let query = Query::select()
            .expr_as(Expr::exists(inner), Alias::new("ex"))
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        match row {
            Some(r) => {
                let ex: bool = r
                    .try_get("", "ex")
                    .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
                Ok(ex)
            }
            None => Ok(false),
        }
    }

    async fn add_node_raw(&self, node: Value) -> GraphDBResult<()> {
        self.add_nodes_raw(vec![node]).await
    }

    async fn add_nodes_raw(&self, nodes: Vec<Value>) -> GraphDBResult<()> {
        if nodes.is_empty() {
            return Ok(());
        }

        // Serialize and deduplicate by id (last wins).
        let mut seen: HashMap<String, NodeRow> = HashMap::new();
        for node in &nodes {
            let row = Self::serialize_node_to_row(node)?;
            seen.insert(row.id.clone(), row);
        }
        let rows: Vec<NodeRow> = seen.into_values().collect();

        for chunk in rows.chunks(BATCH_SIZE) {
            let mut insert = Query::insert()
                .into_table(GNode::Table)
                .columns([
                    GNode::Id,
                    GNode::Name,
                    GNode::Type,
                    GNode::Properties,
                    GNode::CreatedAt,
                    GNode::UpdatedAt,
                ])
                .to_owned();

            for row in chunk {
                insert.values_panic([
                    row.id.clone().into(),
                    row.name.clone().into(),
                    row.node_type.clone().into(),
                    row.properties.clone().into(),
                    row.created_at.into(),
                    row.updated_at.into(),
                ]);
            }

            insert.on_conflict(
                OnConflict::column(GNode::Id)
                    .update_columns([GNode::Name, GNode::Type, GNode::Properties])
                    .value(GNode::UpdatedAt, Expr::current_timestamp())
                    .to_owned(),
            );

            self.db
                .execute(self.build(&insert))
                .await
                .map_err(|e| GraphDBError::NodeError(format!("Failed to upsert nodes: {e}")))?;
        }

        Ok(())
    }

    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
        let query = Query::delete()
            .from_table(GNode::Table)
            .and_where(Expr::col(GNode::Id).eq(node_id))
            .to_owned();

        self.db
            .execute(self.build(&query))
            .await
            .map_err(|e| GraphDBError::NodeError(format!("Failed to delete node: {e}")))?;
        Ok(())
    }

    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
        if node_ids.is_empty() {
            return Ok(());
        }

        let query = Query::delete()
            .from_table(GNode::Table)
            .and_where(Expr::col(GNode::Id).is_in(node_ids.iter().map(|s| s.as_str())))
            .to_owned();

        self.db
            .execute(self.build(&query))
            .await
            .map_err(|e| GraphDBError::NodeError(format!("Failed to delete nodes: {e}")))?;
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>> {
        let query = Query::select()
            .columns([GNode::Id, GNode::Name, GNode::Type, GNode::Properties])
            .from(GNode::Table)
            .and_where(Expr::col(GNode::Id).eq(node_id))
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(Self::parse_node_row(&r)?)),
            None => Ok(None),
        }
    }

    async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
        if node_ids.is_empty() {
            return Ok(vec![]);
        }

        let query = Query::select()
            .columns([GNode::Id, GNode::Name, GNode::Type, GNode::Properties])
            .from(GNode::Table)
            .and_where(Expr::col(GNode::Id).is_in(node_ids.iter().map(|s| s.as_str())))
            .to_owned();

        let rows = self
            .db
            .query_all(self.build(&query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        rows.iter().map(Self::parse_node_row).collect()
    }

    async fn get_node_truth_state(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<HashMap<String, crate::NodeTruthState>> {
        let nodes = self.get_nodes(node_ids).await?;
        let mut out = HashMap::with_capacity(nodes.len());
        for node in nodes {
            let Some(id) = node.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            out.insert(
                id.to_string(),
                crate::NodeTruthState {
                    truth_alignment: crate::traits::extract_truth_alignment(
                        node.get("truth_alignment"),
                    ),
                    truth_epoch: crate::traits::extract_truth_epoch(node.get("truth_epoch")),
                },
            );
        }
        Ok(out)
    }

    async fn set_node_truth_state(
        &self,
        updates: &HashMap<String, crate::NodeTruthState>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        // Fetch current full property maps in one round-trip, merge the two
        // truth-state fields into each, then upsert via `add_nodes_raw`
        // (`INSERT ... ON CONFLICT DO UPDATE`), which never touches edges — so
        // unlike the default's per-property delete+re-add it cannot drop edges.
        let ids: Vec<String> = updates.keys().cloned().collect();
        let nodes = self.get_nodes(&ids).await?;

        // Default every requested id to `false`; flip to `true` when the node
        // was actually present in the fetch (and thus included in the upsert).
        let mut out: HashMap<String, bool> = ids.iter().map(|id| (id.clone(), false)).collect();

        let mut merged: Vec<Value> = Vec::with_capacity(nodes.len());
        for node in nodes {
            let Some(id) = node.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
                continue;
            };
            let Some(state) = updates.get(&id) else {
                continue;
            };
            let mut obj = serde_json::Map::new();
            for (k, v) in node {
                obj.insert(k.into_owned(), v);
            }
            obj.insert("truth_alignment".to_string(), json!(state.truth_alignment));
            obj.insert("truth_epoch".to_string(), json!(state.truth_epoch));
            merged.push(Value::Object(obj));
            out.insert(id, true);
        }

        if !merged.is_empty() {
            self.add_nodes_raw(merged).await?;
        }
        Ok(out)
    }

    /// In-place property update, replacing the base trait's delete-and-re-add
    /// default.
    ///
    /// The default `GraphDBTrait::update_node_property` fetches the node, saves
    /// its edges, `delete_node`s it (cascading the edges away), re-adds the
    /// node and then restores the edges — five statements with no enclosing
    /// transaction. An interruption between the delete and the re-add loses the
    /// node *and* its edges, and the edge read is `unwrap_or_default()`, so a
    /// failed read silently drops every edge on the recreate. This override
    /// merges the property into the fetched map and upserts through
    /// `add_nodes_raw` (`INSERT ... ON CONFLICT DO UPDATE`), which never touches
    /// the edge table — the approach `set_node_truth_state` above already uses.
    ///
    /// Going through `add_nodes_raw` rather than a raw `jsonb ||` update keeps
    /// `serialize_node_to_row`'s split between the core `id`/`name`/`type`
    /// columns and the `properties` JSONB, so `key` lands exactly where
    /// `parse_node_row` reads it back from.
    async fn update_node_property(
        &self,
        node_id: &str,
        key: &str,
        value: Value,
    ) -> GraphDBResult<()> {
        // The id is passed through raw, matching every other read/delete path
        // on this adapter — see the deliberate write-only-sanitization note in
        // `serialize_node_to_row`. Sanitizing here would make a NUL-bearing id
        // hit on Postgres while still missing on ladybug and in Python.
        let ids = [node_id.to_string()];
        let node = self
            .get_nodes(&ids)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| GraphDBError::NodeError(format!("Node not found: {node_id}")))?;

        let mut obj = serde_json::Map::new();
        for (k, v) in node {
            obj.insert(k.into_owned(), v);
        }
        obj.insert(key.to_string(), value);

        self.add_nodes_raw(vec![Value::Object(obj)]).await
    }

    /// One round-trip for the whole batch, versus the default's `get_node` per
    /// id.
    ///
    /// Matches the default's contract: only ids that exist *and* carry a
    /// numeric `feedback_weight` appear in the result.
    async fn get_node_feedback_weights(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<HashMap<String, f64>> {
        if node_ids.is_empty() {
            return Ok(HashMap::new());
        }
        // Ids pass through raw (see `serialize_node_to_row`), and each row is
        // keyed by its own `id` rather than by position — `get_nodes` answers
        // in storage order.
        let nodes = self.get_nodes(node_ids).await?;
        let mut out = HashMap::with_capacity(nodes.len());
        for node in nodes {
            let Some(id) = node.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
                continue;
            };
            if let Some(w) = node.get("feedback_weight").and_then(Value::as_f64) {
                out.insert(id, w);
            }
        }
        Ok(out)
    }

    /// One fetch plus one upsert for the whole batch, versus the default's
    /// `update_node_property` (and therefore full delete+re-add) per id.
    ///
    /// Mirrors `set_node_truth_state`: every requested id defaults to `false`
    /// and flips to `true` only if the node was actually present in the fetch
    /// and thus included in the upsert.
    async fn set_node_feedback_weights(
        &self,
        updates: &HashMap<String, f64>,
    ) -> GraphDBResult<HashMap<String, bool>> {
        if updates.is_empty() {
            return Ok(HashMap::new());
        }
        // Ids pass through raw (see `serialize_node_to_row`).
        let ids: Vec<String> = updates.keys().cloned().collect();
        let nodes = self.get_nodes(&ids).await?;

        let mut out: HashMap<String, bool> = ids.iter().map(|id| (id.clone(), false)).collect();

        let mut merged: Vec<Value> = Vec::with_capacity(nodes.len());
        for node in nodes {
            let Some(id) = node.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
                continue;
            };
            let Some(weight) = updates.get(&id) else {
                continue;
            };
            let mut obj = serde_json::Map::new();
            for (k, v) in node {
                obj.insert(k.into_owned(), v);
            }
            obj.insert("feedback_weight".to_string(), json!(weight));
            merged.push(Value::Object(obj));
            out.insert(id, true);
        }

        if !merged.is_empty() {
            self.add_nodes_raw(merged).await?;
        }
        Ok(out)
    }

    // -- edge operations (sea_query) -----------------------------------------

    async fn has_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
    ) -> GraphDBResult<bool> {
        let inner = Query::select()
            .expr(Expr::val(1))
            .from(GEdge::Table)
            .and_where(Expr::col(GEdge::SourceId).eq(source_id))
            .and_where(Expr::col(GEdge::TargetId).eq(target_id))
            .and_where(Expr::col(GEdge::RelationshipName).eq(relationship_name))
            .to_owned();

        let query = Query::select()
            .expr_as(Expr::exists(inner), Alias::new("ex"))
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        match row {
            Some(r) => {
                let ex: bool = r
                    .try_get("", "ex")
                    .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
                Ok(ex)
            }
            None => Ok(false),
        }
    }

    async fn has_edges(&self, edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
        if edges.is_empty() {
            return Ok(vec![]);
        }

        // Single round-trip regardless of batch size: pass the candidate
        // (source, target, relationship) triples as three `text[]` arrays and let
        // Postgres check existence for all of them at once via `unnest(...)` + `EXISTS`.
        // This replaces the previous one-round-trip-per-edge loop.
        let sources: Vec<_> = edges.iter().map(|e| e.0.clone()).collect();
        let targets: Vec<_> = edges.iter().map(|e| e.1.clone()).collect();
        let rels: Vec<_> = edges.iter().map(|e| e.2.clone()).collect();

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT v.s, v.t, v.r \
                 FROM unnest($1::text[], $2::text[], $3::text[]) AS v(s, t, r) \
                 WHERE EXISTS ( \
                     SELECT 1 FROM graph_edge e \
                     WHERE e.source_id = v.s \
                       AND e.target_id = v.t \
                       AND e.relationship_name = v.r \
                 )",
                [sources.into(), targets.into(), rels.into()],
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        // Collect the triples that exist, then filter the original input so each
        // returned edge keeps its properties (which aren't part of the lookup key).
        // A decode failure is a real error, so propagate it rather than silently
        // dropping the row.
        let mut existing: HashSet<_> = HashSet::with_capacity(rows.len());
        for row in &rows {
            let s: String = row
                .try_get("", "s")
                .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
            let t: String = row
                .try_get("", "t")
                .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
            let r: String = row
                .try_get("", "r")
                .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
            existing.insert((s, t, r));
        }

        let found = edges
            .iter()
            .filter(|e| existing.contains(&(e.0.clone(), e.1.clone(), e.2.clone())))
            .cloned()
            .collect();

        Ok(found)
    }

    /// In-place edge-property update, replacing the base trait's no-op default
    /// (which only logs a warning and reports success).
    ///
    /// Edge properties live wholly in the `properties` JSONB column — unlike
    /// nodes, there is no core-column split to preserve — so a single
    /// `jsonb ||` merge is both correct and atomic. Merging (rather than
    /// overwriting) keeps every other property on the edge intact.
    ///
    /// Reports `EdgeError` when no edge matches, so a caller checking `is_ok()`
    /// is not told an update to a nonexistent edge succeeded.
    async fn update_edge_property(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        key: &str,
        value: Value,
    ) -> GraphDBResult<()> {
        // The property value is sanitized, as `add_edges` does for the same
        // column: Postgres rejects a NUL in a `jsonb` cast, so an unsanitized
        // patch built from LLM- or PDF-derived text errors at runtime.
        //
        // The three key columns are *not* sanitized, matching every other
        // read/delete path on this adapter — see the write-only-sanitization
        // note in `serialize_node_to_row`. That keeps Postgres, ladybug and
        // Python agreeing on whether a NUL-bearing key hits.
        let patch = sanitize_json(json!({ key: value }));
        let result = self
            .db
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE graph_edge \
                 SET properties = COALESCE(properties, '{}'::jsonb) || $4::jsonb, \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE source_id = $1 AND target_id = $2 AND relationship_name = $3",
                [
                    source_id.into(),
                    target_id.into(),
                    relationship_name.into(),
                    patch.to_string().into(),
                ],
            ))
            .await
            .map_err(|e| GraphDBError::EdgeError(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(GraphDBError::EdgeError(format!(
                "Edge not found: {source_id} -> {target_id} ({relationship_name})"
            )));
        }
        Ok(())
    }

    /// One round-trip for the whole batch, replacing the base trait's default
    /// (which returns an empty map and warns, because the generic trait has no
    /// per-edge property read).
    ///
    /// Matches the node-side contract: only edges that exist *and* carry a
    /// numeric `feedback_weight` appear in the result.
    async fn get_edge_feedback_weights(
        &self,
        edge_keys: &[crate::traits::EdgeKey],
    ) -> GraphDBResult<HashMap<crate::traits::EdgeKey, f64>> {
        if edge_keys.is_empty() {
            return Ok(HashMap::new());
        }

        // Keys pass through raw, as on every other lookup path here.
        let sources: Vec<String> = edge_keys.iter().map(|k| k.0.clone()).collect();
        let targets: Vec<String> = edge_keys.iter().map(|k| k.1.clone()).collect();
        let rels: Vec<String> = edge_keys.iter().map(|k| k.2.clone()).collect();

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT e.source_id AS s, e.target_id AS t, e.relationship_name AS r, \
                        e.properties -> 'feedback_weight' AS w \
                 FROM graph_edge e \
                 JOIN unnest($1::text[], $2::text[], $3::text[]) AS v(s, t, r) \
                   ON e.source_id = v.s AND e.target_id = v.t \
                  AND e.relationship_name = v.r",
                [sources.into(), targets.into(), rels.into()],
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut out = HashMap::with_capacity(rows.len());
        for row in &rows {
            let s: String = row
                .try_get("", "s")
                .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
            let t: String = row
                .try_get("", "t")
                .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
            let r: String = row
                .try_get("", "r")
                .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
            // A NULL / absent property decodes to `None`; a non-numeric one
            // fails `as_f64`. Both are "no weight", matching the node side.
            let w: Option<Value> = row.try_get("", "w").unwrap_or(None);
            if let Some(weight) = w.as_ref().and_then(Value::as_f64) {
                out.insert((s, t, r), weight);
            }
        }
        Ok(out)
    }

    /// One round-trip for the whole batch, replacing the base trait's default
    /// (one `update_edge_property` per edge, which on this backend used to be
    /// the warning-only no-op and so reported success without writing).
    ///
    /// Weights travel as `text[]` and are cast to `float8` in SQL — the same
    /// array-parameter shape `has_edges` uses — then merged into each edge's
    /// JSONB. `RETURNING` reports which edges actually matched, so the success
    /// map reflects real writes; every requested key defaults to `false`.
    async fn set_edge_feedback_weights(
        &self,
        updates: &HashMap<crate::traits::EdgeKey, f64>,
    ) -> GraphDBResult<HashMap<crate::traits::EdgeKey, bool>> {
        if updates.is_empty() {
            return Ok(HashMap::new());
        }

        let mut sources: Vec<String> = Vec::with_capacity(updates.len());
        let mut targets: Vec<String> = Vec::with_capacity(updates.len());
        let mut rels: Vec<String> = Vec::with_capacity(updates.len());
        let mut weights: Vec<String> = Vec::with_capacity(updates.len());
        let mut out: HashMap<crate::traits::EdgeKey, bool> = HashMap::with_capacity(updates.len());

        // Keys pass through raw, as on every other lookup path here.
        for (key, weight) in updates {
            out.insert(key.clone(), false);
            // JSON has no infinity or NaN. Postgres does not reject them here
            // — `jsonb_build_object('feedback_weight', 'inf'::float8)` yields
            // the *string* `"Infinity"` — so an unguarded non-finite weight
            // would quietly store a value that `get_edge_feedback_weights`
            // then drops on `as_f64`, leaving a junk property behind and
            // reporting success for a weight nobody can read. Skipping the key
            // keeps it out of the blob and reports `false` honestly.
            if !weight.is_finite() {
                continue;
            }
            sources.push(key.0.clone());
            targets.push(key.1.clone());
            rels.push(key.2.clone());
            // `{:?}` is f64's shortest round-trip representation, so the
            // float8 cast parses back to exactly this value.
            weights.push(format!("{weight:?}"));
        }

        if sources.is_empty() {
            return Ok(out);
        }

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE graph_edge e \
                 SET properties = COALESCE(e.properties, '{}'::jsonb) \
                     || jsonb_build_object('feedback_weight', v.w::float8), \
                     updated_at = CURRENT_TIMESTAMP \
                 FROM unnest($1::text[], $2::text[], $3::text[], $4::text[]) \
                      AS v(s, t, r, w) \
                 WHERE e.source_id = v.s AND e.target_id = v.t \
                   AND e.relationship_name = v.r \
                 RETURNING e.source_id AS s, e.target_id AS t, \
                           e.relationship_name AS r",
                [sources.into(), targets.into(), rels.into(), weights.into()],
            ))
            .await
            .map_err(|e| GraphDBError::EdgeError(e.to_string()))?;

        for row in &rows {
            let s: String = row
                .try_get("", "s")
                .map_err(|e| GraphDBError::EdgeError(e.to_string()))?;
            let t: String = row
                .try_get("", "t")
                .map_err(|e| GraphDBError::EdgeError(e.to_string()))?;
            let r: String = row
                .try_get("", "r")
                .map_err(|e| GraphDBError::EdgeError(e.to_string()))?;
            out.insert((s, t, r), true);
        }
        Ok(out)
    }

    async fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relationship_name: &str,
        properties: Option<HashMap<Cow<'static, str>, Value>>,
    ) -> GraphDBResult<()> {
        let props = properties.unwrap_or_default();
        let edge: EdgeData = (
            source_id.to_string(),
            target_id.to_string(),
            relationship_name.to_string(),
            props,
        );
        self.add_edges(&[edge]).await
    }

    async fn add_edges(&self, edges: &[EdgeData]) -> GraphDBResult<()> {
        if edges.is_empty() {
            return Ok(());
        }

        let now = Utc::now();

        // Sanitize *before* deduplicating, not after. The `ON CONFLICT` target
        // below is the sanitized triple, so two edges differing only by an
        // embedded NUL must collapse into one entry here. Keying the dedup map
        // on the raw triple would let both survive into a single
        // `INSERT ... ON CONFLICT DO UPDATE`, which Postgres rejects with
        // `21000: ON CONFLICT DO UPDATE command cannot affect row a second
        // time` — aborting the whole chunk. `add_nodes_raw` gets this right by
        // construction because it dedups on the already-sanitized `row.id`.
        let mut seen: HashMap<(String, String, String), Value> = HashMap::new();
        for edge in edges {
            let props_json =
                serde_json::to_value(&edge.3).map_err(GraphDBError::SerializationError)?;
            let key = (
                sanitize_str(&edge.0).into_owned(),
                sanitize_str(&edge.1).into_owned(),
                sanitize_str(&edge.2).into_owned(),
            );
            // Edge properties carry LLM-authored descriptions and, through
            // them, source text — same NUL exposure as node properties.
            seen.insert(key, sanitize_json(props_json));
        }
        let deduped: Vec<((String, String, String), Value)> = seen.into_iter().collect();

        for chunk in deduped.chunks(BATCH_SIZE) {
            let mut insert = Query::insert()
                .into_table(GEdge::Table)
                .columns([
                    GEdge::SourceId,
                    GEdge::TargetId,
                    GEdge::RelationshipName,
                    GEdge::Properties,
                    GEdge::CreatedAt,
                    GEdge::UpdatedAt,
                ])
                .to_owned();

            for ((source, target, relationship_name), props_json) in chunk {
                insert.values_panic([
                    source.clone().into(),
                    target.clone().into(),
                    relationship_name.clone().into(),
                    props_json.clone().into(),
                    now.into(),
                    now.into(),
                ]);
            }

            insert.on_conflict(
                OnConflict::columns([GEdge::SourceId, GEdge::TargetId, GEdge::RelationshipName])
                    .update_column(GEdge::Properties)
                    .value(GEdge::UpdatedAt, Expr::current_timestamp())
                    .to_owned(),
            );

            self.db
                .execute(self.build(&insert))
                .await
                .map_err(|e| GraphDBError::EdgeError(format!("Failed to upsert edges: {e}")))?;
        }
        Ok(())
    }

    async fn get_edges(&self, node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
        let query = Query::select()
            .columns([
                GEdge::SourceId,
                GEdge::TargetId,
                GEdge::RelationshipName,
                GEdge::Properties,
            ])
            .from(GEdge::Table)
            .cond_where(
                Cond::any()
                    .add(Expr::col(GEdge::SourceId).eq(node_id))
                    .add(Expr::col(GEdge::TargetId).eq(node_id)),
            )
            .to_owned();

        let rows = self
            .db
            .query_all(self.build(&query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        rows.iter().map(Self::parse_edge_row).collect()
    }

    // -- graph query operations ----------------------------------------------
    //
    // The methods below use raw SQL because they involve recursive CTEs,
    // UNION ALL, JOINs with CASE expressions, or dynamic CTE construction
    // that sea_query's builder cannot express.

    async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT DISTINCT m.id, m.name, m.type, m.properties \
                 FROM graph_edge e \
                 JOIN graph_node m ON m.id = CASE \
                     WHEN e.source_id = $1 THEN e.target_id \
                     ELSE e.source_id \
                 END \
                 WHERE e.source_id = $1 OR e.target_id = $1",
                [node_id.into()],
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        rows.iter().map(Self::parse_node_row).collect()
    }

    async fn get_connections(
        &self,
        node_id: &str,
    ) -> GraphDBResult<Vec<(NodeData, HashMap<Cow<'static, str>, Value>, NodeData)>> {
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT \
                     n.id AS src_id, n.name AS src_name, n.type AS src_type, n.properties AS src_props, \
                     e.relationship_name, e.properties AS edge_props, \
                     m.id AS tgt_id, m.name AS tgt_name, m.type AS tgt_type, m.properties AS tgt_props \
                 FROM graph_edge e \
                 JOIN graph_node n ON n.id = e.source_id \
                 JOIN graph_node m ON m.id = e.target_id \
                 WHERE e.source_id = $1 OR e.target_id = $1",
                [node_id.into()],
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut connections = Vec::new();
        for row in &rows {
            // Source node
            let mut source = NodeData::new();
            let src_id: String = row.try_get("", "src_id").unwrap_or_default();
            let src_name: String = row.try_get("", "src_name").unwrap_or_default();
            let src_type: String = row.try_get("", "src_type").unwrap_or_default();
            let src_props: Option<Value> = row.try_get("", "src_props").unwrap_or(None);
            source.insert(Cow::Borrowed("id"), json!(src_id));
            source.insert(Cow::Borrowed("name"), json!(src_name));
            source.insert(Cow::Borrowed("type"), json!(src_type));
            if let Some(Value::Object(extra)) = src_props {
                for (k, v) in extra {
                    source.insert(Cow::Owned(k), v);
                }
            }

            // Edge properties
            let mut edge_props_map: HashMap<Cow<'static, str>, Value> = HashMap::new();
            let rel_name: String = row.try_get("", "relationship_name").unwrap_or_default();
            edge_props_map.insert(Cow::Borrowed("relationship_name"), json!(rel_name));
            let edge_props_raw: Option<Value> = row.try_get("", "edge_props").unwrap_or(None);
            if let Some(Value::Object(extra)) = edge_props_raw {
                for (k, v) in extra {
                    edge_props_map.insert(Cow::Owned(k), v);
                }
            }

            // Target node
            let mut target = NodeData::new();
            let tgt_id: String = row.try_get("", "tgt_id").unwrap_or_default();
            let tgt_name: String = row.try_get("", "tgt_name").unwrap_or_default();
            let tgt_type: String = row.try_get("", "tgt_type").unwrap_or_default();
            let tgt_props: Option<Value> = row.try_get("", "tgt_props").unwrap_or(None);
            target.insert(Cow::Borrowed("id"), json!(tgt_id));
            target.insert(Cow::Borrowed("name"), json!(tgt_name));
            target.insert(Cow::Borrowed("type"), json!(tgt_type));
            if let Some(Value::Object(extra)) = tgt_props {
                for (k, v) in extra {
                    target.insert(Cow::Owned(k), v);
                }
            }

            connections.push((source, edge_props_map, target));
        }
        Ok(connections)
    }

    async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        // Nodes
        let node_query = Query::select()
            .columns([GNode::Id, GNode::Name, GNode::Type, GNode::Properties])
            .from(GNode::Table)
            .to_owned();

        let node_rows = self
            .db
            .query_all(self.build(&node_query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut nodes = Vec::new();
        for row in &node_rows {
            let data = Self::parse_node_row(row)?;
            let id = data
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            nodes.push((id, data));
        }

        // Edges
        let edge_query = Query::select()
            .columns([
                GEdge::SourceId,
                GEdge::TargetId,
                GEdge::RelationshipName,
                GEdge::Properties,
            ])
            .from(GEdge::Table)
            .to_owned();

        let edge_rows = self
            .db
            .query_all(self.build(&edge_query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut edges = Vec::new();
        for row in &edge_rows {
            edges.push(Self::parse_edge_row(row)?);
        }

        Ok((nodes, edges))
    }

    async fn get_graph_metrics(
        &self,
        include_optional: bool,
    ) -> GraphDBResult<HashMap<Cow<'static, str>, Value>> {
        let mut metrics = HashMap::new();

        // Node count
        let n_row = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS cnt FROM graph_node".to_string(),
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
        let num_nodes: i64 = n_row
            .as_ref()
            .and_then(|r| r.try_get("", "cnt").ok())
            .unwrap_or(0);

        // Edge count
        let e_row = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT count(*) AS cnt FROM graph_edge".to_string(),
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
        let num_edges: i64 = e_row
            .as_ref()
            .and_then(|r| r.try_get("", "cnt").ok())
            .unwrap_or(0);

        metrics.insert(Cow::Borrowed("node_count"), json!(num_nodes));
        metrics.insert(Cow::Borrowed("edge_count"), json!(num_edges));

        let mean_degree = if num_nodes > 0 {
            (2.0 * num_edges as f64) / num_nodes as f64
        } else {
            0.0
        };
        let edge_density = if num_nodes > 1 {
            num_edges as f64 / (num_nodes as f64 * (num_nodes as f64 - 1.0))
        } else {
            0.0
        };

        metrics.insert(Cow::Borrowed("mean_degree"), json!(mean_degree));
        metrics.insert(Cow::Borrowed("edge_density"), json!(edge_density));

        // Connected components via recursive CTE (raw SQL — not expressible in sea_query)
        let comp_rows = self
            .db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "WITH RECURSIVE component AS ( \
                     SELECT id AS node_id, id AS comp_root FROM graph_node \
                     UNION \
                     SELECT CASE WHEN e.source_id = c.node_id THEN e.target_id ELSE e.source_id END, \
                            c.comp_root \
                     FROM component c \
                     JOIN graph_edge e ON e.source_id = c.node_id OR e.target_id = c.node_id \
                 ), \
                 node_comp AS ( \
                     SELECT node_id, MIN(comp_root) AS comp_id FROM component GROUP BY node_id \
                 ) \
                 SELECT comp_id, count(*) AS sz FROM node_comp GROUP BY comp_id ORDER BY sz DESC"
                    .to_string(),
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let component_sizes: Vec<Value> = comp_rows
            .iter()
            .filter_map(|r| {
                let sz: i64 = r.try_get("", "sz").ok()?;
                Some(json!(sz))
            })
            .collect();
        let num_components = component_sizes.len();

        metrics.insert(
            Cow::Borrowed("num_connected_components"),
            json!(num_components),
        );
        metrics.insert(
            Cow::Borrowed("sizes_of_connected_components"),
            Value::Array(component_sizes),
        );

        if include_optional {
            let sl_row = self
                .db
                .query_one(Statement::from_string(
                    DatabaseBackend::Postgres,
                    "SELECT count(*) AS cnt FROM graph_edge WHERE source_id = target_id"
                        .to_string(),
                ))
                .await
                .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
            let num_selfloops: i64 = sl_row
                .as_ref()
                .and_then(|r| r.try_get("", "cnt").ok())
                .unwrap_or(0);
            metrics.insert(Cow::Borrowed("num_selfloops"), json!(num_selfloops));
        }

        Ok(metrics)
    }

    async fn get_filtered_graph_data(
        &self,
        attribute_filters: &HashMap<Cow<'static, str>, Vec<Value>>,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if attribute_filters.is_empty() {
            return self.get_graph_data().await;
        }

        // Build WHERE clause — only allow whitelisted attributes to prevent SQL injection.
        // Raw SQL is used here because the CTE + UNION ALL pattern is not expressible
        // via sea_query.
        let mut where_parts = Vec::new();
        let mut values: Vec<sea_orm::Value> = Vec::new();
        let mut param_idx = 1u32;

        for (attr, filter_values) in attribute_filters {
            if filter_values.is_empty() {
                continue;
            }
            if !ALLOWED_FILTER_ATTRS.contains(&attr.as_ref()) {
                return Err(GraphDBError::QueryError(format!(
                    "Invalid filter attribute: {attr:?}. Allowed: {ALLOWED_FILTER_ATTRS:?}"
                )));
            }
            let placeholders: Vec<String> = filter_values
                .iter()
                .map(|v| {
                    let ph = format!("${param_idx}");
                    param_idx += 1;
                    let s = v
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| v.to_string());
                    values.push(s.into());
                    ph
                })
                .collect();
            where_parts.push(format!("n.{attr} IN ({})", placeholders.join(", ")));
        }

        if where_parts.is_empty() {
            return self.get_graph_data().await;
        }

        let where_clause = where_parts.join(" AND ");
        let sql = format!(
            "WITH filtered_nodes AS ( \
                 SELECT id, name, type, properties FROM graph_node n WHERE {where_clause} \
             ) \
             SELECT 'node' AS kind, fn.id, fn.name, fn.type, fn.properties, \
                    NULL::text AS source_id, NULL::text AS target_id, \
                    NULL::text AS relationship_name, NULL::jsonb AS edge_props \
             FROM filtered_nodes fn \
             UNION ALL \
             SELECT 'edge', NULL, NULL, NULL, NULL, \
                    e.source_id, e.target_id, e.relationship_name, e.properties \
             FROM graph_edge e \
             WHERE e.source_id IN (SELECT id FROM filtered_nodes) \
               AND e.target_id IN (SELECT id FROM filtered_nodes)"
        );

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for row in &rows {
            let kind: String = row.try_get("", "kind").unwrap_or_default();
            if kind == "node" {
                let data = Self::parse_node_row(row)?;
                let id = data
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                nodes.push((id, data));
            } else {
                edges.push(Self::parse_edge_row_cols(
                    row,
                    "source_id",
                    "target_id",
                    "relationship_name",
                    "edge_props",
                )?);
            }
        }

        Ok((nodes, edges))
    }

    /// Narrow the node scan to rows that could carry `needle` as a type-ish
    /// label, and read no edge rows at all.
    ///
    /// The SQL predicate is a deliberate **superset** of
    /// [`node_label_contains`](crate::node_label_contains), but a *narrow* one:
    /// it tests exactly the [`NODE_LABEL_KEYS`](crate::NODE_LABEL_KEYS), never
    /// the whole serialised blob.
    ///
    /// Matching `properties::text` wholesale would also be a superset, and it
    /// was the first version of this, but it is a bad one: the needles callers
    /// pass are ordinary English words. `"rule"` matches every `DocumentChunk`
    /// whose `text` property mentions "rule" or "rules", so on a large corpus
    /// the query drags thousands of full-text rows over the wire for
    /// `is_rule_node` to throw away — the opposite of the point. Testing the
    /// five label keys is orders of magnitude tighter and still cannot lose a
    /// row that satisfies the exact predicate.
    ///
    /// Both `type` the column and `type` the JSON key are tested because
    /// [`Self::parse_node_row`] merges `properties` *over* the columns, so a
    /// writer that put `type` in the blob is what the Rust predicate will see.
    /// For the other four keys the blob is the only home. `->>` renders an
    /// array-valued `labels` as its serialised text, which contains each
    /// element verbatim — jsonb escapes only quotes, backslashes and control
    /// characters, none of which appear in a label — so the array case survives
    /// too. Postgres' `lower()` is Unicode-aware where Rust's
    /// `to_ascii_lowercase` is not, which again only widens the match.
    ///
    /// This is still a sequential scan — there is no index on the extracted
    /// label keys — but it returns a handful of rows instead of the whole
    /// `graph_node` table plus the whole `graph_edge` table.
    async fn get_candidate_nodes_by_label(&self, needle: &str) -> GraphDBResult<Vec<GraphNode>> {
        // Built from `NODE_LABEL_KEYS` rather than spelled out, so the
        // pushed-down predicate cannot drift from the Rust one. These are
        // compile-time constants from this crate, not caller input, so the
        // interpolation needs no `ALLOWED_FILTER_ATTRS`-style whitelist — and
        // they land in a jsonb key literal, not an identifier position.
        let mut clauses = vec!["position($1 IN lower(type)) > 0".to_string()];
        clauses.extend(
            crate::NODE_LABEL_KEYS.iter().map(|key| {
                format!("position($1 IN lower(coalesce(properties->>'{key}', ''))) > 0")
            }),
        );
        let sql = format!(
            "SELECT id, name, type, properties FROM graph_node WHERE {}",
            clauses.join(" OR ")
        );

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                [sea_orm::Value::from(needle.to_lowercase())],
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut nodes = Vec::with_capacity(rows.len());
        for row in &rows {
            let data = Self::parse_node_row(row)?;
            let id = data
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            nodes.push((id, data));
        }
        Ok(nodes)
    }

    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
        node_name_filter_operator: &str,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if node_names.is_empty() {
            return Ok((vec![], vec![]));
        }

        // Raw SQL — complex CTE with dynamic neighbor logic (OR vs AND).
        let name_placeholders: Vec<String> = (0..node_names.len())
            .map(|i| format!("${}", i + 2))
            .collect();
        let names_in = name_placeholders.join(", ");

        let neighbor_cte = if node_name_filter_operator == "OR" {
            "neighbor_ids AS ( \
                 SELECT DISTINCT CASE \
                     WHEN e.source_id IN (SELECT id FROM primary_nodes) \
                     THEN e.target_id ELSE e.source_id \
                 END AS id \
                 FROM graph_edge e \
                 WHERE e.source_id IN (SELECT id FROM primary_nodes) \
                    OR e.target_id IN (SELECT id FROM primary_nodes) \
             )"
            .to_string()
        } else {
            // AND: neighbor must be connected to every primary node.
            let primary_count_param = format!("${}", node_names.len() + 2);
            format!(
                "neighbor_ids AS ( \
                     SELECT nbr_id AS id FROM ( \
                         SELECT CASE \
                             WHEN e.source_id IN (SELECT id FROM primary_nodes) \
                             THEN e.target_id ELSE e.source_id \
                         END AS nbr_id, \
                         CASE \
                             WHEN e.source_id IN (SELECT id FROM primary_nodes) \
                             THEN e.source_id ELSE e.target_id \
                         END AS primary_id \
                         FROM graph_edge e \
                         WHERE e.source_id IN (SELECT id FROM primary_nodes) \
                            OR e.target_id IN (SELECT id FROM primary_nodes) \
                     ) sub \
                     GROUP BY nbr_id \
                     HAVING COUNT(DISTINCT primary_id) = {primary_count_param} \
                 )"
            )
        };

        let sql = format!(
            "WITH primary_nodes AS ( \
                 SELECT DISTINCT id FROM graph_node WHERE type = $1 AND name IN ({names_in}) \
             ), \
             {neighbor_cte}, \
             all_ids AS ( \
                 SELECT id FROM primary_nodes UNION SELECT id FROM neighbor_ids \
             ) \
             SELECT 'node' AS kind, n.id, n.name, n.type, n.properties, \
                    NULL::text AS source_id, NULL::text AS target_id, \
                    NULL::text AS relationship_name, NULL::jsonb AS edge_props \
             FROM graph_node n WHERE n.id IN (SELECT id FROM all_ids) \
             UNION ALL \
             SELECT 'edge', NULL, NULL, NULL, NULL, \
                    e.source_id, e.target_id, e.relationship_name, e.properties \
             FROM graph_edge e \
             WHERE e.source_id IN (SELECT id FROM all_ids) \
               AND e.target_id IN (SELECT id FROM all_ids)"
        );

        let mut values: Vec<sea_orm::Value> = Vec::new();
        values.push(node_type.into());
        for name in node_names {
            values.push(name.clone().into());
        }
        if node_name_filter_operator != "OR" {
            values.push((node_names.len() as i64).into());
        }

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for row in &rows {
            let kind: String = row.try_get("", "kind").unwrap_or_default();
            if kind == "node" {
                let data = Self::parse_node_row(row)?;
                let id = data
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                nodes.push((id, data));
            } else {
                edges.push(Self::parse_edge_row_cols(
                    row,
                    "source_id",
                    "target_id",
                    "relationship_name",
                    "edge_props",
                )?);
            }
        }

        Ok((nodes, edges))
    }

    async fn get_id_filtered_graph_data(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if node_ids.is_empty() {
            return Ok((vec![], vec![]));
        }

        // Raw SQL — the edge query reuses the same $1..$N placeholders for both
        // source_id IN and target_id IN, a PostgreSQL optimisation that sea_query
        // cannot express.
        let placeholders: Vec<String> = (1..=node_ids.len()).map(|i| format!("${i}")).collect();
        let in_clause = placeholders.join(", ");

        let node_sql =
            format!("SELECT id, name, type, properties FROM graph_node WHERE id IN ({in_clause})");
        let edge_sql = format!(
            "SELECT source_id, target_id, relationship_name, properties FROM graph_edge \
             WHERE source_id IN ({in_clause}) AND target_id IN ({in_clause})"
        );

        let values: Vec<sea_orm::Value> = node_ids.iter().map(|id| id.clone().into()).collect();

        let node_rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &node_sql,
                values.clone(),
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut nodes = Vec::new();
        for row in &node_rows {
            let data = Self::parse_node_row(row)?;
            let id = data
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            nodes.push((id, data));
        }

        let edge_rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &edge_sql,
                values,
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut edges = Vec::new();
        for row in &edge_rows {
            edges.push(Self::parse_edge_row(row)?);
        }

        Ok((nodes, edges))
    }

    async fn get_neighborhood(
        &self,
        node_ids: &[String],
        depth: usize,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if node_ids.is_empty() {
            return Ok((vec![], vec![]));
        }

        // Single recursive-CTE round trip: expand the seed set out to `depth`
        // hops, then return both the node rows and the edges internal to the
        // resolved set. `ge.source_id`/`ge.target_id` are selected straight off
        // graph_edge (no CASE swap), so the true stored direction is preserved.
        // The two result halves are discriminated by a literal `kind` column;
        // the edge half's properties are aliased `edge_properties` to avoid a
        // collision with the node half's `properties` column.
        let sql = "WITH RECURSIVE neighborhood(id, hops) AS ( \
                       SELECT unnest($1::text[]), 0 \
                       UNION \
                       SELECT CASE WHEN e.source_id = n.id THEN e.target_id ELSE e.source_id END, \
                              n.hops + 1 \
                       FROM neighborhood n \
                       JOIN graph_edge e ON (e.source_id = n.id OR e.target_id = n.id) \
                       WHERE n.hops < $2 \
                   ), \
                   ids AS (SELECT DISTINCT id FROM neighborhood) \
                   SELECT 'node' AS kind, gn.id, gn.name, gn.type, gn.properties, \
                          NULL::text AS source_id, NULL::text AS target_id, \
                          NULL::text AS relationship_name, NULL::jsonb AS edge_properties \
                   FROM graph_node gn WHERE gn.id IN (SELECT id FROM ids) \
                   UNION ALL \
                   SELECT 'edge', NULL, NULL, NULL, NULL, \
                          ge.source_id, ge.target_id, ge.relationship_name, ge.properties \
                   FROM graph_edge ge \
                   WHERE ge.source_id IN (SELECT id FROM ids) \
                     AND ge.target_id IN (SELECT id FROM ids)";

        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                [node_ids.to_vec().into(), (depth as i64).into()],
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for row in &rows {
            let kind: String = row.try_get("", "kind").unwrap_or_default();
            if kind == "node" {
                let data = Self::parse_node_row(row)?;
                let id = data
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                nodes.push((id, data));
            } else {
                edges.push(Self::parse_edge_row_cols(
                    row,
                    "source_id",
                    "target_id",
                    "relationship_name",
                    "edge_properties",
                )?);
            }
        }

        Ok((nodes, edges))
    }
}

// ---------------------------------------------------------------------------
// Inline SeaORM migration — creates graph_node and graph_edge tables
// ---------------------------------------------------------------------------

mod migrator {
    use sea_orm_migration::prelude::*;

    pub struct Migrator;

    #[async_trait::async_trait]
    impl MigratorTrait for Migrator {
        /// Track applied migrations in a graph-specific bookkeeping table rather
        /// than the default `seaql_migrations`. In an "everything in one Postgres"
        /// deployment the core/relational migrator, the pgvector adapter and this
        /// graph adapter all point at the same database; if they shared the default
        /// table each would treat the others' versions as "applied but missing" and
        /// abort. See the `shared_db_migration_tests` module below.
        fn migration_table_name() -> DynIden {
            Alias::new("seaql_migrations_pggraph").into_iden()
        }

        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(CreateGraphTables)]
        }
    }

    struct CreateGraphTables;

    impl MigrationName for CreateGraphTables {
        fn name(&self) -> &str {
            "m20250101_000001_create_graph_tables"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for CreateGraphTables {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let conn = manager.get_connection();

            // -- graph_node table --
            conn.execute_unprepared(
                "CREATE TABLE IF NOT EXISTS graph_node ( \
                     id         VARCHAR PRIMARY KEY, \
                     name       VARCHAR NOT NULL DEFAULT '', \
                     type       VARCHAR NOT NULL DEFAULT '', \
                     properties JSONB NOT NULL DEFAULT '{}', \
                     created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                     updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW() \
                 )",
            )
            .await?;

            conn.execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_graph_node_type ON graph_node(type)",
            )
            .await?;

            // -- graph_edge table --
            conn.execute_unprepared(
                "CREATE TABLE IF NOT EXISTS graph_edge ( \
                     source_id         VARCHAR NOT NULL REFERENCES graph_node(id) ON DELETE CASCADE, \
                     target_id         VARCHAR NOT NULL REFERENCES graph_node(id) ON DELETE CASCADE, \
                     relationship_name VARCHAR NOT NULL, \
                     properties        JSONB NOT NULL DEFAULT '{}', \
                     created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                     updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                     PRIMARY KEY (source_id, target_id, relationship_name) \
                 )",
            )
            .await?;

            // Covering indexes for efficient neighbor lookups without heap reads.
            conn.execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_graph_edge_source_cover \
                 ON graph_edge(source_id) INCLUDE (target_id, relationship_name)",
            )
            .await?;

            conn.execute_unprepared(
                "CREATE INDEX IF NOT EXISTS idx_graph_edge_target_cover \
                 ON graph_edge(target_id) INCLUDE (source_id, relationship_name)",
            )
            .await?;

            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let conn = manager.get_connection();
            conn.execute_unprepared("DROP TABLE IF EXISTS graph_edge")
                .await?;
            conn.execute_unprepared("DROP TABLE IF EXISTS graph_node")
                .await?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Shared-Postgres migration regression tests
//
// These run only when `PGGRAPH_TEST_URL` points at a live Postgres instance and
// are skipped otherwise. They live inline (rather than under `tests/`) so they
// can reuse the crate's own optional `sea-orm`/`sea-orm-migration` dependencies
// without forcing a heavy dev-dependency onto the default (feature-off) build.
// (`cognee-database` is now a dev-dependency, but only to pin down the sqlx
// driver these tests' throwaway databases need at runtime — see the note on it
// in Cargo.toml; sea-orm itself still comes from this crate.)
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod shared_db_migration_tests {
    use super::PgGraphAdapter;
    use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
    use sea_orm_migration::prelude::*;

    fn test_url() -> Option<String> {
        std::env::var("PGGRAPH_TEST_URL").ok()
    }

    /// Run `body` against a throwaway database of this test's own, dropped again
    /// afterwards — even if `body` panics.
    ///
    /// Both cases below are about two migrators *coexisting inside one database*,
    /// so the isolation is at the database level and nothing inside it is reset.
    /// That replaces a `reset()` helper which dropped `graph_node`, `graph_edge`
    /// and both `seaql_migrations*` tables on the way in and out: destructive
    /// enough to wreck a developer's own database, and useless as mutual
    /// exclusion under `cargo nextest`, where each test runs in its own process
    /// and the `#[serial]` attribute these carried could not serialize anything.
    ///
    /// Not reaching a live Postgres is reported the same way as in the
    /// integration suite: a missing URL prints a skip line and returns; anything
    /// else — failing to create the database, connect, or migrate — panics with
    /// the underlying error rather than masquerading as "not configured".
    ///
    /// `body` runs on a spawned task so a failed assertion surfaces as a
    /// `JoinError` instead of unwinding past the drop. `TempPostgresDb::cleanup`
    /// is `async`, so it cannot be a `Drop` impl; without this the database would
    /// leak on every red run. The panic is re-raised unchanged afterwards.
    async fn with_temp_db<F, Fut>(what: &str, body: F)
    where
        F: FnOnce(String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let Some(base_url) = test_url() else {
            eprintln!("PGGRAPH_TEST_URL not set — skipping {what}");
            return;
        };
        let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
            .await
            .expect("PGGRAPH_TEST_URL is set, so CREATE DATABASE must succeed on that server");
        let outcome = tokio::spawn(body(tmp.url().to_string())).await;
        tmp.cleanup().await;
        if let Err(join_err) = outcome {
            // A `JoinError` here can only be a panic: the task is never aborted
            // and its handle is awaited immediately, so there is no cancellation
            // path. Assert that rather than leaning on it — `into_panic()` panics
            // on a cancelled task, which would replace the real failure with a
            // confusing one.
            assert!(
                join_err.is_panic(),
                "the {what} task was cancelled instead of panicking, which this helper never does: {join_err}"
            );
            std::panic::resume_unwind(join_err.into_panic());
        }
    }

    /// A stand-in for the downstream relational / auth migrator. It writes its
    /// versions into the DEFAULT `seaql_migrations` table — exactly what the core
    /// schema does in an all-Postgres deployment.
    struct RelationalMigrator;

    #[async_trait::async_trait]
    impl MigratorTrait for RelationalMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(RelBaseline), Box::new(RelAuth)]
        }
    }

    struct RelBaseline;
    impl MigrationName for RelBaseline {
        fn name(&self) -> &str {
            "m20260914_000001_baseline"
        }
    }
    #[async_trait::async_trait]
    impl MigrationTrait for RelBaseline {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .get_connection()
                .execute_unprepared("CREATE TABLE IF NOT EXISTS rel_baseline_marker (id INT)")
                .await?;
            Ok(())
        }
        async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
            Ok(())
        }
    }

    struct RelAuth;
    impl MigrationName for RelAuth {
        fn name(&self) -> &str {
            "m20260914_000002_auth"
        }
    }
    #[async_trait::async_trait]
    impl MigrationTrait for RelAuth {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .get_connection()
                .execute_unprepared("CREATE TABLE IF NOT EXISTS rel_auth_marker (id INT)")
                .await?;
            Ok(())
        }
        async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
            Ok(())
        }
    }

    /// Count rows in a bookkeeping table. Returns 0 **only** when the table does
    /// not exist; any other DB error panics so it fails the test rather than
    /// masquerading as an empty table (`table` is a fixed test literal, so
    /// interpolating it carries no injection risk).
    async fn version_count(db: &DatabaseConnection, table: &str) -> i64 {
        let exists = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                format!("SELECT to_regclass('{table}') IS NOT NULL AS present"),
            ))
            .await
            .unwrap()
            .and_then(|row| row.try_get::<bool>("", "present").ok())
            .unwrap_or(false);
        if !exists {
            return 0;
        }
        let row = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                format!("SELECT count(*) AS c FROM {table}"),
            ))
            .await
            .unwrap()
            .unwrap();
        row.try_get::<i64>("", "c").unwrap()
    }

    /// The relational migrator and the graph adapter migrator must coexist in
    /// one Postgres DB without colliding on the default `seaql_migrations` table.
    ///
    /// The database is this test's own, but *both* migrators still run inside
    /// that one database — sharing it is the thing under test.
    #[tokio::test]
    async fn pggraph_coexists_with_relational_migrator_in_shared_db() {
        with_temp_db("shared-DB migration test", |url| async move {
            let db = Database::connect(&url).await.unwrap();

            // 1. Relational / auth migrator runs first and populates the default
            //    `seaql_migrations` with versions the graph migrator does not own.
            RelationalMigrator::up(&db, None)
                .await
                .expect("relational migrator should succeed");
            assert_eq!(version_count(&db, "seaql_migrations").await, 2);

            // 2. Initialising the graph adapter against the SAME database must
            //    succeed. Before the fix it aborted with "Migration file of version
            //    'm20260914_000002_auth' is missing ...".
            let adapter = PgGraphAdapter::new(&url).await;
            assert!(
                adapter.is_ok(),
                "PgGraphAdapter init must not collide with the relational \
                 seaql_migrations table; got: {:?}",
                adapter.err()
            );

            // 3. The graph migrator tracks its version in its OWN table and leaves
            //    the relational bookkeeping untouched.
            assert_eq!(version_count(&db, "seaql_migrations").await, 2);
            assert_eq!(version_count(&db, "seaql_migrations_pggraph").await, 1);
        })
        .await;
    }

    /// Upgrade path: a legacy graph row left in the default `seaql_migrations`
    /// by an older build must be purged so the core migrator no longer chokes.
    ///
    /// Also runs in a database of its own, and again both migrators share it —
    /// the legacy row and the purge are only meaningful in one database.
    #[tokio::test]
    async fn pggraph_purges_legacy_row_from_default_table_on_upgrade() {
        with_temp_db("legacy-purge test", |url| async move {
            let db = Database::connect(&url).await.unwrap();

            // Simulate an older build that recorded the graph version into the
            // DEFAULT `seaql_migrations` table (aux-ran-before-core ordering).
            db.execute(Statement::from_string(
                db.get_database_backend(),
                "CREATE TABLE seaql_migrations (version VARCHAR PRIMARY KEY, applied_at BIGINT NOT NULL)",
            ))
            .await
            .unwrap();
            db.execute(Statement::from_string(
                db.get_database_backend(),
                "INSERT INTO seaql_migrations (version, applied_at) \
                 VALUES ('m20250101_000001_create_graph_tables', 0)",
            ))
            .await
            .unwrap();

            // Upgraded build initialises the graph adapter.
            PgGraphAdapter::new(&url)
                .await
                .expect("graph adapter should initialise on upgrade");

            // The stale graph row is gone, so the core/relational migrator can now
            // run against the default table without aborting.
            assert_eq!(
                version_count(&db, "seaql_migrations").await,
                0,
                "legacy graph row must be purged from the default seaql_migrations"
            );
            RelationalMigrator::up(&db, None)
                .await
                .expect("core migrator must not choke after legacy row is purged");
        })
        .await;
    }
}

// ---------------------------------------------------------------------------
// NUL-byte sanitization (no database required)
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod sanitize_tests {
    use super::PgGraphAdapter;
    use serde_json::json;

    /// `serialize_node_to_row` is the single choke point for every node write on
    /// this adapter, so the strip has to happen there or not at all.
    ///
    /// Before this, a chunk carrying a literal `0x00` — 42.5% of one real corpus
    /// of PDF-extracted papers — made Postgres reject the first `add_nodes` of
    /// the run with `unsupported Unicode escape sequence`, discarding every
    /// chunk, summary and embedding the two LLM stages had just produced.
    #[test]
    fn serialize_node_to_row_strips_nul_bytes() {
        let node = json!({
            "id": "chunk\u{0}1",
            "name": "Nikola\u{0} Tesla",
            "type": "Doc\u{0}Chunk",
            "text": "page 1\u{0}page 2",
            "nested": {"inner": ["a\u{0}b"]},
            "count": 7,
        });

        let row = PgGraphAdapter::serialize_node_to_row(&node).unwrap();

        assert_eq!(row.id, "chunk1");
        assert_eq!(row.name, "Nikola Tesla");
        assert_eq!(row.node_type, "DocChunk");

        let props = serde_json::to_string(&row.properties).unwrap();
        assert!(
            !props.contains("\\u0000"),
            "properties must carry no NUL escape, got: {props}"
        );
        assert_eq!(row.properties["text"], json!("page 1page 2"));
        assert_eq!(row.properties["nested"]["inner"][0], json!("ab"));
        // Non-string values are untouched.
        assert_eq!(row.properties["count"], json!(7));
    }

    #[test]
    fn serialize_node_to_row_leaves_clean_text_alone() {
        let node = json!({
            "id": "n1",
            "name": "Ada Lovelace",
            "type": "Person",
            "text": "no nulls — just an em dash and 日本語",
        });

        let row = PgGraphAdapter::serialize_node_to_row(&node).unwrap();

        assert_eq!(row.id, "n1");
        assert_eq!(row.name, "Ada Lovelace");
        assert_eq!(
            row.properties["text"],
            json!("no nulls — just an em dash and 日本語")
        );
    }
}
