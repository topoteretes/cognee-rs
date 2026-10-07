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
use sea_orm::sea_query::{Alias, Cond, Expr, Iden, Query};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, DbErr,
    ExecResult, QueryResult, Statement, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use tracing::debug;

use cognee_utils::sanitize::{sanitize_json, sanitize_str, sanitize_string};

use crate::error::{GraphDBError, GraphDBResult};
use crate::traits::{GraphDBTrait, NeighborhoodScope, apply_neighborhood_scope};
use crate::types::{EdgeData, GraphNode, NodeData, parse_audit_timestamp};

/// Rows per bulk upsert statement. Columns bind as one array parameter each,
/// so this is bounded only by statement memory, not by PostgreSQL's 65 535
/// bind-parameter limit.
const WRITE_BATCH: usize = 5000;

/// Ids per `= ANY($1::text[])` array parameter on the read/delete paths.
const ID_BATCH: usize = 20_000;

/// `plan_cache_mode = force_custom_plan`, as a `SET LOCAL`.
///
/// The `SET LOCAL` twin of the connection option [`PgGraphAdapter::new`] puts on
/// the pool it opens itself; that constructor's comment carries the measurement
/// (1.2 ms custom vs 10.9 ms generic for a 5-entity `get_neighborhood` on a 10k
/// store, the generic plan having fallen back to a sequential scan of
/// `graph_edge`). This is the form [`PgGraphAdapter::from_connection`] has to
/// use, because the pool is the caller's — in the single-shared-Postgres layout
/// it is the relational store's — so its `ConnectOptions` are not ours to
/// change. Keep the two in step.
///
/// `plan_cache_mode` is core Postgres and needs no version gate, unlike the
/// `hnsw.*` settings the vector adapter carries the same way.
const PLAN_CACHE_LOCAL: &str = "SET LOCAL plan_cache_mode = force_custom_plan";

/// Nodes of the materialized id-set CTE `ids` plus the edges induced on it:
/// each id's outgoing edges through the source-side index (a `LATERAL`
/// subquery, fenced with `OFFSET 0`, so it is always a per-id index probe),
/// kept when the target is in the set. Rows are discriminated by `kind`; see
/// [`PgGraphAdapter::subgraph_query`].
///
/// The plain `ids JOIN graph_edge JOIN ids` form leaves the choice to the
/// planner, which cannot estimate a materialized CTE's size well and picked
/// a sequential scan of `graph_edge` plus hash joins for a 10-seed
/// neighbourhood (88 ids): 3.3-3.9 ms vs 1.8-2.1 ms for this form on a 10k
/// store, and the scan grows with the whole edge table.
const INDUCED_SUBGRAPH: &str = "\
    SELECT 'node' AS kind, n.id, n.name, n.type, n.properties, \
           NULL::text AS source_id, NULL::text AS target_id, \
           NULL::text AS relationship_name, NULL::jsonb AS edge_properties \
    FROM ids CROSS JOIN LATERAL \
         (SELECT x.id, x.name, x.type, x.properties FROM graph_node x \
          WHERE x.id = ids.id OFFSET 0) n \
    UNION ALL \
    SELECT 'edge', NULL, NULL, NULL, NULL, \
           e.source_id, e.target_id, e.relationship_name, e.properties \
    FROM ids a CROSS JOIN LATERAL \
         (SELECT * FROM graph_edge x WHERE x.source_id = a.id OFFSET 0) e \
    WHERE e.target_id IN (SELECT id FROM ids)";

/// The node half is a per-id primary-key probe (`LATERAL`, fenced with
/// `OFFSET 0`) in both forms: as a plain join the planner, unable to size the
/// materialized id set, priced the probes as random IO and hashed all of
/// `graph_node` instead — a 2 GB temp-file spill per 24 triplet queries at
/// 100k, 143 ms per call against ~30 ms for nested probes.
///
/// [`INDUCED_SUBGRAPH`] with the edge half as two `IN (SELECT id FROM ids)`
/// semi-joins. For large id sets (hundreds of seeds, thousands of ids) the
/// explicit double join plans as a merge join that spends most of its time
/// sorting edge rows by `target_id` under the database collation; hashed
/// semi-joins avoid the sort. For a handful of seeds it is slower than the
/// join form, hence both.
const INDUCED_SUBGRAPH_SEMI: &str = "\
    SELECT 'node' AS kind, n.id, n.name, n.type, n.properties, \
           NULL::text AS source_id, NULL::text AS target_id, \
           NULL::text AS relationship_name, NULL::jsonb AS edge_properties \
    FROM ids CROSS JOIN LATERAL \
         (SELECT x.id, x.name, x.type, x.properties FROM graph_node x \
          WHERE x.id = ids.id OFFSET 0) n \
    UNION ALL \
    SELECT 'edge', NULL, NULL, NULL, NULL, \
           e.source_id, e.target_id, e.relationship_name, e.properties \
    FROM graph_edge e \
    WHERE e.source_id IN (SELECT id FROM ids) AND e.target_id IN (SELECT id FROM ids)";

/// A bulk write of at least this many rows into a graph table that has never
/// been analysed runs `ANALYZE` on it (see
/// [`PgGraphAdapter::analyze_if_never_analyzed`]).
const ANALYZE_MIN_ROWS: usize = 1000;

/// Id-set size from which `get_neighborhood` uses [`INDUCED_SUBGRAPH_SEMI`].
const SEMI_JOIN_MIN_SEEDS: usize = 64;

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
    /// Whether every pooled connection was opened with [`Self::new`]'s session
    /// options — today just `plan_cache_mode = force_custom_plan`.
    ///
    /// `false` for [`Self::from_connection`], where the parameterised
    /// statements carry the same setting themselves as a `SET LOCAL` (see
    /// [`Self::tuned_locals`]), so an adapter answers and plans the same
    /// whichever constructor built it — only the round trip differs.
    tuned_sessions: bool,
}

