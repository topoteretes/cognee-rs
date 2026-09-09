//! PGVector adapter — stores vectors in PostgreSQL via the `pgvector` extension.
//!
//! Each `(data_type, field_name)` pair maps to a dedicated PostgreSQL table with
//! columns: `id UUID PRIMARY KEY`, `vector vector(N)`, `metadata JSONB`.
//! A `_vector_collections` bookkeeping table tracks which collection tables exist.
//!
//! # Indexing
//!
//! Each collection also carries an HNSW index over its `vector` column, built at
//! [`VectorDB::create_collection`] time with the `vector_cosine_ops` opclass —
//! matching the `<=>` operator every search orders by. Without it, similarity
//! search is an exact sequential scan over the whole collection.
//!
//! Two deliberate exceptions:
//!
//! - Collections wider than [`MAX_INDEXABLE_DIMENSION`] cannot be indexed by
//!   pgvector and keep the exact scan.
//! - [`VectorDB::search_similar_filtered`] forces the exact scan, because
//!   pgvector post-filters an index scan and that would break the
//!   filter-then-limit guarantee that path exists to provide. See the comment
//!   at that call site.
//!
//! Collections created before indexing existed keep only their btree primary
//! key; [`PgVectorAdapter::create_missing_vector_indexes`] backfills them.

use async_trait::async_trait;
use sea_orm::sea_query::{
    Alias, Asterisk, Expr, Func, Iden, OnConflict, Order, PostgresQueryBuilder, Query, Table,
};
use sea_orm::{
    ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use std::collections::HashMap;
use std::fmt;
use tracing::{Span, debug, instrument, warn};
use uuid::Uuid;

use cognee_utils::sanitize::sanitize_json;
use cognee_utils::tracing_keys::{
    COGNEE_DB_ROW_COUNT, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::vector_db_trait::VectorDB;
use crate::zero_norm::{warn_zero_norm_points, warn_zero_norm_query, warn_zero_norm_query_batch};

#[cfg(test)]
#[path = "pgvector_index_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod vector_index_tests;

/// Max points per INSERT batch (300 params = 100 rows × 3 columns).
const BATCH_SIZE: usize = 100;

/// HNSW graph degree. pgvector's own default; raising it trades build time and
/// index size for recall.
const HNSW_M: u32 = 16;

/// HNSW build-time candidate list size. pgvector's own default.
const HNSW_EF_CONSTRUCTION: u32 = 64;

/// pgvector cannot index a `vector` wider than 2000 dimensions — the index
/// tuple would not fit in a page. `text-embedding-3-large` at 3072 is over the
/// line, so a collection that wide keeps the exact scan and says so, rather
/// than failing `create_collection` outright.
const MAX_INDEXABLE_DIMENSION: usize = 2000;

/// pgvector's own default `hnsw.ef_search`.
///
/// Load-bearing, not decorative: an HNSW index scan returns **at most**
/// `ef_search` tuples and then ends, so a `LIMIT` above it is silently unmet —
/// no error, just fewer rows. cognee's retrieval paths ask for
/// `DEFAULT_WIDE_SEARCH_TOP_K = 100`, so leaving this at 40 would quietly drop
/// 60% of every graph-completion, triplet and temporal seed set.
const HNSW_EF_SEARCH_DEFAULT: usize = 40;

/// Largest `hnsw.ef_search` pgvector accepts. Beyond this a search cannot be
/// made to return `top_k` rows by raising `ef_search`, so those queries fall
/// back to the exact scan — see [`PgVectorAdapter::ann_search_locals`].
const HNSW_EF_SEARCH_MAX: usize = 1000;

/// Postgres truncates any identifier past this many bytes (`NAMEDATALEN - 1`).
const PG_MAX_IDENTIFIER_BYTES: usize = 63;

// ---------------------------------------------------------------------------
// Table / column identifiers for sea_query (`_vector_collections`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum VColl {
    Table,
    CollectionName,
    DataType,
    FieldName,
    Dimension,
}

impl Iden for VColl {
    #[allow(
        clippy::expect_used,
        reason = "writing a static &str into the fmt::Write sink is infallible"
    )]
    fn unquoted(&self, s: &mut dyn fmt::Write) {
        write!(
            s,
            "{}",
            match self {
                Self::Table => "_vector_collections",
                Self::CollectionName => "collection_name",
                Self::DataType => "data_type",
                Self::FieldName => "field_name",
                Self::Dimension => "dimension",
            }
        )
        .expect("write to string cannot fail");
    }
}

/// Migration version recorded by this adapter's migrator (see `migrator`).
///
/// Older builds tracked this in the default `seaql_migrations`; newer builds use
/// `seaql_migrations_pgvector`. The constant is used to purge the stale legacy
/// row during init — see [`cleanup_legacy_seaql_migrations`].
const PGVECTOR_MIGRATION_VERSION: &str = "m20250101_000001_create_pgvector_extension";

/// Remove this adapter's stale bookkeeping row from the *default*
/// `seaql_migrations` table that older builds may have left behind.
///
/// # Why
/// This adapter now tracks its migrations in `seaql_migrations_pgvector`. In an
/// "everything in one Postgres" deployment the core/relational migrator owns the
/// default `seaql_migrations`. If an older build had recorded
/// [`PGVECTOR_MIGRATION_VERSION`] there, the core migrator would treat it as a
/// foreign "applied but its file is missing" version and abort. We delete only
/// the version this adapter itself defines — never a core/relational version — so
/// the operation is safe and idempotent. Guarded by `to_regclass` so it is a
/// no-op on fresh installs where the default table does not (yet) exist.
///
/// # Residual
/// This only helps when this adapter initialises. If the core migrator runs
/// *first* against a DB that still holds the legacy row it aborts before this
/// cleanup can run; such a DB needs a one-time manual
/// `DELETE FROM seaql_migrations WHERE version = 'm20250101_000001_create_pgvector_extension'`.
async fn cleanup_legacy_seaql_migrations(db: &DatabaseConnection) -> VectorDBResult<()> {
    // `PGVECTOR_MIGRATION_VERSION` is a compile-time constant with no user input,
    // so inlining it into the DO block carries no injection risk.
    let sql = format!(
        "DO $$ BEGIN \
             IF to_regclass('seaql_migrations') IS NOT NULL THEN \
                 DELETE FROM seaql_migrations WHERE version = '{PGVECTOR_MIGRATION_VERSION}'; \
             END IF; \
         END $$;"
    );
    db.execute_unprepared(&sql).await.map_err(|e| {
        VectorDBError::StorageError(format!("PGVector legacy migration cleanup failed: {e}"))
    })?;
    Ok(())
}