impl PgGraphAdapter {
    /// Connect to an existing PostgreSQL database and run graph-table migrations.
    ///
    /// The database must already exist. Use [`Self::from_connection`] to share
    /// a connection that was established elsewhere (e.g. by the database crate).
    pub async fn new(database_url: &str) -> GraphDBResult<Self> {
        // Custom plans for every statement on this adapter's own pool. The
        // graph reads bind their id sets as one `text[]`, and sqlx caches each
        // prepared statement per connection; after five executions Postgres
        // may switch it to a generic plan, which cannot see the array's size
        // and so costs a 5-seed neighbourhood like any other. Measured on a
        // 10k store: the hybrid lane's 5-entity `get_neighborhood` ran 1.2 ms
        // with a custom plan and 10.9 ms with the generic one (a sequential
        // scan of `graph_edge` for the induced edges). Planning these
        // statements costs well under a millisecond.
        let mut opts = ConnectOptions::new(database_url.to_string());
        opts.map_sqlx_postgres_opts(|o| {
            o.options([("plan_cache_mode", "force_custom_plan".to_string())])
        });
        let db = Database::connect(opts)
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
            tuned_sessions: true,
        })
    }

    /// Wrap an existing SeaORM `DatabaseConnection` (must be Postgres).
    ///
    /// Only the graph tables are created if missing (via migration).
    ///
    /// The session tuning [`Self::new`] puts in its pool's connection options
    /// cannot be applied here — the pool is the caller's, and in the
    /// single-shared-Postgres layout that caller is the relational store — so
    /// the parameterised statements carry it per statement instead
    /// (`tuned_sessions: false`; see [`Self::tuned_locals`]). This is the
    /// constructor that layout actually uses, so it is the one where the
    /// generic-plan cliff `new()` measures would otherwise be hit.
    pub async fn from_connection(db: DatabaseConnection) -> GraphDBResult<Self> {
        cleanup_legacy_seaql_migrations(&db).await?;
        migrator::Migrator::up(&db, None).await.map_err(|e| {
            GraphDBError::InitializationError(format!("PgGraph migration failed: {e}"))
        })?;

        Ok(Self {
            db,
            owns_pool: false,
            tuned_sessions: false,
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

    /// `ANALYZE table` if it has never been analysed (`reltuples = -1`).
    ///
    /// Autovacuum analyses a table only after its naptime (60 s) and once
    /// enough rows changed, so a store searched right after its first
    /// cognify — every 10k-tier run here — plans the neighbourhood reads
    /// with no statistics at all: hash joins over guessed sizes, 9.4 ms per
    /// hybrid entity neighbourhood against 0.8 ms once analysed. One cheap
    /// catalog probe per bulk write; failures are logged (the write already
    /// succeeded).
    async fn analyze_if_never_analyzed(&self, table: &str) {
        let probe = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(
                    "SELECT (reltuples < 0) AS never FROM pg_class WHERE oid = '{table}'::regclass"
                ),
            ))
            .await;
        let never =
            matches!(probe, Ok(Some(ref r)) if r.try_get::<bool>("", "never").unwrap_or(false));
        if never
            && let Err(e) = self
                .db
                .execute_unprepared(&format!("ANALYZE {table}"))
                .await
        {
            debug!("ANALYZE {table} failed: {e}");
        }
    }

    /// The `SET LOCAL` a statement has to carry on a connection this adapter
    /// did not open, or `None` on one of its own pools.
    ///
    /// Applied to the **parameterised** statements only, and that is the whole
    /// scope of the setting: a generic plan can differ from a custom one only
    /// where there is a parameter to be costed blind. The unparameterised
    /// statements here — the `count(*)`s, `is_empty`, `get_graph_data`,
    /// `get_all_relationship_names`, the `reltuples` probe — plan identically
    /// either way, so wrapping them would buy a transaction and a round trip
    /// for nothing. `ANALYZE` could not take it at all: it cannot run inside a
    /// transaction block.
    ///
    /// A plain `SET` would be worse than nothing: it outlives the statement and
    /// leaks to whatever borrows that pooled connection next, which on a
    /// caller-owned pool is the relational store's own queries.
    ///
    /// Pure in `tuned_sessions` so the decision is testable without a server.
    fn tuned_locals(tuned_sessions: bool) -> Option<&'static str> {
        (!tuned_sessions).then_some(PLAN_CACHE_LOCAL)
    }

    /// [`ConnectionTrait::query_all`] with [`Self::tuned_locals`] applied, in
    /// the transaction that makes a `SET LOCAL` mean anything — outside one it
    /// is a no-op with a warning — and that scopes it to this statement so it
    /// cannot leak to the next borrower of a pooled connection.
    ///
    /// One plain statement on this adapter's own pools, where the connection
    /// options already carry the setting.
    async fn query_all_tuned(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        let Some(locals) = Self::tuned_locals(self.tuned_sessions) else {
            return self.db.query_all(stmt).await;
        };
        let txn = self.db.begin().await?;
        txn.execute_unprepared(locals).await?;
        let rows = txn.query_all(stmt).await?;
        txn.commit().await?;
        Ok(rows)
    }

    /// [`Self::query_all_tuned`] for a single-row read.
    async fn query_one_tuned(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        let Some(locals) = Self::tuned_locals(self.tuned_sessions) else {
            return self.db.query_one(stmt).await;
        };
        let txn = self.db.begin().await?;
        txn.execute_unprepared(locals).await?;
        let row = txn.query_one(stmt).await?;
        txn.commit().await?;
        Ok(row)
    }

    /// [`Self::query_all_tuned`] for a write. The write paths need it too: a
    /// `DELETE … WHERE id = ANY($1::text[])` costed without the array is the
    /// same sequential scan the read paths fall into.
    async fn execute_tuned(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        let Some(locals) = Self::tuned_locals(self.tuned_sessions) else {
            return self.db.execute(stmt).await;
        };
        let txn = self.db.begin().await?;
        txn.execute_unprepared(locals).await?;
        let out = txn.execute(stmt).await?;
        txn.commit().await?;
        Ok(out)
    }

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

    /// Run a node query (`id, name, type, properties`) and key each row by id.
    async fn graph_nodes_where(
        &self,
        sql: &str,
        values: Vec<sea_orm::Value>,
    ) -> GraphDBResult<Vec<GraphNode>> {
        let rows = self
            .query_all_tuned(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
        rows.iter().map(Self::parse_graph_node).collect()
    }

    /// [`Self::parse_node_row`], keyed by the node's id.
    fn parse_graph_node(row: &sea_orm::QueryResult) -> GraphDBResult<GraphNode> {
        let data = Self::parse_node_row(row)?;
        let id = data
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        Ok((id, data))
    }

    /// Run a `kind`-discriminated node/edge union (`'node'` rows carry
    /// `id, name, type, properties`; edge rows `source_id, target_id,
    /// relationship_name, edge_properties`) and split it.
    async fn subgraph_query(
        &self,
        sql: &str,
        values: Vec<sea_orm::Value>,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        let rows = self
            .query_all_tuned(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                sql,
                values,
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for row in &rows {
            let kind: String = row.try_get("", "kind").unwrap_or_default();
            if kind == "node" {
                nodes.push(Self::parse_graph_node(row)?);
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
            .query_one_tuned(self.build(&query))
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

        // Serialize and deduplicate by id (last wins), keeping first-seen order.
        let mut order: Vec<String> = Vec::with_capacity(nodes.len());
        let mut seen: HashMap<String, NodeRow> = HashMap::with_capacity(nodes.len());
        for node in &nodes {
            let row = Self::serialize_node_to_row(node)?;
            if !seen.contains_key(&row.id) {
                order.push(row.id.clone());
            }
            seen.insert(row.id.clone(), row);
        }

        // One `INSERT … SELECT FROM unnest(…)` per `WRITE_BATCH` rows: every
        // column travels as a single array parameter, so the statement text
        // (and its cached prepared plan) is the same for every batch, and a
        // batch costs one round trip instead of `rows / 100`.
        for chunk in order.chunks(WRITE_BATCH) {
            let mut ids = Vec::with_capacity(chunk.len());
            let mut names = Vec::with_capacity(chunk.len());
            let mut types = Vec::with_capacity(chunk.len());
            let mut props = Vec::with_capacity(chunk.len());
            let mut created = Vec::with_capacity(chunk.len());
            let mut updated = Vec::with_capacity(chunk.len());
            for id in chunk {
                let Some(row) = seen.remove(id) else {
                    continue;
                };
                ids.push(row.id);
                names.push(row.name);
                types.push(row.node_type);
                props.push(row.properties);
                created.push(row.created_at.to_rfc3339());
                updated.push(row.updated_at.to_rfc3339());
            }
            self.execute_tuned(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "INSERT INTO graph_node (id, name, type, properties, created_at, updated_at) \
                     SELECT * FROM unnest($1::text[], $2::text[], $3::text[], $4::jsonb[], \
                                          $5::text[]::timestamptz[], $6::text[]::timestamptz[]) \
                     ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, type = EXCLUDED.type, \
                       properties = EXCLUDED.properties, updated_at = CURRENT_TIMESTAMP",
                [
                    sea_orm::Value::from(ids),
                    sea_orm::Value::from(names),
                    sea_orm::Value::from(types),
                    sea_orm::Value::from(props),
                    sea_orm::Value::from(created),
                    sea_orm::Value::from(updated),
                ],
            ))
            .await
            .map_err(|e| GraphDBError::NodeError(format!("Failed to upsert nodes: {e}")))?;
        }
        if order.len() >= ANALYZE_MIN_ROWS {
            self.analyze_if_never_analyzed("graph_node").await;
        }

        Ok(())
    }

    async fn delete_node(&self, node_id: &str) -> GraphDBResult<()> {
        let query = Query::delete()
            .from_table(GNode::Table)
            .and_where(Expr::col(GNode::Id).eq(node_id))
            .to_owned();

        self.execute_tuned(self.build(&query))
            .await
            .map_err(|e| GraphDBError::NodeError(format!("Failed to delete node: {e}")))?;
        Ok(())
    }

    async fn delete_nodes(&self, node_ids: &[String]) -> GraphDBResult<()> {
        // One `= ANY($1)` array parameter per `ID_BATCH` ids: a constant
        // statement text, and no ceiling at PostgreSQL's 65 535 bind
        // parameters (the per-id `IN ($1, …)` list failed past it).
        for chunk in node_ids.chunks(ID_BATCH) {
            self.execute_tuned(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "DELETE FROM graph_node WHERE id = ANY($1::text[])",
                [chunk.to_vec().into()],
            ))
            .await
            .map_err(|e| GraphDBError::NodeError(format!("Failed to delete nodes: {e}")))?;
        }
        Ok(())
    }

    async fn get_node(&self, node_id: &str) -> GraphDBResult<Option<NodeData>> {
        let query = Query::select()
            .columns([GNode::Id, GNode::Name, GNode::Type, GNode::Properties])
            .from(GNode::Table)
            .and_where(Expr::col(GNode::Id).eq(node_id))
            .to_owned();

        let row = self
            .query_one_tuned(self.build(&query))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;

        match row {
            Some(r) => Ok(Some(Self::parse_node_row(&r)?)),
            None => Ok(None),
        }
    }

    async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
        // Array parameter per `ID_BATCH` ids, as in `delete_nodes`.
        let mut out = Vec::with_capacity(node_ids.len());
        for chunk in node_ids.chunks(ID_BATCH) {
            let rows = self
                .query_all_tuned(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "SELECT id, name, type, properties FROM graph_node WHERE id = ANY($1::text[])",
                    [chunk.to_vec().into()],
                ))
                .await
                .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
            for row in &rows {
                out.push(Self::parse_node_row(row)?);
            }
        }
        Ok(out)
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
        // Probe with the *sanitized* triple, because that is the key `add_edges`
        // stores under. Passing the raw strings straight through would fail
        // twice over: an edge written under its sanitized name would read as
        // absent when probed with the NUL-bearing original (so dedup misfires
        // and the caller re-adds it), and — worse — a NUL inside a bound `text`
        // parameter makes Postgres reject *the query itself* with `invalid byte
        // sequence for encoding "UTF8": 0x00`, turning a lookup into a hard
        // error. Unlike node ids (see `serialize_node_to_row`, where the
        // read/write asymmetry is deliberate parity with Python), edge
        // relationship names carry real chunk text, so reads and writes must
        // agree on the key.
        let source_id = sanitize_str(source_id);
        let target_id = sanitize_str(target_id);
        let relationship_name = sanitize_str(relationship_name);

        let inner = Query::select()
            .expr(Expr::val(1))
            .from(GEdge::Table)
            .and_where(Expr::col(GEdge::SourceId).eq(source_id.as_ref()))
            .and_where(Expr::col(GEdge::TargetId).eq(target_id.as_ref()))
            .and_where(Expr::col(GEdge::RelationshipName).eq(relationship_name.as_ref()))
            .to_owned();

        let query = Query::select()
            .expr_as(Expr::exists(inner), Alias::new("ex"))
            .to_owned();

        let row = self
            .query_one_tuned(self.build(&query))
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
        //
        // The probe triples are sanitized for the same two reasons as in
        // `has_edge` above: `add_edges` keys rows on the sanitized triple, and a
        // NUL inside a bound `text[]` element makes Postgres reject the whole
        // statement with `invalid byte sequence for encoding "UTF8": 0x00`.
        let keys: Vec<(String, String, String)> = edges
            .iter()
            .map(|e| {
                (
                    sanitize_str(&e.0).into_owned(),
                    sanitize_str(&e.1).into_owned(),
                    sanitize_str(&e.2).into_owned(),
                )
            })
            .collect();
        let sources: Vec<_> = keys.iter().map(|k| k.0.clone()).collect();
        let targets: Vec<_> = keys.iter().map(|k| k.1.clone()).collect();
        let rels: Vec<_> = keys.iter().map(|k| k.2.clone()).collect();

        let rows = self
            .query_all_tuned(Statement::from_sql_and_values(
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

        // Match on the sanitized key computed above — `existing` holds what the
        // database returned, which is sanitized by construction — but return the
        // caller's original `EdgeData` untouched.
        let found = edges
            .iter()
            .zip(&keys)
            .filter(|(_, key)| existing.contains(*key))
            .map(|(edge, _)| edge.clone())
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
        // Both halves are sanitized, for the two reasons `has_edge` above and
        // `add_edges` already document: the value because Postgres rejects a
        // NUL in a `jsonb` cast, and the key columns because `add_edges` keys
        // rows on the sanitized triple *and* a NUL inside a bound parameter
        // makes Postgres reject the whole statement.
        //
        // Note this is the edge rule, not the node rule: node ids stay raw on
        // lookup paths (see the write-only-sanitization note in
        // `serialize_node_to_row`, which is about `get_node`/`get_nodes` and
        // Python parity). Edge probes deliberately went the other way.
        let patch = sanitize_json(json!({ key: value }));
        let source_id = sanitize_str(source_id).into_owned();
        let target_id = sanitize_str(target_id).into_owned();
        let relationship_name = sanitize_str(relationship_name).into_owned();
        let result = self
            .execute_tuned(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "UPDATE graph_edge \
                 SET properties = COALESCE(properties, '{}'::jsonb) || $4::jsonb, \
                     updated_at = CURRENT_TIMESTAMP \
                 WHERE source_id = $1 AND target_id = $2 AND relationship_name = $3",
                [
                    source_id.clone().into(),
                    target_id.clone().into(),
                    relationship_name.clone().into(),
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

        // Probe triples are sanitized, as `has_edges` does: `add_edges` keys
        // rows on the sanitized triple, and a NUL inside a bound `text[]`
        // element makes Postgres reject the whole statement.
        let keys: Vec<crate::traits::EdgeKey> = edge_keys
            .iter()
            .map(|k| {
                (
                    sanitize_str(&k.0).into_owned(),
                    sanitize_str(&k.1).into_owned(),
                    sanitize_str(&k.2).into_owned(),
                )
            })
            .collect();
        let sources: Vec<String> = keys.iter().map(|k| k.0.clone()).collect();
        let targets: Vec<String> = keys.iter().map(|k| k.1.clone()).collect();
        let rels: Vec<String> = keys.iter().map(|k| k.2.clone()).collect();

        let rows = self
            .query_all_tuned(Statement::from_sql_and_values(
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

        let mut stored: HashMap<crate::traits::EdgeKey, f64> = HashMap::with_capacity(rows.len());
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
                stored.insert((s, t, r), weight);
            }
        }

        // Rows carry the sanitized key; report under the caller's own key, the
        // way `has_edges` returns the caller's `EdgeData` untouched.
        let mut out = HashMap::with_capacity(stored.len());
        for (original, sanitized) in edge_keys.iter().zip(&keys) {
            if let Some(weight) = stored.get(sanitized) {
                out.insert(original.clone(), *weight);
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

        // Sanitized (stored) key -> the caller's key. Keys are sanitized for
        // the same reasons as in `has_edges`, and results are reported under
        // the caller's own key rather than the stored form.
        let mut by_stored: HashMap<crate::traits::EdgeKey, crate::traits::EdgeKey> =
            HashMap::with_capacity(updates.len());

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
            let stored = (
                sanitize_str(&key.0).into_owned(),
                sanitize_str(&key.1).into_owned(),
                sanitize_str(&key.2).into_owned(),
            );
            sources.push(stored.0.clone());
            targets.push(stored.1.clone());
            rels.push(stored.2.clone());
            by_stored.insert(stored, key.clone());
            // `{:?}` is f64's shortest round-trip representation, so the
            // float8 cast parses back to exactly this value.
            weights.push(format!("{weight:?}"));
        }

        if sources.is_empty() {
            return Ok(out);
        }

        let rows = self
            .query_all_tuned(Statement::from_sql_and_values(
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
            if let Some(original) = by_stored.get(&(s, t, r)) {
                out.insert(original.clone(), true);
            }
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
        //
        // The map holds a *borrow* of the properties and serializes only the
        // survivors: `to_value` + `sanitize_json` on an entry that the next
        // duplicate immediately overwrites is pure waste.
        type EdgeProps = HashMap<Cow<'static, str>, Value>;
        let mut order: Vec<(String, String, String)> = Vec::with_capacity(edges.len());
        let mut seen: HashMap<(String, String, String), &EdgeProps> =
            HashMap::with_capacity(edges.len());
        for edge in edges {
            let key = (
                sanitize_str(&edge.0).into_owned(),
                sanitize_str(&edge.1).into_owned(),
                sanitize_str(&edge.2).into_owned(),
            );
            if seen.insert(key.clone(), &edge.3).is_none() {
                order.push(key);
            }
        }

        // One `INSERT … SELECT FROM unnest(…)` per `WRITE_BATCH` edges (see
        // `add_nodes_raw`). All rows of a call share one `created_at`, as
        // before.
        let now = now.to_rfc3339();
        for chunk in order.chunks_mut(WRITE_BATCH) {
            let mut src = Vec::with_capacity(chunk.len());
            let mut dst = Vec::with_capacity(chunk.len());
            let mut rel = Vec::with_capacity(chunk.len());
            let mut props = Vec::with_capacity(chunk.len());
            for key in chunk.iter_mut() {
                let Some(p) = seen.get(key) else {
                    continue;
                };
                // Edge properties carry LLM-authored descriptions and, through
                // them, source text — same NUL exposure as node properties.
                let json = serde_json::to_value(p).map_err(GraphDBError::SerializationError)?;
                props.push(sanitize_json(json));
                src.push(std::mem::take(&mut key.0));
                dst.push(std::mem::take(&mut key.1));
                rel.push(std::mem::take(&mut key.2));
            }
            self
                .execute_tuned(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    "INSERT INTO graph_edge (source_id, target_id, relationship_name, properties, \
                                             created_at, updated_at) \
                     SELECT s, t, r, p, $5::timestamptz, $5::timestamptz \
                     FROM unnest($1::text[], $2::text[], $3::text[], $4::jsonb[]) AS u(s, t, r, p) \
                     ON CONFLICT (source_id, target_id, relationship_name) \
                     DO UPDATE SET properties = EXCLUDED.properties, updated_at = CURRENT_TIMESTAMP",
                    [
                        sea_orm::Value::from(src),
                        sea_orm::Value::from(dst),
                        sea_orm::Value::from(rel),
                        sea_orm::Value::from(props),
                        sea_orm::Value::from(now.clone()),
                    ],
                ))
                .await
                .map_err(|e| GraphDBError::EdgeError(format!("Failed to upsert edges: {e}")))?;
        }
        if order.len() >= ANALYZE_MIN_ROWS {
            self.analyze_if_never_analyzed("graph_edge").await;
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
            .query_all_tuned(self.build(&query))
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
        // Two directed index probes unioned into the id set, instead of one
        // `source_id = $1 OR target_id = $1` scan joined through a CASE.
        let rows = self
            .query_all_tuned(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT m.id, m.name, m.type, m.properties FROM graph_node m \
                 WHERE m.id IN (SELECT target_id FROM graph_edge WHERE source_id = $1 \
                                UNION SELECT source_id FROM graph_edge WHERE target_id = $1)",
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
            .query_all_tuned(Statement::from_sql_and_values(
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

        // Connected components by one streaming union-find pass over the
        // node and edge keys (O(V + E)); the recursive CTE this replaces
        // materialised every (node, reachable root) pair and did not finish
        // within minutes on a 10k-node cognify graph.
        let component_sizes: Vec<Value> = components::component_sizes(&self.db)
            .await?
            .into_iter()
            .map(|size| json!(size))
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
        // Only whitelisted attributes may be interpolated (SQL injection);
        // each attribute's values bind as one `text[]`.
        let mut clauses = Vec::new();
        let mut values: Vec<sea_orm::Value> = Vec::new();
        for (attr, filter_values) in attribute_filters {
            if filter_values.is_empty() {
                continue;
            }
            if !ALLOWED_FILTER_ATTRS.contains(&attr.as_ref()) {
                return Err(GraphDBError::QueryError(format!(
                    "Invalid filter attribute: {attr:?}. Allowed: {ALLOWED_FILTER_ATTRS:?}"
                )));
            }
            let strings: Vec<String> = filter_values
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .unwrap_or_else(|| v.to_string())
                })
                .collect();
            values.push(strings.into());
            clauses.push(format!("n.{attr} = ANY(${}::text[])", values.len()));
        }

        if clauses.is_empty() {
            return self.get_graph_data().await;
        }

        let sql = format!(
            "WITH ids AS MATERIALIZED (SELECT n.id FROM graph_node n WHERE {}) {INDUCED_SUBGRAPH}",
            clauses.join(" AND ")
        );
        self.subgraph_query(&sql, values).await
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
            .query_all_tuned(Statement::from_sql_and_values(
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

    /// The primary nodes (`type` = `node_type`, `name` in `node_names`), their
    /// neighbours — any neighbour for `"OR"`, otherwise only neighbours
    /// adjacent to as many distinct primaries as there are requested names —
    /// and the edges induced on that set. Driven through the edge indexes
    /// instead of `OR`-ed `IN (subquery)` predicates that scan the whole edge
    /// table.
    async fn get_nodeset_subgraph(
        &self,
        node_type: &str,
        node_names: &[String],
        node_name_filter_operator: &str,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if node_names.is_empty() {
            return Ok((vec![], vec![]));
        }
        // (neighbour, primary) per edge touching a primary node; an edge whose
        // source is primary contributes its target, otherwise its source.
        let touching = "touch AS ( \
            SELECT e.target_id AS nbr, e.source_id AS prim \
              FROM p JOIN graph_edge e ON e.source_id = p.id \
            UNION ALL \
            SELECT e.source_id, e.target_id \
              FROM p JOIN graph_edge e ON e.target_id = p.id \
             WHERE NOT EXISTS (SELECT 1 FROM p p2 WHERE p2.id = e.source_id))";
        let neighbours = if node_name_filter_operator == "OR" {
            "SELECT nbr FROM touch"
        } else {
            "SELECT nbr FROM touch GROUP BY nbr HAVING count(DISTINCT prim) = $3"
        };
        let sql = format!(
            "WITH p AS MATERIALIZED (SELECT DISTINCT id FROM graph_node \
                                     WHERE type = $1 AND name = ANY($2::text[])), \
             {touching}, \
             ids AS MATERIALIZED (SELECT id FROM p UNION {neighbours}) \
             {INDUCED_SUBGRAPH}"
        );
        let mut values: Vec<sea_orm::Value> = vec![node_type.into(), node_names.to_vec().into()];
        if node_name_filter_operator != "OR" {
            values.push(i64::try_from(node_names.len()).unwrap_or(i64::MAX).into());
        }
        self.subgraph_query(&sql, values).await
    }

    async fn get_id_filtered_graph_data(
        &self,
        node_ids: &[String],
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if node_ids.is_empty() {
            return Ok((vec![], vec![]));
        }
        // One `text[]` parameter instead of the id list bound twice (once per
        // edge endpoint), so no 65 535-parameter ceiling at ~32k ids.
        let sql = format!(
            "WITH ids AS MATERIALIZED (SELECT DISTINCT unnest($1::text[]) AS id) {INDUCED_SUBGRAPH}"
        );
        self.subgraph_query(&sql, vec![node_ids.to_vec().into()])
            .await
    }

    /// Nodes of `node_type` with exactly one incident edge (a self-loop
    /// counts twice, as in the trait default), without loading the graph.
    /// Each candidate probes the edge indexes and stops at the second hit, so
    /// hubs cost no more than leaves.
    async fn get_degree_one_nodes(&self, node_type: &str) -> GraphDBResult<Vec<GraphNode>> {
        self.graph_nodes_where(
            "SELECT n.id, n.name, n.type, n.properties FROM graph_node n \
             WHERE n.type = $1 AND ( \
               SELECT count(*) FROM ( \
                 (SELECT 1 FROM graph_edge e WHERE e.source_id = n.id LIMIT 2) \
                 UNION ALL \
                 (SELECT 1 FROM graph_edge e WHERE e.target_id = n.id LIMIT 2) \
               ) d) = 1",
            vec![node_type.into()],
        )
        .await
    }

    async fn get_all_relationship_names(&self) -> GraphDBResult<HashSet<String>> {
        let rows = self
            .db
            .query_all(Statement::from_string(
                DatabaseBackend::Postgres,
                "SELECT DISTINCT relationship_name FROM graph_edge".to_string(),
            ))
            .await
            .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
        rows.iter()
            .map(|r| {
                r.try_get("", "relationship_name")
                    .map_err(|e| GraphDBError::QueryError(e.to_string()))
            })
            .collect()
    }

    /// `EdgeType` nodes with no incident edge whose `relationship_name`
    /// property (default "") names no edge — the trait default's predicate,
    /// evaluated in SQL instead of over the loaded graph.
    async fn get_zero_degree_edge_type_nodes(&self) -> GraphDBResult<Vec<GraphNode>> {
        self.graph_nodes_where(
            "SELECT n.id, n.name, n.type, n.properties FROM graph_node n \
             WHERE n.type = 'EdgeType' \
               AND NOT EXISTS (SELECT 1 FROM graph_edge e WHERE e.source_id = n.id) \
               AND NOT EXISTS (SELECT 1 FROM graph_edge e WHERE e.target_id = n.id) \
               AND NOT EXISTS (SELECT 1 FROM graph_edge e WHERE e.relationship_name = \
                   CASE WHEN jsonb_typeof(n.properties->'relationship_name') = 'string' \
                        THEN n.properties->>'relationship_name' ELSE '' END)",
            vec![],
        )
        .await
    }

    /// The seeds' `depth`-hop neighbourhood: every node within `depth`
    /// undirected hops and every edge whose endpoints are both in that set
    /// (stored direction preserved).
    ///
    /// Depth 0 and 1 are one statement: the id set is a CTE built from two
    /// directed index joins (no `source = x OR target = x` join, which can
    /// only be answered by a bitmap-OR per row of the recursive CTE), and the
    /// induced edges come from index scans on `source_id` joined against the
    /// set. Deeper walks expand the frontier level by level (one round trip
    /// each, never revisiting a node) and then read the induced subgraph.
    /// With a per-seed neighbour cap at depth 1 the cap is applied in the
    /// query: each seed's non-seed neighbours are read from the edge indexes
    /// (a `LATERAL` per seed) and only the `cap` smallest ids (`COLLATE "C"`,
    /// i.e. byte order, as the trait default sorts) join the id set, so the
    /// hub fan-out never reaches the induced-subgraph join. Property
    /// narrowing, if any, is then applied client-side.
    async fn get_neighborhood_scoped(
        &self,
        node_ids: &[String],
        depth: usize,
        scope: &NeighborhoodScope,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        let (Some(cap), 1) = (scope.max_neighbors_per_seed, depth) else {
            let (nodes, edges) = self.get_neighborhood(node_ids, depth).await?;
            return Ok(apply_neighborhood_scope(
                node_ids, depth, nodes, edges, scope,
            ));
        };
        if node_ids.is_empty() {
            return Ok((vec![], vec![]));
        }
        let induced = if node_ids.len() >= SEMI_JOIN_MIN_SEEDS {
            INDUCED_SUBGRAPH_SEMI
        } else {
            INDUCED_SUBGRAPH
        };
        let sql = format!(
            "WITH seeds AS MATERIALIZED (SELECT DISTINCT unnest($1::text[]) AS id), \
             nb AS (SELECT c.nbr FROM seeds s CROSS JOIN LATERAL ( \
                 SELECT u.nbr FROM ( \
                     SELECT e.target_id AS nbr FROM graph_edge e WHERE e.source_id = s.id \
                     UNION SELECT e.source_id FROM graph_edge e WHERE e.target_id = s.id) u \
                 WHERE NOT EXISTS (SELECT 1 FROM seeds s2 WHERE s2.id = u.nbr) \
                 ORDER BY u.nbr COLLATE \"C\" LIMIT {cap}) c), \
             ids AS MATERIALIZED (SELECT id FROM seeds UNION SELECT nbr FROM nb) {induced}"
        );
        let (nodes, edges) = self
            .subgraph_query(&sql, vec![node_ids.to_vec().into()])
            .await?;
        let rest = NeighborhoodScope {
            max_neighbors_per_seed: None,
            ..scope.clone()
        };
        Ok(apply_neighborhood_scope(
            node_ids, depth, nodes, edges, &rest,
        ))
    }

    async fn get_neighborhood(
        &self,
        node_ids: &[String],
        depth: usize,
    ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
        if node_ids.is_empty() {
            return Ok((vec![], vec![]));
        }
        let ids: Vec<String> = if depth <= 1 {
            node_ids.to_vec()
        } else {
            let mut seen: HashSet<String> = node_ids.iter().cloned().collect();
            let mut frontier: Vec<String> = seen.iter().cloned().collect();
            for _ in 0..depth - 1 {
                if frontier.is_empty() {
                    break;
                }
                let rows = self
                    .query_all_tuned(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "WITH f AS (SELECT DISTINCT unnest($1::text[]) AS id) \
                         SELECT e.target_id AS id FROM graph_edge e JOIN f ON e.source_id = f.id \
                         UNION SELECT e.source_id FROM graph_edge e JOIN f ON e.target_id = f.id",
                        [frontier.clone().into()],
                    ))
                    .await
                    .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
                let mut next = Vec::new();
                for row in &rows {
                    let id: String = row
                        .try_get("", "id")
                        .map_err(|e| GraphDBError::QueryError(e.to_string()))?;
                    if seen.insert(id.clone()) {
                        next.push(id);
                    }
                }
                frontier = next;
            }
            seen.into_iter().collect()
        };
        // For depth >= 1 the final statement adds the last hop itself.
        let expand = if depth == 0 {
            ""
        } else {
            "UNION SELECT e.target_id FROM graph_edge e JOIN seeds s ON e.source_id = s.id \
             UNION SELECT e.source_id FROM graph_edge e JOIN seeds s ON e.target_id = s.id"
        };
        let induced = if ids.len() >= SEMI_JOIN_MIN_SEEDS {
            INDUCED_SUBGRAPH_SEMI
        } else {
            INDUCED_SUBGRAPH
        };
        let sql = format!(
            "WITH seeds AS (SELECT DISTINCT unnest($1::text[]) AS id), \
             ids AS MATERIALIZED (SELECT id FROM seeds {expand}) {induced}"
        );
        self.subgraph_query(&sql, vec![ids.into()]).await
    }
}

// ---------------------------------------------------------------------------
// Connected components (streaming union-find)
// ---------------------------------------------------------------------------

/// Connected components by streaming union-find: node and edge keys are
/// streamed once and unioned client-side, O(V + E) time and O(V) memory.
///
/// The two scans run in one `REPEATABLE READ` read-only transaction, so they
/// see the same snapshot. They have to: an edge whose endpoints are not both in
/// the node index is silently skipped (`if let (Some(&a), Some(&b))`, there for
/// the foreign keys that make it impossible), and under the default
/// `READ COMMITTED` each statement takes a *fresh* snapshot — so a node
/// inserted between the passes makes its edges invisible to the index and the
/// components it joined come apart. Wrapping the two statements in a plain
/// `begin()` would not have been enough for the same reason.
///
/// Consistency stops at this pass: `node_count` and `edge_count` are separate
/// statements outside it, so on a graph being written concurrently the
/// component sizes can still sum to less than the reported `node_count`.
///
/// Keys travel as 64-bit `hashtextextended` values (16 bytes per edge instead
/// of two text ids); a hash collision among node ids is detected while
/// building the index, and the pass is then redone with the ids themselves.
mod components {
    use std::collections::HashMap;
    use std::collections::hash_map::Entry;
    use std::hash::Hash;

    use futures::TryStreamExt;
    use sea_orm::{
        AccessMode, DatabaseBackend, DatabaseConnection, DatabaseTransaction, IsolationLevel,
        Statement, StreamTrait, TransactionTrait, TryGetable,
    };

    use crate::error::{GraphDBError, GraphDBResult};

    struct UnionFind {
        parent: Vec<u32>,
        size: Vec<u32>,
    }

    impl UnionFind {
        fn new(n: u32) -> Self {
            Self {
                parent: (0..n).collect(),
                size: vec![1; n as usize],
            }
        }

        fn find(&mut self, mut x: u32) -> u32 {
            while self.parent[x as usize] != x {
                let p = self.parent[x as usize];
                self.parent[x as usize] = self.parent[p as usize];
                x = p;
            }
            x
        }

        fn union(&mut self, a: u32, b: u32) {
            let (mut ra, mut rb) = (self.find(a), self.find(b));
            if ra == rb {
                return;
            }
            if self.size[ra as usize] < self.size[rb as usize] {
                std::mem::swap(&mut ra, &mut rb);
            }
            self.parent[rb as usize] = ra;
            self.size[ra as usize] += self.size[rb as usize];
        }
    }

    fn scan_err(e: impl std::fmt::Display) -> GraphDBError {
        GraphDBError::QueryError(format!("connected-components scan failed: {e}"))
    }

    /// Component sizes, largest first; `None` if two node keys collided.
    async fn sizes_by<K>(
        db: &DatabaseTransaction,
        node_sql: &str,
        edge_sql: &str,
    ) -> GraphDBResult<Option<Vec<i64>>>
    where
        K: TryGetable + Eq + Hash,
    {
        let mut index: HashMap<K, u32> = HashMap::new();
        {
            let mut nodes = db
                .stream(Statement::from_string(
                    DatabaseBackend::Postgres,
                    node_sql.to_string(),
                ))
                .await
                .map_err(scan_err)?;
            while let Some(row) = nodes.try_next().await.map_err(scan_err)? {
                let key: K = row.try_get("", "k").map_err(scan_err)?;
                let next = u32::try_from(index.len()).map_err(scan_err)?;
                match index.entry(key) {
                    Entry::Occupied(_) => return Ok(None),
                    Entry::Vacant(v) => {
                        v.insert(next);
                    }
                }
            }
        }
        let n = u32::try_from(index.len()).map_err(scan_err)?;
        let mut uf = UnionFind::new(n);
        {
            let mut edges = db
                .stream(Statement::from_string(
                    DatabaseBackend::Postgres,
                    edge_sql.to_string(),
                ))
                .await
                .map_err(scan_err)?;
            while let Some(row) = edges.try_next().await.map_err(scan_err)? {
                let s: K = row.try_get("", "s").map_err(scan_err)?;
                let t: K = row.try_get("", "t").map_err(scan_err)?;
                // Endpoints always exist (foreign keys); skip defensively.
                if let (Some(&a), Some(&b)) = (index.get(&s), index.get(&t)) {
                    uf.union(a, b);
                }
            }
        }
        let mut sizes: Vec<i64> = Vec::new();
        for i in 0..n {
            if uf.find(i) == i {
                sizes.push(i64::from(uf.size[i as usize]));
            }
        }
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        Ok(Some(sizes))
    }

    /// Sizes of the connected components of the (undirected) graph, largest
    /// first — the recursive-CTE query's `ORDER BY sz DESC` result.
    pub(super) async fn component_sizes(db: &DatabaseConnection) -> GraphDBResult<Vec<i64>> {
        if let Some(sizes) = sizes_in_snapshot::<i64>(
            db,
            "SELECT hashtextextended(id, 0) AS k FROM graph_node",
            "SELECT hashtextextended(source_id, 0) AS s, \
                    hashtextextended(target_id, 0) AS t FROM graph_edge",
        )
        .await?
        {
            return Ok(sizes);
        }
        // The hash pass found a collision among the node ids; redo it on the
        // ids themselves, in a snapshot of its own — it is a fresh read either
        // way, and the first snapshot has already been given back.
        sizes_in_snapshot::<String>(
            db,
            "SELECT id AS k FROM graph_node",
            "SELECT source_id AS s, target_id AS t FROM graph_edge",
        )
        .await?
        .ok_or_else(|| GraphDBError::QueryError("duplicate graph_node ids".to_string()))
    }

    /// [`sizes_by`] with both of its scans inside one `REPEATABLE READ`,
    /// read-only transaction, so they see the same snapshot.
    ///
    /// Read-only is not decoration: it tells Postgres this transaction can
    /// never take a write lock or hit a serialization failure, so holding a
    /// snapshot across two full table scans costs nothing but the vacuum
    /// horizon for the duration.
    async fn sizes_in_snapshot<K>(
        db: &DatabaseConnection,
        node_sql: &str,
        edge_sql: &str,
    ) -> GraphDBResult<Option<Vec<i64>>>
    where
        K: TryGetable + Eq + Hash,
    {
        let txn = db
            .begin_with_config(
                Some(IsolationLevel::RepeatableRead),
                Some(AccessMode::ReadOnly),
            )
            .await
            .map_err(scan_err)?;
        let out = sizes_by::<K>(&txn, node_sql, edge_sql).await;
        // Releasing a read-only snapshot, not undoing anything: a failure here
        // must not turn a good answer into an error.
        if let Err(e) = txn.rollback().await {
            tracing::debug!("connected-components snapshot rollback failed: {e}");
        }
        out
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
            vec![Box::new(CreateGraphTables), Box::new(GraphAutovacuum)]
        }
    }

    /// Per-table autovacuum thresholds for the graph tables: the server
    /// defaults (vacuum at 20% dead, analyze at 10% changed) leave a table
    /// that grows by cognify batches with stale statistics for most of a
    /// load, and a `graph_edge` of 500k rows accumulates 100k dead tuples
    /// before a vacuum. Reloptions only — no server setting is touched.
    struct GraphAutovacuum;

    impl MigrationName for GraphAutovacuum {
        fn name(&self) -> &str {
            "m20250930_000002_graph_autovacuum"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for GraphAutovacuum {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let conn = manager.get_connection();
            for t in ["graph_node", "graph_edge"] {
                conn.execute_unprepared(&format!(
                    "ALTER TABLE {t} SET (autovacuum_vacuum_scale_factor = 0.05, \
                     autovacuum_vacuum_insert_scale_factor = 0.05, \
                     autovacuum_analyze_scale_factor = 0.02)"
                ))
                .await?;
            }
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let conn = manager.get_connection();
            for t in ["graph_node", "graph_edge"] {
                conn.execute_unprepared(&format!(
                    "ALTER TABLE {t} RESET (autovacuum_vacuum_scale_factor, \
                     autovacuum_vacuum_insert_scale_factor, autovacuum_analyze_scale_factor)"
                ))
                .await?;
            }
            Ok(())
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
            //
            // Key columns use the "C" collation: they are only ever compared
            // for equality (ids are UUIDs / normalized identifiers, `type` and
            // `relationship_name` are labels), and every btree insert, lookup,
            // FK check and merge join on them otherwise goes through the
            // database's locale collation (`strcoll`). Measured on a 10k-node
            // store: 30.8k edge inserts 685 ms -> 382 ms, 8k node inserts
            // 75 ms -> 37 ms. Equality semantics are identical (both are
            // deterministic), so queries and other SDKs are unaffected; this
            // applies to newly created tables only (`IF NOT EXISTS`), existing
            // ones keep their collation.
            conn.execute_unprepared(
                "CREATE TABLE IF NOT EXISTS graph_node ( \
                     id         VARCHAR COLLATE \"C\" PRIMARY KEY, \
                     name       VARCHAR NOT NULL DEFAULT '', \
                     type       VARCHAR COLLATE \"C\" NOT NULL DEFAULT '', \
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
                     source_id         VARCHAR COLLATE \"C\" NOT NULL REFERENCES graph_node(id) ON DELETE CASCADE, \
                     target_id         VARCHAR COLLATE \"C\" NOT NULL REFERENCES graph_node(id) ON DELETE CASCADE, \
                     relationship_name VARCHAR COLLATE \"C\" NOT NULL, \
                     properties        JSONB NOT NULL DEFAULT '{}', \
                     created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                     updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(), \
                     PRIMARY KEY (source_id, target_id, relationship_name) \
                 )",
            )
            .await?;

            // Covering index for target-side neighbour lookups without heap
            // reads. Source-side lookups need none: the primary key
            // `(source_id, target_id, relationship_name)` already answers
            // `source_id = x` with an index-only scan returning both other
            // columns, so the former `idx_graph_edge_source_cover` duplicated
            // it and only cost every edge write a fourth btree insert. Tables
            // that already carry it keep it (harmless).
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
    /// `pub(super)` only so the sibling `session_tuning_tests` module can reuse
    /// it rather than copy it; nothing outside the test modules can see it.
    pub(super) async fn with_temp_db<F, Fut>(what: &str, body: F)
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

            // 3. The graph migrator tracks its versions in its OWN table and
            //    leaves the relational bookkeeping untouched. The expected count
            //    is read off the migrator's own list rather than written out, so
            //    adding a graph migration cannot make this assertion stale.
            assert_eq!(version_count(&db, "seaql_migrations").await, 2);
            assert_eq!(
                version_count(&db, "seaql_migrations_pggraph").await,
                // Fully qualified: `sea_orm_migration::prelude::*` brings a
                // `ValueType::try_from` for `i64` into scope too.
                <i64 as TryFrom<usize>>::try_from(super::migrator::Migrator::migrations().len())
                    .unwrap(),
            );
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

// ---------------------------------------------------------------------------
// Session-tuning tests
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod session_tuning_tests {
    use super::shared_db_migration_tests::with_temp_db;
    use super::{PLAN_CACHE_LOCAL, PgGraphAdapter};
    use crate::traits::GraphDBTrait;
    use sea_orm::{
        ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement,
    };
    use serde_json::json;

    /// The decision itself, with no server: a pool this adapter opened carries
    /// the setting in its connection options and must not pay a transaction to
    /// repeat it; one it did not open has no such hook and every parameterised
    /// statement has to bring it.
    #[test]
    fn only_a_caller_owned_connection_carries_the_setting_per_statement() {
        assert_eq!(PgGraphAdapter::tuned_locals(true), None);
        assert_eq!(
            PgGraphAdapter::tuned_locals(false),
            Some(PLAN_CACHE_LOCAL),
            "from_connection has no ConnectOptions hook, so the statements must \
             carry what new()'s pool gets as an option"
        );
    }

    /// What only a server can answer: that the `SET LOCAL` is accepted and in
    /// force for the statement, that it does not outlive it, and that every
    /// parameterised path still works now that it runs inside a transaction.
    ///
    /// The caller's pool is capped at one connection so the leak check is exact
    /// rather than probabilistic — a plain `SET` would leak to the next borrower
    /// of that connection, which in the single-shared-Postgres layout is the
    /// relational store's own queries.
    #[tokio::test]
    async fn a_caller_owned_connection_runs_its_statements_with_a_custom_plan() {
        with_temp_db(
            "a_caller_owned_connection_runs_its_statements_with_a_custom_plan",
            |url| async move {
                let mut opts = ConnectOptions::new(url.clone());
                opts.max_connections(1);
                let caller_pool: DatabaseConnection = Database::connect(opts).await.unwrap();
                let shared = PgGraphAdapter::from_connection(caller_pool.clone())
                    .await
                    .unwrap();
                assert!(
                    !shared.tuned_sessions,
                    "from_connection wraps a pool whose options it never chose"
                );

                // In force for the statement — read back through the adapter's
                // own helper, on a statement that actually has a parameter.
                let row = shared
                    .query_one_tuned(Statement::from_sql_and_values(
                        DatabaseBackend::Postgres,
                        "SELECT current_setting('plan_cache_mode') AS pc, $1::text AS echo",
                        ["probe".into()],
                    ))
                    .await
                    .unwrap()
                    .expect("one row");
                assert_eq!(
                    row.try_get::<String>("", "pc").unwrap(),
                    "force_custom_plan",
                    "without this the generic plan cannot see a bound id array \
                     and costs a neighbourhood read like any other — 10.9 ms \
                     against 1.2 ms, per new()'s measurement"
                );
                assert_eq!(row.try_get::<String>("", "echo").unwrap(), "probe");

                // And gone again: the caller's next query on that same, single
                // connection must see the server default.
                let after = caller_pool
                    .query_one(Statement::from_string(
                        DatabaseBackend::Postgres,
                        "SELECT current_setting('plan_cache_mode') AS pc".to_string(),
                    ))
                    .await
                    .unwrap()
                    .expect("one row");
                assert_eq!(
                    after.try_get::<String>("", "pc").unwrap(),
                    "auto",
                    "a setting that outlives its statement leaks to whatever \
                     borrows this pooled connection next"
                );

                // An owned pool keeps the plain single-statement path, and gets
                // the same setting from its connection options.
                let owned = PgGraphAdapter::new(&url).await.unwrap();
                assert!(owned.tuned_sessions);
                assert_eq!(
                    owned
                        .query_one_tuned(Statement::from_sql_and_values(
                            DatabaseBackend::Postgres,
                            "SELECT current_setting('plan_cache_mode') AS pc, $1::text AS echo",
                            ["probe".into()],
                        ))
                        .await
                        .unwrap()
                        .expect("one row")
                        .try_get::<String>("", "pc")
                        .unwrap(),
                    "force_custom_plan"
                );

                // Every kind of parameterised statement still answers now that
                // it runs inside a transaction: an array read, an `unnest`
                // insert, a single-row read, an edge read and an `ANY` delete.
                shared
                    .add_nodes_raw(vec![
                        json!({"id": "a", "name": "A", "type": "Entity"}),
                        json!({"id": "b", "name": "B", "type": "Entity"}),
                    ])
                    .await
                    .unwrap();
                shared.add_edge("a", "b", "knows", None).await.unwrap();
                assert!(shared.has_node("a").await.unwrap());
                assert_eq!(
                    shared
                        .get_nodes(&["a".to_string(), "b".to_string()])
                        .await
                        .unwrap()
                        .len(),
                    2
                );
                assert!(shared.get_node("a").await.unwrap().is_some());
                assert_eq!(shared.get_edges("a").await.unwrap().len(), 1);
                assert_eq!(shared.get_neighbors("a").await.unwrap().len(), 1);
                shared.delete_nodes(&["a".to_string()]).await.unwrap();
                assert!(!shared.has_node("a").await.unwrap());

                owned.close().await.unwrap();
                drop(shared);
                caller_pool.close().await.unwrap();
            },
        )
        .await;
    }
}