/// Vector database backed by PostgreSQL + pgvector extension.
///
/// Requires a PostgreSQL instance with the `vector` extension installed (the
/// adapter will attempt `CREATE EXTENSION IF NOT EXISTS vector` on startup).
pub struct PgVectorAdapter {
    db: DatabaseConnection,
    dimension: usize,
    /// Whether this adapter opened `db` itself and may therefore close it.
    ///
    /// Load-bearing, not defensive: [`Self::from_connection`] wraps a connection
    /// the *caller* owns, and in the single-shared-Postgres layout that caller is
    /// the relational store. Closing it from a vector teardown would turn a leak
    /// fix into an outage. Neither in-tree factory takes that path today, but
    /// both constructors are public API.
    owns_pool: bool,
}

impl PgVectorAdapter {
    /// Connect to an existing PostgreSQL database and run pgvector migrations.
    ///
    /// The database must already exist. Use [`Self::from_connection`] to share
    /// a connection that was established elsewhere (e.g. by the database crate).
    ///
    /// # Arguments
    /// * `database_url` — Postgres connection string, e.g.
    ///   `postgres://user:pass@localhost:5432/mydb`
    /// * `dimension` — default vector dimension (e.g. 384 for BGE-Small)
    pub async fn new(database_url: &str, dimension: usize) -> VectorDBResult<Self> {
        let db = Database::connect(database_url)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector connect failed: {e}")))?;

        cleanup_legacy_seaql_migrations(&db).await?;
        migrator::Migrator::up(&db, None)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector migration failed: {e}")))?;

        debug!("PgVectorAdapter initialised (dimension={dimension})");
        Ok(Self {
            db,
            dimension,
            owns_pool: true,
        })
    }

    /// Wrap an existing SeaORM `DatabaseConnection` (must be Postgres).
    ///
    /// The caller is responsible for ensuring the database already exists
    /// (the connection proves it does). Only the pgvector extension and
    /// bookkeeping table are created if missing.
    pub async fn from_connection(db: DatabaseConnection, dimension: usize) -> VectorDBResult<Self> {
        cleanup_legacy_seaql_migrations(&db).await?;
        migrator::Migrator::up(&db, None)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector migration failed: {e}")))?;

        Ok(Self {
            db,
            dimension,
            owns_pool: false,
        })
    }

    /// Close this adapter's **own** Postgres pool, so its server-side backends go
    /// away now rather than whenever the last `Arc` happens to be dropped.
    ///
    /// The vector twin of `PgGraphAdapter::close`; that method carries the full
    /// measurement table. The short version: a drop of an *idle* pool does drain
    /// (in ~4 ms), so the leak is not "drop never works" — it is that (a) a
    /// retained `Arc` never gets dropped at all, which is exactly the HTTP
    /// server's `AppState` shape and leaves 10 backends open for the life of the
    /// process, and (b) with one query in flight a drop pins the entire pool until
    /// that query finishes, where `close_by_ref` reclaims the idle connections
    /// immediately. This adapter's pool is separate from both the relational pool
    /// and the graph adapter's — a warm `ComponentManager` on Postgres holds
    /// three.
    ///
    /// A **no-op when the connection came from [`Self::from_connection`]**, and
    /// idempotent.
    pub async fn close(&self) -> VectorDBResult<()> {
        if !self.owns_pool {
            debug!("PgVectorAdapter::close is a no-op for a caller-owned connection");
            return Ok(());
        }
        self.db
            .close_by_ref()
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector pool close failed: {e}")))
    }

    /// Returns the default vector dimension this adapter was configured with.
    pub fn dimension(&self) -> usize {
        self.dimension
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
    // -- helpers ----------------------------------------------------------

    /// Build a SeaORM [`Statement`] from a `sea_query` query.
    fn build<S: sea_orm::StatementBuilder>(&self, query: &S) -> Statement {
        self.db.get_database_backend().build(query)
    }

    /// Build a validated table name from a `(data_type, field_name)` pair.
    ///
    /// Returns an error if the resulting name contains characters outside
    /// `[a-zA-Z0-9_]`, preventing SQL injection in dynamic DDL.
    fn collection_name(data_type: &str, field_name: &str) -> VectorDBResult<String> {
        let name = format!("{data_type}_{field_name}");
        Self::validate_identifier(&name)?;
        Ok(name)
    }

    /// Reject identifiers that could cause SQL-injection via dynamic DDL.
    fn validate_identifier(name: &str) -> VectorDBResult<()> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(VectorDBError::StorageError(format!(
                "Invalid identifier: {name}"
            )));
        }
        Ok(())
    }

    /// Name of the HNSW index over a collection's `vector` column.
    ///
    /// Derived from the collection name, which [`Self::validate_identifier`] has
    /// already restricted to `[A-Za-z0-9_]`, so it is safe to interpolate.
    ///
    /// Truncated to [`PG_MAX_IDENTIFIER_BYTES`] here, because Postgres would
    /// truncate it anyway when creating the index — and then
    /// [`Self::vector_index_state`], which compares the name as *data* against
    /// `pg_class.relname`, would be looking for the untruncated string and never
    /// match. The consequences of that mismatch are not cosmetic: the backfill
    /// would re-issue `CREATE INDEX` and re-count it on every run, and an index
    /// left invalid by an interrupted build would report as absent, so it would
    /// never be dropped or rebuilt. Slicing bytes is safe because the name is
    /// ASCII by construction.
    ///
    /// Two collections whose names agree in their first 51 characters therefore
    /// collide on one index name. That is inherited from the collection names
    /// themselves — they are table names under the same limit — so it is not
    /// introduced here.
    fn vector_index_name(coll: &str) -> String {
        let mut name = format!("{coll}_vector_hnsw");
        name.truncate(PG_MAX_IDENTIFIER_BYTES);
        name
    }

    /// `SET LOCAL` statements that make an ANN search actually return `top_k`
    /// rows, as one semicolon-separated string (one round trip).
    ///
    /// An HNSW index scan stops after `ef_search` tuples, so `ef_search` must be
    /// at least `top_k` or the `LIMIT` is silently unmet. Past
    /// [`HNSW_EF_SEARCH_MAX`] that lever runs out, and the only way left to
    /// guarantee `top_k` rows is to not use the index — which is the right
    /// trade at that size, since such a query is scanning most of the
    /// collection regardless.
    fn ann_search_locals(top_k: usize) -> String {
        if top_k > HNSW_EF_SEARCH_MAX {
            return Self::exact_scan_locals().to_string();
        }
        let ef = top_k.max(HNSW_EF_SEARCH_DEFAULT);
        format!("SET LOCAL hnsw.ef_search = {ef}")
    }

    /// `SET LOCAL` statements that force an exact scan, for the paths whose
    /// correctness depends on it.
    fn exact_scan_locals() -> &'static str {
        "SET LOCAL enable_indexscan = off; SET LOCAL enable_bitmapscan = off"
    }

    /// Run `stmt` in a transaction with `locals` applied first.
    ///
    /// The transaction is what makes `SET LOCAL` mean anything — outside one it
    /// is a no-op with a warning — and it scopes the settings to this statement
    /// so they cannot leak to the next borrower of a pooled connection. `locals`
    /// goes over the simple-query protocol, so several `SET LOCAL`s cost one
    /// round trip rather than one each.
    async fn query_all_with_locals(
        &self,
        locals: &str,
        stmt: Statement,
    ) -> VectorDBResult<Vec<sea_orm::QueryResult>> {
        let txn = self
            .db
            .begin()
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        txn.execute_unprepared(locals)
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        let rows = txn
            .query_all(stmt)
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        txn.commit()
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        Ok(rows)
    }

    /// Create the HNSW index over `coll`'s `vector` column, if the collection is
    /// narrow enough to index.
    ///
    /// The opclass is `vector_cosine_ops` because every search site orders by
    /// the cosine operator `<=>`; an opclass that does not match the operator in
    /// the `ORDER BY` is simply never used by the planner, which would leave the
    /// sequential scan in place while looking like it had been fixed.
    ///
    /// `concurrently` builds without taking a write lock, at the cost of not
    /// being runnable inside a transaction. Pass `false` from
    /// [`VectorDB::create_collection`], where the table is empty and the lock is
    /// free; pass `true` from [`Self::create_missing_vector_indexes`], which
    /// backfills tables that already hold rows and may be serving traffic.
    async fn create_vector_index(
        db: &DatabaseConnection,
        coll: &str,
        dimension: usize,
        concurrently: bool,
    ) -> VectorDBResult<bool> {
        if dimension > MAX_INDEXABLE_DIMENSION {
            debug!(
                "collection {coll} is {dimension}-dimensional, over pgvector's \
                 {MAX_INDEXABLE_DIMENSION}-d index ceiling — keeping the exact scan"
            );
            return Ok(false);
        }

        let index = Self::vector_index_name(coll);
        let concurrent_kw = if concurrently { " CONCURRENTLY" } else { "" };
        let ddl = format!(
            r#"CREATE INDEX{concurrent_kw} IF NOT EXISTS "{index}"
               ON "{coll}" USING hnsw (vector vector_cosine_ops)
               WITH (m = {HNSW_M}, ef_construction = {HNSW_EF_CONSTRUCTION})"#
        );

        db.execute_unprepared(&ddl)
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        // "ensured", not "created": the statement carries `IF NOT EXISTS`, so it
        // is a no-op when the index is already there — including when two
        // backfills race — and claiming a creation would misreport that.
        debug!("ensured HNSW index {index} on {coll} (dim={dimension})");
        Ok(true)
    }

    /// State of the index named `index`: `None` if it does not exist, else
    /// whether Postgres considers it valid.
    ///
    /// Validity is the part that matters for the backfill. A `CREATE INDEX
    /// CONCURRENTLY` that fails or is interrupted leaves the index in place but
    /// marked invalid; the planner ignores an invalid index, and
    /// `CREATE INDEX ... IF NOT EXISTS` sees the *name* and does nothing — so a
    /// presence-only check would skip it on every subsequent run and the
    /// collection would keep its sequential scan forever, while looking indexed.
    /// The caller drops an invalid index and rebuilds it.
    ///
    /// Matches on `pg_class.relname`, which compares the name as *data*. The
    /// obvious alternative, `to_regclass($1)`, parses its argument as an SQL
    /// identifier and case-folds an unquoted one, so it would look for
    /// `idx_f_vector_hnsw` and never match the `"Idx_f_vector_hnsw"` that was
    /// actually created — collection names carry their case, coming from a
    /// `data_type` like `DocumentChunk`.
    ///
    /// Restricted to `current_schema()`, because `relname` alone matches across
    /// every schema in the database while every other statement here is
    /// unqualified and so `search_path`-relative. Without the filter, two cognee
    /// installs in one database (the shared single-database deployment) would
    /// interfere: schema A's valid index would make this report `Some(true)` for
    /// schema B, leaving B on the sequential scan — and worse, an *invalid*
    /// index in A would report `Some(false)`, so the caller's unqualified
    /// `DROP INDEX` would resolve through `search_path` and drop B's valid one.
    /// `current_schema()` is the first entry of `search_path`, which is exactly
    /// what the unqualified DDL resolves to, so probe and DDL agree.
    async fn vector_index_state(
        db: &DatabaseConnection,
        index: &str,
    ) -> VectorDBResult<Option<bool>> {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT i.indisvalid AS valid
                   FROM pg_class c
                   JOIN pg_index i ON i.indexrelid = c.oid
                  WHERE c.relname = $1
                    AND c.relnamespace = current_schema()::regnamespace",
                [index.into()],
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        match row {
            Some(row) => row
                .try_get::<bool>("", "valid")
                .map(Some)
                .map_err(|e| VectorDBError::StorageError(e.to_string())),
            None => Ok(None),
        }
    }

    /// Build the HNSW index on every registered collection that does not have
    /// one yet, and report how many were created.
    ///
    /// [`VectorDB::create_collection`] indexes new collections, but it guards on
    /// `has_collection`, so it never runs for a collection that already exists.
    /// Every collection created before this change therefore still has only its
    /// btree primary key, and stays on the sequential scan until this runs once.
    ///
    /// Uses `CREATE INDEX CONCURRENTLY`, so it does not block writes and cannot
    /// be called inside a transaction — which is also why it is not a migration.
    /// Idempotent and safe to call on every startup, but building an HNSW index
    /// over a large collection is expensive, so the caller decides when.
    ///
    /// The returned count is indexes this call actually built: collections that
    /// already had one, and collections wider than [`MAX_INDEXABLE_DIMENSION`],
    /// are skipped and not counted.
    ///
    /// A collection that fails is logged and skipped rather than aborting the
    /// run, so one bad entry cannot leave every collection after it unindexed.
    /// That is reachable without any corruption: `delete_collection` drops the
    /// table before deleting its bookkeeping row and not in one transaction, so
    /// an interrupted delete leaves an orphan row whose `CREATE INDEX` fails
    /// with `relation does not exist`.
    pub async fn create_missing_vector_indexes(&self) -> VectorDBResult<usize> {
        let query = Query::select()
            .columns([VColl::CollectionName, VColl::Dimension])
            .from(VColl::Table)
            .to_owned();

        let rows = self
            .db
            .query_all(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        let mut created = 0usize;
        for row in &rows {
            let coll: String = row
                .try_get("", "collection_name")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            let dimension: i32 = row
                .try_get("", "dimension")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

            // A row in the bookkeeping table came from `create_collection`, which
            // validated the name before creating the table — but re-validate
            // rather than trust the table, since this name is interpolated into
            // DDL and the table is reachable by anything with the connection.
            Self::validate_identifier(&coll)?;

            // Check first rather than leaning on `IF NOT EXISTS`, so the count
            // reports work actually done and an already-indexed collection is
            // not handed a redundant CONCURRENTLY build.
            let index = Self::vector_index_name(&coll);
            let state = match Self::vector_index_state(&self.db, &index).await {
                Ok(state) => state,
                Err(e) => {
                    warn!("could not read index state for collection {coll}, skipping it: {e}");
                    continue;
                }
            };

            if state == Some(true) {
                continue;
            }

            // `Some(false)` is an index left behind by an interrupted
            // CONCURRENTLY build. `IF NOT EXISTS` would refuse to replace it, so
            // drop it and rebuild — otherwise this collection can never become
            // indexed.
            if state == Some(false) {
                debug!("dropping invalid index {index} left by a failed build, rebuilding");
                if let Err(e) = self
                    .db
                    .execute_unprepared(&format!(r#"DROP INDEX CONCURRENTLY "{index}""#))
                    .await
                {
                    warn!("could not drop invalid index {index}, skipping {coll}: {e}");
                    continue;
                }
            }

            match Self::create_vector_index(&self.db, &coll, dimension.max(0) as usize, true).await
            {
                Ok(true) => created += 1,
                Ok(false) => {}
                Err(e) => warn!("could not index collection {coll}, skipping it: {e}"),
            }
        }

        Ok(created)
    }

    /// Format a vector as pgvector text literal: `[1.0,2.0,3.0]`
    fn format_vector(v: &[f32]) -> String {
        let inner: String = v
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!("[{inner}]")
    }

    /// Build the `belongs_to_set` NodeSet-membership `WHERE` fragment (over the
    /// `metadata` JSONB column) plus the ordered name parameters, numbered from
    /// `$first_param`. `names` must be non-empty. Names are bound as parameters
    /// — never interpolated — so caller-supplied node names cannot inject SQL.
    ///
    /// Semantics mirror [`crate::node_filter::metadata_matches_node_filter`]:
    /// each element's set-name is a bare string as-is, or an object's `"name"`
    /// field, or nothing; `"AND"` requires the requested names to be a subset of
    /// the row's names, anything else is `"OR"` (non-empty intersection). The
    /// `CASE ... ELSE '[]'::jsonb` fallback keeps `jsonb_array_elements` from
    /// erroring when `belongs_to_set` is missing or not an array — such a row
    /// then yields an empty name-set and matches nothing, exactly as the
    /// in-memory predicate returns `false` for it.
    fn node_filter_where(
        names: &[String],
        operator: &str,
        first_param: usize,
    ) -> (String, Vec<sea_orm::Value>) {
        // Set-name of one `belongs_to_set` element.
        let elem_name = "(CASE jsonb_typeof(elem.v) \
             WHEN 'string' THEN elem.v #>> '{}' \
             WHEN 'object' THEN elem.v ->> 'name' \
             ELSE NULL END)";
        // Array-guarded element expansion (never errors on non-array values).
        let elements = "jsonb_array_elements(\
             CASE WHEN jsonb_typeof(metadata->'belongs_to_set') = 'array' \
                  THEN metadata->'belongs_to_set' ELSE '[]'::jsonb END) AS elem(v)";

        let placeholders: Vec<String> = (0..names.len())
            .map(|i| format!("${}::text", first_param + i))
            .collect();
        let values: Vec<sea_orm::Value> = names.iter().map(|n| n.clone().into()).collect();

        let where_sql = if operator == "AND" {
            // Requested ⊆ payload: no requested name is absent from the row.
            format!(
                "NOT EXISTS (SELECT 1 FROM unnest(ARRAY[{}]) AS req(name) \
                 WHERE NOT EXISTS (SELECT 1 FROM {elements} WHERE {elem_name} = req.name))",
                placeholders.join(", ")
            )
        } else {
            // OR: non-empty intersection.
            format!(
                "EXISTS (SELECT 1 FROM {elements} WHERE {elem_name} = ANY(ARRAY[{}]))",
                placeholders.join(", ")
            )
        };
        (where_sql, values)
    }

    /// Fetch the current `metadata` JSONB for the given points (by id) from
    /// `coll`, keyed by id. Used to union dataset membership before an upsert so
    /// re-indexing a content-addressed point under a new dataset does not drop
    /// the datasets it already belonged to.
    async fn fetch_metadata(
        &self,
        coll: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<HashMap<Uuid, HashMap<String, serde_json::Value>>> {
        let mut out: HashMap<Uuid, HashMap<String, serde_json::Value>> = HashMap::new();
        if points.is_empty() {
            return Ok(out);
        }
        let placeholders: Vec<String> = (1..=points.len()).map(|i| format!("${i}::uuid")).collect();
        let sql = format!(
            r#"SELECT id, metadata FROM "{coll}" WHERE id IN ({})"#,
            placeholders.join(", ")
        );
        let values: Vec<sea_orm::Value> = points.iter().map(|p| p.id.into()).collect();
        let rows = self
            .db
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        for row in &rows {
            let id: Uuid = row
                .try_get("", "id")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            let metadata_val: serde_json::Value = row
                .try_get("", "metadata")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            if let serde_json::Value::Object(map) = metadata_val {
                out.insert(id, map.into_iter().collect());
            }
        }
        Ok(out)
    }

    /// Decode one `(id, score, metadata)` query row into a [`SearchResult`].
    /// Shared by `search_similar` and `batch_search_similar` so the metadata
    /// decode and `score as f32` cast live in one place.
    fn row_to_search_result(row: &sea_orm::QueryResult) -> VectorDBResult<SearchResult> {
        let id: Uuid = row
            .try_get("", "id")
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        let score: f64 = row
            .try_get("", "score")
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        let metadata_val: serde_json::Value = row
            .try_get("", "metadata")
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        let metadata = match metadata_val {
            serde_json::Value::Object(map) => map
                .into_iter()
                .collect::<HashMap<String, serde_json::Value>>(),
            _ => HashMap::new(),
        };
        Ok(SearchResult {
            id,
            score: score as f32,
            metadata,
        })
    }

    /// Decode one `(id, metadata)` retrieve row into a [`SearchResult`],
    /// always setting `score: 0.0`. `retrieve` is a direct fetch (not a
    /// similarity search), so the score is a placeholder — matching Python's
    /// `ScoredResult(score=0)`. A separate decoder from `row_to_search_result`
    /// because the retrieve query intentionally does not select a `score`
    /// column (no fake `0 AS score` is added just to reuse the other decoder).
    fn row_to_retrieve_result(row: &sea_orm::QueryResult) -> VectorDBResult<SearchResult> {
        let id: Uuid = row
            .try_get("", "id")
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        let metadata_val: serde_json::Value = row
            .try_get("", "metadata")
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        let metadata = match metadata_val {
            serde_json::Value::Object(map) => map
                .into_iter()
                .collect::<HashMap<String, serde_json::Value>>(),
            _ => HashMap::new(),
        };
        Ok(SearchResult {
            id,
            score: 0.0,
            metadata,
        })
    }
}

#[async_trait]
impl VectorDB for PgVectorAdapter {
    /// Delegates to the inherent [`PgVectorAdapter::close`], so a holder of an
    /// `Arc<dyn VectorDB>` can release the pool without downcasting.
    async fn close(&self) -> VectorDBResult<()> {
        PgVectorAdapter::close(self).await
    }

    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        let coll = Self::collection_name(data_type, field_name)?;

        if self.has_collection(data_type, field_name).await? {
            return Err(VectorDBError::CollectionExists(coll));
        }

        // Create the vector table.
        let ddl = format!(
            r#"CREATE TABLE "{coll}" (
                id UUID PRIMARY KEY,
                vector vector({dimension}),
                metadata JSONB NOT NULL DEFAULT '{{}}'
            )"#
        );
        self.db
            .execute_unprepared(&ddl)
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        // Build the ANN index while the table is empty, which is why HNSW and
        // not IVFFlat: IVFFlat derives its centroids from existing rows, so it
        // cannot be built here and would need a rebuild policy. HNSW builds
        // incrementally, so an empty-table CREATE INDEX is instant and takes no
        // meaningful lock.
        //
        // Best-effort on purpose. `CREATE TABLE` above has no `IF NOT EXISTS`
        // and `has_collection` reads only the bookkeeping table, so propagating
        // an error here would leave the table created and unregistered — and
        // every retry would then fail at `CREATE TABLE` with `already exists`,
        // wedging that collection permanently. An index failure (pgvector too
        // old to have the `hnsw` access method, a restricted role, no
        // `maintenance_work_mem`) must degrade to the sequential scan, which is
        // exactly the behaviour before this index existed. The backfill picks it
        // up later.
        if let Err(e) = Self::create_vector_index(&self.db, &coll, dimension, false).await {
            warn!(
                "collection {coll} was created without an ANN index, so its searches will \
                 be sequential scans until create_missing_vector_indexes() runs: {e}"
            );
        }

        // Register in bookkeeping table.
        let insert = Query::insert()
            .into_table(VColl::Table)
            .columns([
                VColl::CollectionName,
                VColl::DataType,
                VColl::FieldName,
                VColl::Dimension,
            ])
            .values_panic([
                coll.clone().into(),
                data_type.to_string().into(),
                field_name.to_string().into(),
                (dimension as i32).into(),
            ])
            .on_conflict(
                OnConflict::column(VColl::CollectionName)
                    .do_nothing()
                    .to_owned(),
            )
            .to_owned();

        self.db
            .execute(self.build(&insert))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        debug!("created collection {coll} (dim={dimension})");
        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let coll = Self::collection_name(data_type, field_name)?;

        let inner = Query::select()
            .expr(Expr::val(1))
            .from(VColl::Table)
            .and_where(Expr::col(VColl::CollectionName).eq(coll))
            .to_owned();

        let query = Query::select()
            .expr_as(Expr::exists(inner), Alias::new("exists"))
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        match row {
            Some(r) => {
                let exists: bool = r
                    .try_get("", "exists")
                    .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
                Ok(exists)
            }
            None => Ok(false),
        }
    }

    #[instrument(
        name = "cognee.db.vector.upsert",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        if points.is_empty() {
            return Ok(());
        }

        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());

        // Dimension check.
        let expected_dim = points[0].vector.len();
        for p in points {
            if p.vector.len() != expected_dim {
                return Err(VectorDBError::DimensionMismatch {
                    collection: coll.clone(),
                    expected: expected_dim,
                    actual: p.vector.len(),
                });
            }
        }

        // `vector <=> $1` is NaN when either operand has zero norm, so these
        // rows score and sort unusably once written.
        warn_zero_norm_points("pgvector", &coll, points);

        // Batch upsert in chunks to stay within parameter limits.
        for chunk in points.chunks(BATCH_SIZE) {
            // Point IDs are content-addressed, so the same point is re-indexed
            // once per dataset. A plain `metadata = EXCLUDED.metadata` overwrite
            // would drop earlier datasets' `dataset_id` (cross-dataset dedup
            // bug). Read the existing rows' membership and union it into the
            // incoming points before upserting, mirroring the in-memory /
            // lancedb adapters and Python's union semantics.
            let existing = self.fetch_metadata(&coll, chunk).await?;

            let mut sql = format!(r#"INSERT INTO "{coll}" (id, vector, metadata) VALUES "#);
            let mut values: Vec<sea_orm::Value> = Vec::with_capacity(chunk.len() * 3);
            let mut idx = 1u32;

            for (i, pt) in chunk.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!(
                    "(${}, ${}::vector, ${}::jsonb)",
                    idx,
                    idx + 1,
                    idx + 2
                ));
                idx += 3;

                let mut merged = pt.clone();
                if let Some(prev_meta) = existing.get(&pt.id) {
                    let prev = VectorPoint {
                        id: pt.id,
                        vector: Vec::new(),
                        metadata: prev_meta.clone(),
                    };
                    merged.merge_dataset_membership(&prev);
                }

                values.push(pt.id.into());
                values.push(Self::format_vector(&pt.vector).into());
                // Chunk and summary text is injected into point metadata, so
                // this `jsonb` cast has the same NUL exposure as the graph
                // tables — see `cognee_utils::sanitize`.
                let metadata_obj: serde_json::Value = sanitize_json(serde_json::Value::Object(
                    merged
                        .metadata
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ));
                values.push(metadata_obj.into());
            }

            sql.push_str(
                " ON CONFLICT (id) DO UPDATE SET vector = EXCLUDED.vector, metadata = EXCLUDED.metadata",
            );

            self.db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    &sql,
                    values,
                ))
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        }

        Span::current().record(COGNEE_DB_ROW_COUNT, points.len() as i64);
        Ok(())
    }

    #[instrument(
        name = "cognee.db.vector.upsert_raw",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn upsert_raw_vectors(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        // Empty input is a no-op — must not touch `points[0]`.
        if points.is_empty() {
            return Ok(());
        }

        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());
        // Raw upsert writes system-owned collections (TruthCentroid_vector and
        // friends); a zero-norm centroid stored here is unsearchable too.
        warn_zero_norm_points("pgvector", &coll, points);

        // Dimension check across the batch.
        let expected_dim = points[0].vector.len();
        for p in points {
            if p.vector.len() != expected_dim {
                return Err(VectorDBError::DimensionMismatch {
                    collection: coll.clone(),
                    expected: expected_dim,
                    actual: p.vector.len(),
                });
            }
        }

        // Self-create the collection when absent, sized from the first vector
        // (nothing else ever creates a system-owned collection like
        // TruthCentroid_vector).
        if !self.has_collection(data_type, field_name).await? {
            self.create_collection(data_type, field_name, expected_dim)
                .await?;
        }

        // Batched upsert. Unlike `index_points`, we do NOT read + union prior
        // dataset membership via `fetch_metadata`; the incoming metadata is
        // written verbatim (full replace on conflict).
        for chunk in points.chunks(BATCH_SIZE) {
            let mut sql = format!(r#"INSERT INTO "{coll}" (id, vector, metadata) VALUES "#);
            let mut values: Vec<sea_orm::Value> = Vec::with_capacity(chunk.len() * 3);
            let mut idx = 1u32;

            for (i, pt) in chunk.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!(
                    "(${}, ${}::vector, ${}::jsonb)",
                    idx,
                    idx + 1,
                    idx + 2
                ));
                idx += 3;

                values.push(pt.id.into());
                values.push(Self::format_vector(&pt.vector).into());
                let metadata_obj: serde_json::Value = sanitize_json(serde_json::Value::Object(
                    pt.metadata
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ));
                values.push(metadata_obj.into());
            }

            sql.push_str(
                " ON CONFLICT (id) DO UPDATE SET vector = EXCLUDED.vector, metadata = EXCLUDED.metadata",
            );

            self.db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    &sql,
                    values,
                ))
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        }

        Span::current().record(COGNEE_DB_ROW_COUNT, points.len() as i64);
        Ok(())
    }

    #[instrument(
        name = "cognee.db.vector.search",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.vector.result_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());
        warn_zero_norm_query("pgvector", &coll, query_vector);

        let vec_str = Self::format_vector(query_vector);

        // cosine distance `<=>` returns 0..2 (0 = identical).
        // Convert to similarity: score = 1 - distance.
        let sql = format!(
            r#"SELECT id, 1 - (vector <=> $1::vector) AS score, metadata
               FROM "{coll}"
               ORDER BY vector <=> $1::vector
               LIMIT $2"#
        );

        // `ef_search` must cover `top_k`, or the index scan ends early and the
        // LIMIT is silently unmet — see `ann_search_locals`.
        let rows = self
            .query_all_with_locals(
                &Self::ann_search_locals(top_k),
                Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    &sql,
                    [vec_str.into(), (top_k as i64).into()],
                ),
            )
            .await?;

        let mut results = Vec::with_capacity(rows.len());
        for row in &rows {
            results.push(Self::row_to_search_result(row)?);
        }

        Span::current().record(COGNEE_VECTOR_RESULT_COUNT, results.len() as i64);
        Ok(results)
    }

    #[instrument(
        name = "cognee.db.vector.search_filtered",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.vector.result_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn search_similar_filtered(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
        node_name: Option<&[String]>,
        node_name_filter_operator: &str,
    ) -> VectorDBResult<Vec<SearchResult>> {
        // No filter requested — identical to the unfiltered similarity search.
        let requested: &[String] = match node_name {
            Some(names) if !names.is_empty() => names,
            _ => {
                return self
                    .search_similar(data_type, field_name, query_vector, top_k)
                    .await;
            }
        };

        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());
        warn_zero_norm_query("pgvector", &coll, query_vector);

        let vec_str = Self::format_vector(query_vector);
        // $1 = query vector, $2 = top_k, $3.. = requested node names. Pushing the
        // NodeSet predicate into the WHERE clause makes the ORDER BY distance
        // LIMIT run *after* the filter (server-side filter-then-limit), so every
        // returned row is in-set and none is crowded out — exact at any size.
        let (where_sql, name_values) =
            Self::node_filter_where(requested, node_name_filter_operator, 3);

        let sql = format!(
            r#"SELECT id, 1 - (vector <=> $1::vector) AS score, metadata
               FROM "{coll}"
               WHERE {where_sql}
               ORDER BY vector <=> $1::vector
               LIMIT $2"#
        );

        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(2 + name_values.len());
        values.push(vec_str.into());
        values.push((top_k as i64).into());
        values.extend(name_values);

        // Deliberately keep the HNSW index out of this one query.
        //
        // pgvector post-filters an index scan: the index yields roughly
        // `hnsw.ef_search` (default 40) candidates by distance alone, and the
        // `WHERE` clause is applied to *those*. A selective NodeSet filter then
        // returns fewer than `top_k` rows, or none at all — the exact opposite
        // of the filter-then-limit guarantee documented above, which
        // `test_search_similar_filtered_filter_then_limit` pins with 64
        // zero-distance out-of-set rows crowding 2 in-set ones.
        //
        // Disabling index and bitmap scans for the statement forces the exact
        // scan that guarantee needs. The unfiltered paths — `search_similar` and
        // `batch_search_similar`, which is where the volume is — are untouched
        // and do use the index. Trading this exactness for indexed filtered
        // search is possible (pgvector 0.8's `hnsw.iterative_scan`, or reshaping
        // the predicate into something GIN-indexable) but it is a behaviour
        // change that needs its own decision, not a side effect of adding an
        // index.
        let rows = self
            .query_all_with_locals(
                Self::exact_scan_locals(),
                Statement::from_sql_and_values(DatabaseBackend::Postgres, &sql, values),
            )
            .await?;

        let mut results = Vec::with_capacity(rows.len());
        for row in &rows {
            results.push(Self::row_to_search_result(row)?);
        }

        Span::current().record(COGNEE_VECTOR_RESULT_COUNT, results.len() as i64);
        Ok(results)
    }

    #[instrument(
        name = "cognee.db.vector.retrieve",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.vector.result_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn retrieve(
        &self,
        data_type: &str,
        field_name: &str,
        ids: &[Uuid],
    ) -> VectorDBResult<Vec<SearchResult>> {
        let coll = Self::collection_name(data_type, field_name)?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());

        // Missing collection → empty (deliberate Python-parity divergence from
        // search_similar/delete_points/collection_size; see the trait
        // doc-comment on `retrieve`). Prefer the explicit pre-check over
        // parsing a Postgres "relation does not exist" error.
        if !self.has_collection(data_type, field_name).await? {
            return Ok(vec![]);
        }

        // Chunk the IN-list by BATCH_SIZE (parameter-count safety), mirroring
        // the placeholder-building shape used by `fetch_metadata`.
        let mut results = Vec::new();
        for chunk in ids.chunks(BATCH_SIZE) {
            let placeholders: Vec<String> =
                (1..=chunk.len()).map(|i| format!("${i}::uuid")).collect();
            let sql = format!(
                r#"SELECT id, metadata FROM "{coll}" WHERE id IN ({})"#,
                placeholders.join(", ")
            );
            let values: Vec<sea_orm::Value> = chunk.iter().map(|id| (*id).into()).collect();
            let rows = self
                .db
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    &sql,
                    values,
                ))
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            for row in &rows {
                results.push(Self::row_to_retrieve_result(row)?);
            }
        }

        Span::current().record(COGNEE_VECTOR_RESULT_COUNT, results.len() as i64);
        Ok(results)
    }

    #[instrument(
        name = "cognee.db.vector.batch_search_similar",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.vector.result_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn batch_search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vectors: &[Vec<f32>],
        top_k: usize,
    ) -> VectorDBResult<Vec<Vec<SearchResult>>> {
        if query_vectors.is_empty() {
            return Ok(vec![]);
        }
        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());
        // This override never routes through `search_similar`, so the
        // single-query warning would otherwise never fire on this path.
        warn_zero_norm_query_batch("pgvector", &coll, query_vectors);

        // One round-trip for the whole batch instead of the default's one query
        // per vector: unnest the query vectors with ordinality and run the ANN
        // search for each via a LATERAL join. Vector literals and `top_k` are
        // numeric-only, so inlining them carries no injection risk (same approach
        // as `search_similar`; `coll` is a validated identifier).
        let array_literal = query_vectors
            .iter()
            .map(|v| format!("'{}'::vector", Self::format_vector(v)))
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            r#"SELECT q.idx AS idx, t.id AS id, t.score AS score, t.metadata AS metadata
               FROM unnest(ARRAY[{array_literal}]) WITH ORDINALITY AS q(vec, idx)
               CROSS JOIN LATERAL (
                   SELECT id, 1 - (vector <=> q.vec) AS score, metadata
                   FROM "{coll}"
                   ORDER BY vector <=> q.vec
                   LIMIT {top_k}
               ) t
               ORDER BY q.idx, t.score DESC"#
        );

        // Each LATERAL subquery is its own index scan with its own `LIMIT
        // {top_k}`, so this path needs the same `ef_search` floor as
        // `search_similar` or every one of them ends early.
        let rows = self
            .query_all_with_locals(
                &Self::ann_search_locals(top_k),
                Statement::from_string(DatabaseBackend::Postgres, sql),
            )
            .await?;

        // Pre-size one bucket per query; `idx` (1-based ordinality) routes each row
        // back to its query, and queries with no hits keep their empty bucket.
        let mut results: Vec<Vec<SearchResult>> =
            (0..query_vectors.len()).map(|_| Vec::new()).collect();
        let mut total = 0usize;
        for row in &rows {
            let idx: i64 = row
                .try_get("", "idx")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            let result = Self::row_to_search_result(row)?;
            if let Some(bucket) = results.get_mut((idx as usize).saturating_sub(1)) {
                bucket.push(result);
                total += 1;
            }
        }
        Span::current().record(COGNEE_VECTOR_RESULT_COUNT, total as i64);
        Ok(results)
    }

    #[instrument(
        name = "cognee.db.vector.delete_collection",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
        ),
        err,
    )]
    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());

        let drop = Table::drop()
            .table(Alias::new(&coll))
            .if_exists()
            .to_owned();

        self.db
            .execute_unprepared(&drop.to_string(PostgresQueryBuilder))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        let delete = Query::delete()
            .from_table(VColl::Table)
            .and_where(Expr::col(VColl::CollectionName).eq(&coll))
            .to_owned();

        self.db
            .execute(self.build(&delete))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Ok(())
    }

    #[instrument(
        name = "cognee.db.vector.delete",
        level = "info",
        skip_all,
        fields(
            cognee.db.system = "pgvector",
            cognee.vector.collection = tracing::field::Empty,
            cognee.db.row_count = tracing::field::Empty,
        ),
        err,
    )]
    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        if point_ids.is_empty() {
            return Ok(());
        }

        let coll = Self::collection_name(data_type, field_name)?;
        Span::current().record(COGNEE_VECTOR_COLLECTION, coll.as_str());

        let query = Query::delete()
            .from_table(Alias::new(&coll))
            .and_where(
                Expr::col(Alias::new("id"))
                    .is_in(point_ids.iter().copied().map(sea_orm::Value::from)),
            )
            .to_owned();

        self.db
            .execute(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        Span::current().record(COGNEE_DB_ROW_COUNT, point_ids.len() as i64);
        Ok(())
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        let coll = Self::collection_name(data_type, field_name)?;

        let query = Query::select()
            .expr_as(Func::count(Expr::col(Asterisk)), Alias::new("count"))
            .from(Alias::new(&coll))
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        match row {
            Some(r) => {
                let count: i64 = r
                    .try_get("", "count")
                    .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
                Ok(count as usize)
            }
            None => Ok(0),
        }
    }

    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        let query = Query::select()
            .columns([VColl::DataType, VColl::FieldName])
            .from(VColl::Table)
            .order_by(VColl::CollectionName, Order::Asc)
            .to_owned();

        let rows = self
            .db
            .query_all(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        let mut pairs = Vec::with_capacity(rows.len());
        for row in &rows {
            let dt: String = row
                .try_get("", "data_type")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            let fn_: String = row
                .try_get("", "field_name")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            pairs.push((dt, fn_));
        }
        Ok(pairs)
    }
}

// ---------------------------------------------------------------------------
// SeaORM migration — creates the `vector` extension and bookkeeping table.
// ---------------------------------------------------------------------------
mod migrator {
    use sea_orm_migration::prelude::*;

    pub struct Migrator;

    #[async_trait::async_trait]
    impl MigratorTrait for Migrator {
        /// Track applied migrations in a pgvector-specific bookkeeping table rather
        /// than the default `seaql_migrations`. In an "everything in one Postgres"
        /// deployment the core/relational migrator, this pgvector adapter and the
        /// graph adapter all point at the same database; if they shared the default
        /// table each would treat the others' versions as "applied but missing" and
        /// abort. See the `shared_db_migration_tests` module below.
        fn migration_table_name() -> DynIden {
            Alias::new("seaql_migrations_pgvector").into_iden()
        }

        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(CreatePgVectorExtension)]
        }
    }

    struct CreatePgVectorExtension;

    impl MigrationName for CreatePgVectorExtension {
        fn name(&self) -> &str {
            "m20250101_000001_create_pgvector_extension"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for CreatePgVectorExtension {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let conn = manager.get_connection();

            conn.execute_unprepared("CREATE EXTENSION IF NOT EXISTS vector")
                .await?;

            conn.execute_unprepared(
                "CREATE TABLE IF NOT EXISTS _vector_collections (
                    collection_name TEXT PRIMARY KEY,
                    data_type       TEXT    NOT NULL,
                    field_name      TEXT    NOT NULL,
                    dimension       INTEGER NOT NULL,
                    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
                )",
            )
            .await?;

            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let conn = manager.get_connection();
            conn.execute_unprepared("DROP TABLE IF EXISTS _vector_collections")
                .await?;
            conn.execute_unprepared("DROP EXTENSION IF EXISTS vector")
                .await?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Shared-Postgres migration regression tests
//
// These run only when `PGVECTOR_TEST_URL` points at a live Postgres instance
// (with the `vector` extension) and are skipped otherwise. They live inline
// (rather than under `tests/`) so they can reuse the crate's own optional
// `sea-orm`/`sea-orm-migration` dependencies without forcing a heavy
// dev-dependency onto the default (feature-off) build.
//
// Each case provisions its OWN throwaway database via
// `cognee_test_utils::create_temp_postgres_db` and drops it again, so no
// `#[serial]` is needed. They used to share the `PGVECTOR_TEST_URL` database and
// call a `reset()` helper that dropped `_vector_collections` and the
// `seaql_migrations*` tables — which left the collection tables it did not know
// about (`UUID_f`, `Meta_f`, …) orphaned in the database, invisible to
// `list_collections`, and therefore undeletable by the integration suite's
// cleanup. The next `pgvector_integration` run against that server then failed
// with `relation "UUID_f" already exists`. That only became reachable in CI once
// both targets ran in one lane against one server, which is what surfaced it.
// ---------------------------------------------------------------------------
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod shared_db_migration_tests {
    use super::PgVectorAdapter;
    use sea_orm::{ConnectionTrait, Database, Statement};
    use sea_orm_migration::prelude::*;

    fn test_url() -> Option<String> {
        std::env::var("PGVECTOR_TEST_URL")
            .ok()
            .filter(|v| !v.is_empty())
    }

    /// Run `body` against a throwaway database of this test's own, dropped again
    /// afterwards — even if `body` panics.
    ///
    /// Both cases below are about two migrators *coexisting inside one database*,
    /// so the isolation is at the database level and nothing inside it is reset.
    ///
    /// `body` runs on a spawned task so a failed assertion surfaces as a
    /// `JoinError` instead of unwinding past the drop. `TempPostgresDb::cleanup`
    /// is `async`, so it cannot be a `Drop` impl; without this the database would
    /// leak on every red run. The panic is re-raised unchanged afterwards, so
    /// libtest still reports the original failure and message. Mirrors
    /// `with_temp_db` in `crates/graph/src/pg_graph_adapter.rs`, which solved the
    /// same problem for the same helper.
    ///
    /// The one leak this cannot cover is a test hard-killed rather than unwound
    /// (a `SIGKILL`, or nextest's `slow-timeout` terminate-after), which no async
    /// cleanup can survive; the databases are uniquely named, so the fallback is
    /// dropping stragglers by hand.
    async fn with_temp_db<F, Fut>(what: &str, body: F)
    where
        F: FnOnce(String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let Some(base_url) = test_url() else {
            eprintln!("PGVECTOR_TEST_URL not set — skipping {what}");
            return;
        };
        let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
            .await
            .expect("PGVECTOR_TEST_URL is set, so CREATE DATABASE must succeed on that server");
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
    async fn version_count(db: &sea_orm::DatabaseConnection, table: &str) -> i64 {
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

    /// The relational migrator and the pgvector adapter migrator must coexist in
    /// one Postgres DB without colliding on the default `seaql_migrations` table.
    #[tokio::test]
    async fn pgvector_coexists_with_relational_migrator_in_shared_db() {
        with_temp_db("shared-DB migration test", |url| async move {
            let db = Database::connect(&url).await.unwrap();

            // 1. Relational / auth migrator runs first and populates the default
            //    `seaql_migrations` with versions the vector migrator does not own.
            RelationalMigrator::up(&db, None)
                .await
                .expect("relational migrator should succeed");
            assert_eq!(version_count(&db, "seaql_migrations").await, 2);

            // 2. Initialising the vector adapter against the SAME database must
            //    succeed. Before the fix it aborted with "Migration file of version
            //    'm20260914_000002_auth' is missing ...".
            let adapter = PgVectorAdapter::new(&url, 384).await;
            assert!(
                adapter.is_ok(),
                "PgVectorAdapter init must not collide with the relational \
                 seaql_migrations table; got: {:?}",
                adapter.err()
            );

            // 3. The vector migrator tracks its version in its OWN table and leaves
            //    the relational bookkeeping untouched.
            assert_eq!(version_count(&db, "seaql_migrations").await, 2);
            assert_eq!(version_count(&db, "seaql_migrations_pgvector").await, 1);

            // Hand the pooled connections back before the database is dropped, so
            // cleanup does not have to lean on `WITH (FORCE)`.
            drop(adapter);
            drop(db);
        })
        .await;
    }

    /// Upgrade path: a legacy pgvector row left in the default `seaql_migrations`
    /// by an older build must be purged so the core migrator no longer chokes.
    #[tokio::test]
    async fn pgvector_purges_legacy_row_from_default_table_on_upgrade() {
        with_temp_db("legacy-purge test", |url| async move {
            let db = Database::connect(&url).await.unwrap();

            // Simulate an older build that recorded the pgvector version into the
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
                 VALUES ('m20250101_000001_create_pgvector_extension', 0)",
            ))
            .await
            .unwrap();

            // Upgraded build initialises the vector adapter.
            let adapter = PgVectorAdapter::new(&url, 384)
                .await
                .expect("vector adapter should initialise on upgrade");

            // The stale vector row is gone, so the core/relational migrator can now
            // run against the default table without aborting.
            assert_eq!(
                version_count(&db, "seaql_migrations").await,
                0,
                "legacy pgvector row must be purged from the default seaql_migrations"
            );
            RelationalMigrator::up(&db, None)
                .await
                .expect("core migrator must not choke after legacy row is purged");

            drop(adapter);
            drop(db);
        })
        .await;
    }
}
