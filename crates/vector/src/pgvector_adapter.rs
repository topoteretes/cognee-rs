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
    ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, Statement,
    TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::RwLock;
use tracing::{Span, debug, instrument, warn};
use uuid::Uuid;

use cognee_utils::sanitize::sanitize_json;
use cognee_utils::tracing_keys::{
    COGNEE_DB_ROW_COUNT, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint, dedup_points_by_id, dedup_points_by_id_last_wins};
use crate::vector_db_trait::{VectorDB, VectorIndexBackfill};
use crate::zero_norm::{warn_zero_norm_points, warn_zero_norm_query, warn_zero_norm_query_batch};

#[cfg(test)]
#[path = "pgvector_index_tests.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod vector_index_tests;

/// Ids per `= ANY($1::uuid[])` array parameter (retrieve, delete).
const ID_BATCH: usize = 20_000;

/// Rows per multi-row upsert `INSERT` (three bind parameters per row, so
/// 6 000 parameters — well under PostgreSQL's 65 535). Full batches share one
/// statement text, so their prepared statement is cached per connection.
const WRITE_BATCH: usize = 2000;

/// `ON CONFLICT (id)` metadata for [`VectorDB::index_points`]: the incoming
/// metadata with `dataset_ids` replaced by the union of the stored row's and
/// the incoming point's membership — stored `dataset_ids`, stored
/// `dataset_id`, incoming `dataset_ids`, incoming `dataset_id`; strings only,
/// blanks skipped, first occurrence wins — which is exactly
/// [`VectorPoint::merge_dataset_membership`] applied with the stored row as
/// `previous`. With no membership on either side the incoming metadata is
/// kept as is. Computed server-side, so no read-before-write round trip.
const MERGED_METADATA: &str = "COALESCE((
  SELECT jsonb_set(EXCLUDED.metadata, '{dataset_ids}', jsonb_agg(to_jsonb(d.v) ORDER BY d.o))
  FROM (
    SELECT s.v, min(s.o) AS o FROM (
      SELECT a.e #>> '{}' AS v, a.n AS o
        FROM jsonb_array_elements(CASE WHEN jsonb_typeof(t.metadata->'dataset_ids') = 'array'
                                       THEN t.metadata->'dataset_ids' ELSE '[]'::jsonb END)
             WITH ORDINALITY a(e, n)
       WHERE jsonb_typeof(a.e) = 'string'
      UNION ALL
      SELECT t.metadata->>'dataset_id', 1000000000
       WHERE jsonb_typeof(t.metadata->'dataset_id') = 'string'
      UNION ALL
      SELECT b.e #>> '{}', 2000000000 + b.n
        FROM jsonb_array_elements(CASE WHEN jsonb_typeof(EXCLUDED.metadata->'dataset_ids') = 'array'
                                       THEN EXCLUDED.metadata->'dataset_ids' ELSE '[]'::jsonb END)
             WITH ORDINALITY b(e, n)
       WHERE jsonb_typeof(b.e) = 'string'
      UNION ALL
      SELECT EXCLUDED.metadata->>'dataset_id', 4000000000
       WHERE jsonb_typeof(EXCLUDED.metadata->'dataset_id') = 'string'
    ) s
    WHERE s.v <> ''
    GROUP BY s.v
  ) d
  HAVING count(*) > 0), EXCLUDED.metadata)";

/// HNSW graph degree. Above pgvector's default of 16: with m = 16 /
/// ef_construction = 64 a 209k-row EdgeType index (clustered, low intrinsic
/// dimension) left whole clusters unreachable — recall@100 0.883 with two of
/// 20 queries near 0.02-0.06 at any ef_search <= 200 — and m = 24 /
/// ef_construction = 128 raised it to 0.9985 for ~1.75x the build time at
/// the same index size.
/// Default `m`; `COGNEE_PGVECTOR_HNSW_M` overrides it — see [`tuning`].
const HNSW_M: u32 = 24;

/// HNSW build-time candidate list size (pgvector's default is 64); see
/// [`HNSW_M`]. `COGNEE_PGVECTOR_HNSW_EF_CONSTRUCTION` overrides it.
const HNSW_EF_CONSTRUCTION: u32 = 128;

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

/// Candidate-list size per requested row: an ANN search runs with
/// `hnsw.ef_search = HNSW_EF_PER_K * top_k` (at least pgvector's default,
/// at most [`HNSW_EF_SEARCH_MAX`]). With `ef_search = top_k` a top-100 search
/// missed ~1% of the true top 100 at 10k rows and ~10% on EdgeType at 100k.
const HNSW_EF_PER_K: usize = 2;

/// Session `hnsw.ef_search` on pools this adapter opens itself: covers
/// cognee's `DEFAULT_WIDE_SEARCH_TOP_K = 100` searches (at
/// [`HNSW_EF_PER_K`]) without a per-query `SET LOCAL` transaction.
const HNSW_EF_SEARCH_SESSION: usize = 200;

/// Largest `hnsw.ef_search` pgvector accepts. Beyond this a search cannot be
/// made to return `top_k` rows by raising `ef_search`, so those queries fall
/// back to the exact scan — see [`PgVectorAdapter::ann_search_locals`].
const HNSW_EF_SEARCH_MAX: usize = 1000;

/// `hnsw.iterative_scan`, as a `SET LOCAL`. The value, and why it is
/// `relaxed_order` rather than `strict_order`, are in [`PgVectorAdapter::new`],
/// which sets the same thing as a connection option on its own pool; this is
/// the form [`PgVectorAdapter::untuned_session_locals`] uses on a connection it
/// did not open.
const ITERATIVE_SCAN_LOCAL: &str = "SET LOCAL hnsw.iterative_scan = relaxed_order";

/// `plan_cache_mode`, as a `SET LOCAL` — the twin of [`ITERATIVE_SCAN_LOCAL`].
const PLAN_CACHE_LOCAL: &str = "SET LOCAL plan_cache_mode = force_custom_plan";

/// Batches of at least this many points into an HNSW-indexed collection are
/// candidates for [`PgVectorAdapter::upsert_rebuilding_index`].
const HNSW_REBUILD_MIN_ROWS: usize = 200;

/// Rebuild the HNSW index around a batch that would insert at least this
/// fraction of the collection's current rows into it (see
/// [`PgVectorAdapter::upsert_rebuilding_index`]). Calibrated at 100k: four
/// concurrent writers insert at ~0.5 ms per row, a 4-worker build of a 212k
/// index costs ~0.14 ms per row of the whole collection, so a rebuild pays
/// off from roughly 0.3 x the current rows; 0.4 leaves room for re-indexed
/// points that turn out to be no-ops.
const HNSW_REBUILD_RATIO: f64 = 0.4;

/// `max_parallel_maintenance_workers` for an index rebuild (the server default
/// is 2). pgvector's HNSW build scales with workers: 41k 384-d rows took
/// 12.8 s / 5.0 s / 3.2 s / 2.1 s with 0 / 2 / 4 / 7 workers here. Still
/// capped by the server's `max_parallel_workers` / `max_worker_processes`.
const HNSW_BUILD_WORKERS: u32 = 4;

/// Inside a bulk-load scope (see [`VectorDB::begin_bulk_load`]), drop a
/// collection's HNSW index once the points written to it in this scope reach
/// this fraction of its rows, and build it once when the scope ends.
///
/// A ski-rental break-even: a live insert costs ~1.2-1.7 ms per 384-d row
/// (~0.35 ms wall with four writers) and a 7-worker build ~0.09 ms per row of
/// the whole collection (209k rows: 17.9 s), so keep inserting live until
/// the live cost already paid in this scope equals one build — ~0.25 x the
/// rows — then stop maintaining the index. Worst case this costs one build
/// more than staying live; an empty or small collection defers at once.
const BULK_DEFER_RATIO: f64 = 0.25;

/// Whether a collection that held `base_rows` rows when this bulk load started,
/// and has been handed `written` points since, has reached
/// [`BULK_DEFER_RATIO`] of its (current) rows and should stop maintaining its
/// index for the rest of the load.
///
/// The current size is `base_rows + written` rather than a fresh `count(*)`;
/// see [`PgVectorAdapter::bulk_should_defer`] for why that is both cheaper and
/// equivalent. An empty collection (`base_rows == 0`) defers on its first
/// batch, which is the intended behaviour — there is no index work worth
/// maintaining.
fn bulk_defer_reached(written: i64, base_rows: i64) -> bool {
    let written = written.max(0);
    let current = base_rows.max(0).saturating_add(written);
    written > 0 && (written as f64) >= BULK_DEFER_RATIO * current as f64
}

/// `max_parallel_maintenance_workers` for the end-of-load build (the server
/// caps it by `max_parallel_workers` / `max_worker_processes`, 8 by default).
const BULK_BUILD_WORKERS: u32 = 7;

/// Concurrent upsert statements for a batch that goes into a live HNSW
/// index (see [`PgVectorAdapter::write_points_concurrently`]).
const HNSW_INSERT_WRITERS: usize = 4;

/// Smallest per-statement batch when a load is split across writers.
const HNSW_INSERT_MIN_BATCH: usize = 250;

/// Lower / upper bound of the `maintenance_work_mem` (kB) an index rebuild
/// runs under: pgvector's default, and 2 GB. The upper bound is what
/// `COGNEE_PGVECTOR_MAINTENANCE_WORK_MEM_MB` overrides (see [`tuning`]).
const HNSW_BUILD_BUDGET_MIN_KB: i64 = 64 * 1024;
const HNSW_BUILD_BUDGET_MAX_KB: i64 = 2 * 1024 * 1024;

/// Operator overrides for the index-build knobs above.
///
/// Four environment variables, each read once per process and clamped to a
/// range the adapter can still work in. They exist because the defaults are
/// calibrated on one machine against one corpus shape, and the two that cost
/// resources — parallel maintenance workers and `maintenance_work_mem` — have
/// to be turnable *down* on a small or shared server without patching the
/// crate. Unset, every one of them is the measured default.
///
/// | Variable | Default | Range | Effect |
/// |---|---|---|---|
/// | `COGNEE_PGVECTOR_HNSW_M` | 24 | 2–100 | `m` of every HNSW index built from now on |
/// | `COGNEE_PGVECTOR_HNSW_EF_CONSTRUCTION` | 128 | `2 * m`–1000 | `ef_construction` of those builds |
/// | `COGNEE_PGVECTOR_MAINTENANCE_WORKERS` | 4 rebuild / 7 bulk-load | 0–64 | `max_parallel_maintenance_workers` for a build, when set overriding both defaults |
/// | `COGNEE_PGVECTOR_MAINTENANCE_WORK_MEM_MB` | 2048 | 64–65536 | ceiling of the per-build `maintenance_work_mem` |
///
/// `m` / `ef_construction` apply to indexes **built after** the change; an
/// index already on disk keeps the parameters it was built with until it is
/// rebuilt (`create_missing_vector_indexes` does not rebuild a valid index).
///
/// Asking for more parallel maintenance workers than the server can start is
/// not an error: `max_parallel_maintenance_workers` is a plain per-session
/// GUC, and Postgres silently launches as many as
/// `max_parallel_workers` / `max_worker_processes` still allow — down to zero,
/// where the build runs serially. So a 7-worker request on a server
/// configured for two gets two, and the only cost is a slower build.
mod tuning {
    use std::sync::OnceLock;

    /// `var` parsed as a `u64` and clamped to `[min, max]`, or `default` when
    /// unset, blank or unparseable. A bad value is a typo in a deployment
    /// environment, not something to fail a connection over, so it falls back
    /// to the default rather than erroring.
    fn env_u64(var: &str, default: u64, min: u64, max: u64) -> u64 {
        std::env::var(var)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map_or(default, |v| v.clamp(min, max))
    }

    /// `m` for new HNSW indexes — see the module docs.
    pub fn hnsw_m() -> u32 {
        static CACHE: OnceLock<u32> = OnceLock::new();
        *CACHE.get_or_init(|| {
            env_u64("COGNEE_PGVECTOR_HNSW_M", u64::from(super::HNSW_M), 2, 100) as u32
        })
    }

    /// `ef_construction` for new HNSW indexes. pgvector requires
    /// `ef_construction >= 2 * m`, so the floor follows [`hnsw_m`] rather than
    /// being a constant — a configuration that violated it would make every
    /// `CREATE INDEX` fail.
    pub fn hnsw_ef_construction() -> u32 {
        static CACHE: OnceLock<u32> = OnceLock::new();
        *CACHE.get_or_init(|| {
            let floor = u64::from(hnsw_m()) * 2;
            env_u64(
                "COGNEE_PGVECTOR_HNSW_EF_CONSTRUCTION",
                u64::from(super::HNSW_EF_CONSTRUCTION).max(floor),
                floor,
                1000,
            ) as u32
        })
    }

    /// `max_parallel_maintenance_workers` for an index build. `default` is the
    /// measured value for that build path ([`super::HNSW_BUILD_WORKERS`] for a
    /// mid-load rebuild, [`super::BULK_BUILD_WORKERS`] for the one build at
    /// the end of a bulk-load scope); the override, when set, replaces both.
    pub fn maintenance_workers(default: u32) -> u32 {
        static CACHE: OnceLock<Option<u32>> = OnceLock::new();
        CACHE
            .get_or_init(|| {
                std::env::var("COGNEE_PGVECTOR_MAINTENANCE_WORKERS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(|v| v.min(64) as u32)
            })
            .unwrap_or(default)
    }

    /// Ceiling of the per-build `maintenance_work_mem`, in kB.
    pub fn build_budget_max_kb() -> i64 {
        static CACHE: OnceLock<i64> = OnceLock::new();
        *CACHE.get_or_init(|| {
            let default_mb = (super::HNSW_BUILD_BUDGET_MAX_KB / 1024) as u64;
            (env_u64(
                "COGNEE_PGVECTOR_MAINTENANCE_WORK_MEM_MB",
                default_mb,
                (super::HNSW_BUILD_BUDGET_MIN_KB / 1024) as u64,
                64 * 1024,
            ) * 1024) as i64
        })
    }

    #[cfg(test)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable failures"
    )]
    mod tests {
        use super::env_u64;

        /// The accessors above cache in a `OnceLock`, so the parsing and
        /// clamping rule is what there is to pin: a blank, negative or
        /// non-numeric value must fall back to the default rather than
        /// propagate into DDL, and an out-of-range one must clamp.
        #[test]
        fn a_bad_or_out_of_range_value_falls_back_or_clamps() {
            const VAR: &str = "COGNEE_PGVECTOR_TUNING_PARSE_TEST";
            // SAFETY: single-threaded test over a variable no other test or
            // accessor reads; nothing else in this process can observe it.
            for (raw, want) in [
                (None, 24),
                (Some(""), 24),
                (Some("   "), 24),
                (Some("not-a-number"), 24),
                (Some("-8"), 24),
                (Some("48"), 48),
                (Some(" 48 "), 48),
                (Some("1"), 2),
                (Some("100000"), 100),
            ] {
                unsafe {
                    match raw {
                        Some(v) => std::env::set_var(VAR, v),
                        None => std::env::remove_var(VAR),
                    }
                }
                assert_eq!(
                    env_u64(VAR, 24, 2, 100),
                    want,
                    "{raw:?} must resolve to {want}"
                );
            }
            unsafe { std::env::remove_var(VAR) };
        }
    }
}

/// See `migrator::CreateSetNamesFunction`.
const SET_NAMES_FUNCTION_DDL: &str = "
CREATE OR REPLACE FUNCTION cognee_vector_set_names(m jsonb) RETURNS text[]
LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT AS $fn$
  SELECT COALESCE(array_agg(n), '{}'::text[]) FROM (
    SELECT CASE jsonb_typeof(e)
             WHEN 'string' THEN e #>> '{}'
             WHEN 'object' THEN CASE WHEN jsonb_typeof(e->'name') = 'string' THEN e->>'name' END
           END AS n
    FROM jsonb_array_elements(
      CASE WHEN jsonb_typeof(m->'belongs_to_set') = 'array'
           THEN m->'belongs_to_set' ELSE '[]'::jsonb END) e
  ) s WHERE n IS NOT NULL
$fn$";

/// Postgres truncates any identifier past this many bytes (`NAMEDATALEN - 1`).
const PG_MAX_IDENTIFIER_BYTES: usize = 63;

/// Index-name suffixes. Each is kept whole by [`PgVectorAdapter::index_name`],
/// so two *kinds* of index on one collection can never truncate onto the same
/// name however long the collection name is.
const HNSW_HALFVEC_SUFFIX: &str = "_halfvec_hnsw";
const HNSW_VECTOR_SUFFIX: &str = "_vector_hnsw";
const SET_NAMES_SUFFIX: &str = "_set_names";

/// Leading `ORDER BY` key that keeps a `NaN` similarity score *last* rather
/// than first, for a `score DESC` ordering over the `score` alias.
///
/// Postgres sorts `NaN` as greater than every other `float8` (and equal to
/// itself, which is why `= 'NaN'` is the test), so a bare `ORDER BY score DESC`
/// leads with it. A `NaN` score is not hypothetical: it is what
/// `1 - (vector <=> $1)` gives for a stored vector of norm zero — the case
/// [`warn_zero_norm_points`] exists to warn about — so without this key a
/// single all-zero row becomes `results[0]` of every search over a collection
/// at or below `top_k`. The distance ordering that selects candidates sorts
/// `NaN` last on its own (it is an `ASC` ordering), so this only has to repair
/// the outer re-sort.
///
/// Sorting booleans ascending puts `false` (every real score) before `true`.
const NAN_LAST: &str = "(score = 'NaN'::float8)";

/// Lowest `vector` extension version that has the `halfvec` type and the
/// `halfvec_cosine_ops` opclass (pgvector 0.7.0, May 2024).
///
/// Below it the half-precision index and the `vector::halfvec(n)` ordering both
/// fail outright with `type "halfvec" does not exist` — not a slow fallback, an
/// error on every similarity search — and nothing in the setup would catch it:
/// the migration only runs `CREATE EXTENSION IF NOT EXISTS vector`, which is a
/// no-op against a database that already carries 0.5.x or 0.6.x. So the version
/// is probed once per adapter and the full-precision path used below it; see
/// [`halfvec_in_extversion`] and [`probe_pgvector_features`].
const HALFVEC_MIN_VERSION: (u32, u32) = (0, 7);

/// Lowest `vector` extension version that has the `hnsw.iterative_scan` setting
/// (pgvector 0.8.0, October 2024).
///
/// This one has to be gated, not just preferred. pgvector marks the `hnsw.` GUC
/// prefix reserved, so on an older extension `SET LOCAL hnsw.iterative_scan` is
/// not quietly ignored — it is `unrecognized configuration parameter`, which
/// aborts the transaction and so fails the search that was about to run in it.
/// [`PgVectorAdapter::new`] is not exposed to that, because a libpq connection
/// *option* is applied before any extension library is loaded and therefore
/// becomes a placeholder (dropped with a warning when an older pgvector reserves
/// the prefix); a `SET LOCAL` on a connection that has already touched a vector
/// column is not so lucky. See [`PgVectorAdapter::untuned_session_locals`].
const ITERATIVE_SCAN_MIN_VERSION: (u32, u32) = (0, 8);

/// Whether `extversion` (the `pg_extension.extversion` string of the `vector`
/// extension, e.g. `"0.8.2"`) is at least `min`.
///
/// Only the first two components are compared: both gated features arrived in a
/// `.0`, so no patch release is on a boundary. An unparseable version answers
/// `false`, because each fallback is correct on every version while the gated
/// path is correct on none below its minimum — the safe side of a guess is the
/// slower index, not a store whose every search errors.
fn extversion_at_least(extversion: &str, min: (u32, u32)) -> bool {
    let mut parts = extversion.trim().split('.');
    let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
        return false;
    };
    // A suffix like `0.8.0-dev` or `0.7` is fine: only the leading digits of
    // each of the first two components are read.
    let num = |s: &str| {
        s.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .ok()
    };
    match (num(major), num(minor)) {
        (Some(major), Some(minor)) => (major, minor) >= min,
        _ => false,
    }
}

/// [`extversion_at_least`] at [`HALFVEC_MIN_VERSION`].
fn halfvec_in_extversion(extversion: &str) -> bool {
    extversion_at_least(extversion, HALFVEC_MIN_VERSION)
}

/// [`extversion_at_least`] at [`ITERATIVE_SCAN_MIN_VERSION`].
fn iterative_scan_in_extversion(extversion: &str) -> bool {
    extversion_at_least(extversion, ITERATIVE_SCAN_MIN_VERSION)
}

/// What the installed `vector` extension's version allows, probed once per
/// adapter (see [`probe_pgvector_features`]).
#[derive(Debug, Clone, Copy, Default)]
struct PgVectorFeatures {
    /// The `halfvec` type and `halfvec_cosine_ops` opclass
    /// ([`HALFVEC_MIN_VERSION`]).
    halfvec: bool,
    /// The `hnsw.iterative_scan` setting ([`ITERATIVE_SCAN_MIN_VERSION`]).
    iterative_scan: bool,
}

/// Probe the installed `vector` extension once and say which version-gated
/// features this adapter may use.
///
/// Called from both constructors, after the migration has run
/// `CREATE EXTENSION IF NOT EXISTS vector`, so the row is expected to be there.
/// A missing row or a failed query answers "neither" and logs: the adapter stays
/// usable on the full-precision, non-iterative path either way, and degrading
/// loudly beats erroring on every search.
async fn probe_pgvector_features(db: &DatabaseConnection) -> PgVectorFeatures {
    let row = db
        .query_one(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT extversion FROM pg_extension WHERE extname = 'vector'",
        ))
        .await;
    let extversion = match row {
        Ok(Some(row)) => row.try_get::<String>("", "extversion").ok(),
        Ok(None) => None,
        Err(e) => {
            warn!(
                error = %e,
                "could not read the installed pgvector version — using the \
                 full-precision HNSW index and ordering"
            );
            return PgVectorFeatures::default();
        }
    };
    let Some(v) = extversion else {
        warn!(
            "the `vector` extension reported no version — using the \
             full-precision HNSW index and ordering"
        );
        return PgVectorFeatures::default();
    };
    if halfvec_in_extversion(&v) {
        debug!("pgvector {v}: using the half-precision HNSW index and ordering");
    } else {
        warn!(
            "pgvector {v} is older than {}.{} and has no `halfvec` type: falling back \
             to the full-precision index and ordering (larger index, slower scans, \
             identical results). `ALTER EXTENSION vector UPDATE` after installing a \
             newer pgvector binary, then `cognee-cli vector-reindex`, switches to the \
             half-precision index.",
            HALFVEC_MIN_VERSION.0, HALFVEC_MIN_VERSION.1,
        );
    }
    if !iterative_scan_in_extversion(&v) {
        // Only `from_connection` adapters can notice: `new`'s pools get this as
        // a connection option, which an older pgvector turns into a dropped
        // placeholder rather than an error.
        debug!(
            "pgvector {v} is older than {}.{} and has no `hnsw.iterative_scan`: a search \
             on a caller-owned connection may return fewer than top_k rows once the \
             collection has dead tuples",
            ITERATIVE_SCAN_MIN_VERSION.0, ITERATIVE_SCAN_MIN_VERSION.1,
        );
    }
    PgVectorFeatures {
        halfvec: halfvec_in_extversion(&v),
        iterative_scan: iterative_scan_in_extversion(&v),
    }
}

/// Cases for the pgvector version gate.
///
/// `cfg(test)` alone, with no feature and no database: what has to be pinned is
/// the *decision*, and that is a pure function of the `extversion` string.
///
/// # Verifying the degraded path against a real old extension
/// Not done here, and not claimed: there is no pgvector 0.6 to hand, and the
/// floating `pgvector/pgvector:pg16` tag CI uses resolves to 0.8.x, which is
/// exactly why this defect reached review. To check it end to end:
///
/// ```text
/// docker run -e POSTGRES_PASSWORD=pg -p 5432:5432 pgvector/pgvector:0.6.2-pg16
/// PGVECTOR_TEST_URL=postgres://postgres:pg@localhost/postgres \
///   cargo test -p cognee-vector --features pgvector -- --test-threads 1
/// ```
///
/// Expected: the `pgvector 0.6.2 is older than 0.7` warning once per adapter,
/// `<coll>_vector_hnsw` instead of `<coll>_halfvec_hnsw` (the index-name
/// assertions in `pgvector_index_tests` are written for the modern name and
/// will fail on that image — they pin the 0.7+ shape deliberately), and every
/// similarity search returning the same rows it does on 0.8.
#[cfg(test)]
mod halfvec_gate_tests {
    use super::halfvec_in_extversion;

    /// 0.7.0 is the boundary, and an unreadable version must land on the
    /// full-precision side — the one that works on every version. Getting the
    /// `false` direction wrong makes every similarity search fail with
    /// `type "halfvec" does not exist`; getting `true` wrong only costs index
    /// size and scan time.
    #[test]
    fn the_gate_opens_at_0_7_and_closes_on_anything_unreadable() {
        for (raw, want) in [
            ("0.8.2", true),
            ("0.7.0", true),
            ("0.7", true),
            (" 0.8.0 ", true),
            ("0.8.0-dev", true),
            ("1.0.0", true),
            ("0.10.0", true),
            ("0.6.2", false),
            ("0.6", false),
            ("0.5.1", false),
            ("", false),
            ("0", false),
            ("unknown", false),
            ("0.x", false),
        ] {
            assert_eq!(
                halfvec_in_extversion(raw),
                want,
                "pgvector {raw:?} must resolve to halfvec = {want}"
            );
        }
    }
}

/// Cases for index naming at the identifier-length limit.
///
/// No feature and no database: the names are pure functions of the collection
/// name, and what has to hold is that the two *kinds* of index on one
/// collection never land on the same name. They used to, silently: a 62-byte
/// collection gave both `coll + "_"`, a 63-byte one gave both the table's own
/// name, and `CREATE INDEX IF NOT EXISTS` reports a taken name as a NOTICE
/// rather than an error.
#[cfg(test)]
mod index_name_tests {
    use super::{
        HNSW_HALFVEC_SUFFIX, HNSW_VECTOR_SUFFIX, PG_MAX_IDENTIFIER_BYTES, PgVectorAdapter,
        SET_NAMES_SUFFIX,
    };

    #[test]
    fn the_index_kinds_never_collide_however_long_the_collection_name_is() {
        // Short names are unaffected; the lengths that used to collide are 62
        // and 63, and the suffix starts being trimmed around 50.
        for len in [1, 10, 49, 50, 51, 57, 61, 62, 63, 64, 200] {
            let coll = "c".repeat(len);
            let hnsw = PgVectorAdapter::vector_index_name(&coll, true);
            let legacy = PgVectorAdapter::vector_index_name(&coll, false);
            let gin = PgVectorAdapter::set_names_index_name(&coll);

            for (what, name, suffix) in [
                ("halfvec hnsw", &hnsw, HNSW_HALFVEC_SUFFIX),
                ("full-precision hnsw", &legacy, HNSW_VECTOR_SUFFIX),
                ("set-names gin", &gin, SET_NAMES_SUFFIX),
            ] {
                assert!(
                    name.len() <= PG_MAX_IDENTIFIER_BYTES,
                    "{what} at collection length {len} must fit the identifier \
                     limit, or Postgres truncates it and `vector_index_state` \
                     looks for a name that was never created: {name}"
                );
                assert!(
                    name.ends_with(suffix),
                    "{what} at collection length {len} must keep its whole \
                     suffix — that is what makes a collision impossible: {name}"
                );
            }

            assert_ne!(
                hnsw, gin,
                "the HNSW and GIN indexes of one {len}-byte collection must \
                 have different names, or the second CREATE INDEX IF NOT \
                 EXISTS silently no-ops against the first"
            );
            assert_ne!(legacy, gin, "and so must the full-precision HNSW index");
            assert_ne!(
                hnsw, coll,
                "an index must never be named after its own table, or \
                 CREATE INDEX IF NOT EXISTS no-ops for ever"
            );
            assert_ne!(gin, coll);
        }
    }
}

/// Cases for the bulk-load deferral threshold.
///
/// No feature and no database: the decision became a pure function the moment
/// the collection's size stopped being re-counted per batch, and keeping it
/// equivalent to the per-batch `count(*)` is the whole point of the change.
#[cfg(test)]
mod bulk_defer_tests {
    use super::bulk_defer_reached;

    /// The threshold has to be what the per-batch `count(*)` produced. That
    /// count was `base + new rows so far`, so `w >= 0.25 (base + w)`, i.e.
    /// `w >= base / 3` — and an empty collection defers on its first batch.
    #[test]
    fn the_threshold_matches_the_per_batch_count_it_replaced() {
        assert!(
            bulk_defer_reached(1, 0),
            "an empty collection has no index work worth maintaining"
        );
        assert!(
            !bulk_defer_reached(0, 0),
            "but nothing written is not a load at all"
        );
        assert!(!bulk_defer_reached(0, 300_000));

        // 300k rows: a third of them, which is where 0.25 x (base + written)
        // lands. The row below the line must not defer, the row on it must.
        assert!(!bulk_defer_reached(99_999, 300_000));
        assert!(bulk_defer_reached(100_000, 300_000));

        // The incremental re-cognify this change is for: small batches into a
        // large collection stay under the line, which is correct — the point
        // is that they no longer pay a scan-sized count to find that out.
        assert!(!bulk_defer_reached(500, 300_000));
        assert!(!bulk_defer_reached(50_000, 300_000));

        // Negative or absurd inputs must not panic or wrap.
        assert!(!bulk_defer_reached(-1, 300_000));
        assert!(bulk_defer_reached(i64::MAX, i64::MAX));
    }
}

/// Cases for which searches may select candidates in half precision.
///
/// No feature and no database: the decision is a pure function of the
/// dimension, the `top_k`, whether `halfvec` exists at all and whether this
/// collection's index is dropped right now. What it protects
/// is the two paths this adapter and `docs/tools/backends.md` both declare
/// *exact*, where an fp16 ordering would reorder rows at the `LIMIT` boundary
/// and the outer `ORDER BY score DESC` would not notice — it only re-sorts a
/// candidate set already chosen.
#[cfg(test)]
mod candidate_order_tests {
    use super::{HNSW_EF_SEARCH_MAX, MAX_INDEXABLE_DIMENSION, PgVectorAdapter};

    fn is_fp16(dim: usize, top_k: usize, halfvec: bool) -> bool {
        indexed_is_fp16(dim, top_k, halfvec, false)
    }

    fn indexed_is_fp16(dim: usize, top_k: usize, halfvec: bool, indexless: bool) -> bool {
        PgVectorAdapter::candidate_order_expr("$1", dim, top_k, halfvec, indexless)
            .contains("halfvec")
    }

    #[test]
    fn half_precision_selection_is_confined_to_the_indexable_searches() {
        // The ordinary case, and the two boundaries that still have an index.
        assert!(is_fp16(384, 100, true), "the indexed path selects in fp16");
        assert!(
            is_fp16(MAX_INDEXABLE_DIMENSION, 100, true),
            "the widest indexable collection is still indexed"
        );
        assert!(
            is_fp16(384, HNSW_EF_SEARCH_MAX, true),
            "the largest top_k an ef_search can cover still uses the index"
        );

        // Past the index ceiling pgvector cannot index the collection at all,
        // so the search is the sequential scan the docs call exact.
        assert!(
            !is_fp16(MAX_INDEXABLE_DIMENSION + 1, 100, true),
            "a collection over the index ceiling must be ordered exactly"
        );
        assert!(
            !is_fp16(3072, 100, true),
            "text-embedding-3-large is the real case of that"
        );

        // Past HNSW_EF_SEARCH_MAX, `ann_search_locals` forces the exact scan
        // precisely to guarantee the true top-k; fp16 would spoil it.
        assert!(
            !is_fp16(384, HNSW_EF_SEARCH_MAX + 1, true),
            "a top_k that forces the exact scan must be ordered exactly"
        );

        // And without the type there is no fp16 expression to emit.
        assert!(!is_fp16(384, 100, false));
    }

    /// The bulk-load case, which `VectorDB::begin_bulk_load` calls "a hint,
    /// never a semantic change … served by exact scan meanwhile". A collection
    /// whose index is dropped for the rest of the load has nothing to match, so
    /// entering a scope must not quietly move rows across the `LIMIT` boundary.
    #[test]
    fn a_collection_with_its_index_dropped_is_ordered_exactly() {
        assert!(
            indexed_is_fp16(384, 100, true, false),
            "the control: the very same search is fp16 while the index is there"
        );
        assert!(
            !indexed_is_fp16(384, 100, true, true),
            "an indexless collection must be ordered exactly, or a bulk load \
             changes what a search returns"
        );
        // The three already-exact cases stay exact — the flag can only remove
        // fp16, never add it.
        assert!(!indexed_is_fp16(
            MAX_INDEXABLE_DIMENSION + 1,
            100,
            true,
            true
        ));
        assert!(!indexed_is_fp16(384, HNSW_EF_SEARCH_MAX + 1, true, true));
        assert!(!indexed_is_fp16(384, 100, false, true));
    }
}

/// Cases for the per-statement session settings a search carries.
///
/// No feature and no database: the builders are pure functions of
/// `tuned_sessions`, the probed pgvector version and `top_k`, which is the whole
/// reason they take those as arguments rather than reading `self`. What they
/// protect
/// is that an adapter built by [`PgVectorAdapter::from_connection`] — where the
/// pool belongs to the caller and `new`'s connection options were never
/// applied — answers the same as one built by [`PgVectorAdapter::new`], while
/// `new`'s own pools keep the single-round-trip fast path.
#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod session_locals_tests {
    use super::{
        HNSW_EF_SEARCH_MAX, HNSW_EF_SEARCH_SESSION, ITERATIVE_SCAN_LOCAL,
        ITERATIVE_SCAN_MIN_VERSION, PLAN_CACHE_LOCAL, PgVectorAdapter,
        iterative_scan_in_extversion,
    };

    /// The gate's own boundary, like `halfvec_gate_tests` does for 0.7.
    #[test]
    fn the_iterative_scan_gate_opens_at_0_8() {
        for (raw, want) in [
            ("0.8.2", true),
            ("0.8.0", true),
            ("0.8", true),
            ("1.0.0", true),
            ("0.10.0", true),
            ("0.7.4", false),
            ("0.7", false),
            ("0.6.2", false),
            ("", false),
            ("unknown", false),
        ] {
            assert_eq!(
                iterative_scan_in_extversion(raw),
                want,
                "pgvector {raw:?} must resolve to iterative_scan = {want} (gate at {}.{})",
                ITERATIVE_SCAN_MIN_VERSION.0,
                ITERATIVE_SCAN_MIN_VERSION.1,
            );
        }
    }

    /// `new()`'s pools: one plain statement for an ordinary search, and never a
    /// repeat of what the connection options already carry.
    #[test]
    fn an_owned_pool_keeps_the_single_round_trip_fast_path() {
        assert_eq!(
            PgVectorAdapter::ann_search_locals(true, true, 100),
            None,
            "a top-100 search on our own pool must stay one plain statement"
        );
        assert_eq!(PgVectorAdapter::filtered_search_locals(true), None);

        // Above the session ef_search it does need a transaction — but only for
        // ef_search; the rest is already on the connection.
        let locals = PgVectorAdapter::ann_search_locals(true, true, HNSW_EF_SEARCH_SESSION)
            .expect("2 x top_k is past the session ef_search, so this must set one");
        assert!(locals.contains("hnsw.ef_search"));
        assert!(
            !locals.contains("iterative_scan") && !locals.contains("plan_cache_mode"),
            "repeating the connection options would cost every search a \
             transaction it does not need: {locals}"
        );
    }

    /// `from_connection()`: the caller's pool never saw `new`'s options, so each
    /// search has to bring them. This is free — `tuned_sessions == false`
    /// already forces a `SET LOCAL ef_search`, so the transaction and its one
    /// round trip exist either way.
    #[test]
    fn a_caller_owned_pool_carries_the_ann_settings_per_statement() {
        let locals = PgVectorAdapter::ann_search_locals(false, true, 100)
            .expect("a connection we did not open has no session tuning at all");
        assert!(
            locals.contains("hnsw.ef_search"),
            "ef_search still has to cover top_k: {locals}"
        );
        assert!(
            locals.contains(ITERATIVE_SCAN_LOCAL),
            "without iterative_scan an HNSW scan whose beam runs dry returns \
             fewer than top_k rows and says nothing: {locals}"
        );
        assert!(
            locals.contains(PLAN_CACHE_LOCAL),
            "without force_custom_plan a cached generic plan costs the search \
             blind: {locals}"
        );

        // Past the ef_search ceiling the exact scan is forced instead, and the
        // rest still has to come along.
        let huge = PgVectorAdapter::ann_search_locals(false, true, HNSW_EF_SEARCH_MAX + 1)
            .expect("the exact-scan settings are always needed");
        assert!(huge.contains("enable_indexscan = off"));
        assert!(huge.contains(PLAN_CACHE_LOCAL));

        // The filtered search keeps the index out of the query on purpose, so
        // it needs none of the ANN levers — but it is the measured case for
        // force_custom_plan, and today it is the one search path that would
        // otherwise run with no settings at all.
        assert_eq!(
            PgVectorAdapter::filtered_search_locals(false).as_deref(),
            Some(PLAN_CACHE_LOCAL),
        );
    }

    /// The version gate. pgvector marks the `hnsw.` GUC prefix reserved, so on
    /// an extension older than [`ITERATIVE_SCAN_MIN_VERSION`] that `SET LOCAL`
    /// is `unrecognized configuration parameter` — an error that aborts the
    /// transaction and takes the search with it, which is strictly worse than
    /// the short result it was meant to prevent. `plan_cache_mode` is core
    /// Postgres and must still come along.
    #[test]
    fn an_older_pgvector_gets_no_iterative_scan_and_still_gets_the_rest() {
        let locals = PgVectorAdapter::ann_search_locals(false, false, 100)
            .expect("ef_search and plan_cache_mode are still needed");
        assert!(
            !locals.contains("iterative_scan"),
            "setting a GUC this pgvector does not define fails the search: {locals}"
        );
        assert!(locals.contains("hnsw.ef_search"), "{locals}");
        assert!(locals.contains(PLAN_CACHE_LOCAL), "{locals}");

        // The gate is about the older extension only; `hnsw.ef_search` has
        // existed since 0.5, so it is never gated.
        assert_eq!(
            PgVectorAdapter::untuned_session_locals(false, false),
            [PLAN_CACHE_LOCAL]
        );
        assert_eq!(
            PgVectorAdapter::untuned_session_locals(false, true),
            [ITERATIVE_SCAN_LOCAL, PLAN_CACHE_LOCAL]
        );
        assert!(PgVectorAdapter::untuned_session_locals(true, true).is_empty());
    }

    /// Every statement is a complete `SET LOCAL …`, joined so the whole string
    /// is one simple-query round trip — a missing separator would make the
    /// *first* setting a syntax error, failing every search.
    #[test]
    fn the_locals_are_semicolon_separated_set_local_statements() {
        let locals = PgVectorAdapter::ann_search_locals(false, true, HNSW_EF_SEARCH_MAX + 1)
            .expect("the exact-scan settings are always needed");
        for stmt in locals.split("; ") {
            assert!(
                stmt.starts_with("SET LOCAL ") && !stmt.contains(';'),
                "not a single SET LOCAL statement: {stmt:?} in {locals:?}"
            );
        }
    }
}

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
    /// Every pooled connection was opened with [`Self::new`]'s session options —
    /// [`HNSW_EF_SEARCH_SESSION`] as `hnsw.ef_search`, plus
    /// `hnsw.iterative_scan` and `plan_cache_mode` — so an ANN search with
    /// `top_k` up to that runs as one plain statement instead of
    /// `BEGIN; SET LOCAL …; SELECT; COMMIT`.
    ///
    /// `false` for [`Self::from_connection`], where the searches carry the same
    /// settings themselves as `SET LOCAL`s (see
    /// [`Self::untuned_session_locals`]) — they already open a transaction for
    /// `ef_search`, so correctness does not depend on which constructor a caller
    /// used, only the round trip does.
    tuned_sessions: bool,
    /// Collections known to exist, so `has_collection` — called before every
    /// search and upsert by the retrievers and the indexer — is a lookup
    /// instead of a round trip. Only positive answers are cached; a
    /// collection dropped behind this adapter's back (another process) is
    /// evicted on the first "relation does not exist" error.
    known: RwLock<HashSet<String>>,
    /// Bulk-load scope state (see [`VectorDB::begin_bulk_load`]).
    bulk: std::sync::Mutex<BulkLoad>,
    /// Whether the installed `vector` extension has the `halfvec` type, probed
    /// once at construction (see [`probe_pgvector_features`]).
    ///
    /// `false` puts both the index and the search ordering back on
    /// full-precision `vector`: correct on every pgvector version, just a
    /// larger index and a slower scan. It is not a tuning knob — below
    /// [`HALFVEC_MIN_VERSION`] the half-precision expression does not parse,
    /// so every similarity search would fail with `type "halfvec" does not
    /// exist`.
    halfvec: bool,
    /// Whether the installed `vector` extension has `hnsw.iterative_scan`,
    /// probed in the same query as [`Self::halfvec`].
    ///
    /// Only `!tuned_sessions` adapters consult it, and only to decide whether a
    /// search may `SET LOCAL` that setting at all; see
    /// [`Self::untuned_session_locals`].
    iterative_scan: bool,
}

/// Open bulk-load scopes, and per collection the points written in them and
/// whether its HNSW index was dropped for the rest of the load.
#[derive(Debug, Default)]
struct BulkLoad {
    depth: usize,
    written: HashMap<String, i64>,
    /// Collection -> vector dimension of the index to build at scope end.
    deferred: HashMap<String, usize>,
    /// Collection -> its row count when this bulk load first wrote to it.
    ///
    /// The deferral decision needs the collection's size, and reading it per
    /// batch is a `count(*)` per batch: an incremental re-cognify into a large
    /// collection in small batches never reaches `BULK_DEFER_RATIO x total`, so
    /// it pays a scan-sized count for every batch and never even gets the
    /// deferral — the exact opposite of the intent, in the scenario the scope
    /// exists for. One count per collection per load is enough, because the
    /// rows added since are what `written` already tracks (see
    /// [`PgVectorAdapter::bulk_should_defer`]).
    base_rows: HashMap<String, i64>,
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
        let mut opts = ConnectOptions::new(database_url.to_string());
        opts.map_sqlx_postgres_opts(|o| {
            o.options([
                ("hnsw.ef_search", HNSW_EF_SEARCH_SESSION.to_string()),
                // pgvector 0.8+: when the beam runs dry before the LIMIT —
                // dead tuples after deletes, or a degenerate graph over
                // near-identical vectors (the `a_search_returns_top_k_rows…`
                // test: 33 of 100 rows at ef_search = 200 with the scan off
                // *and* with `strict_order`) — keep scanning instead of
                // silently returning fewer rows. `relaxed_order` may emit
                // rows slightly out of distance order, so the searches
                // re-sort their (at most `top_k`) rows. Older pgvector
                // ignores the unknown placeholder setting.
                ("hnsw.iterative_scan", "relaxed_order".to_string()),
                // Custom plans: sqlx caches each prepared statement per
                // connection, and a generic plan cannot see the NodeSet
                // array of `search_similar_filtered`, so it costs the GIN
                // prefilter blind and fell back to a sequential scan of every
                // wide chunk row — 40 ms p50 for a 2k-of-14k-row filter at
                // 100k, where the custom plan's bitmap scan takes 4.4 ms.
                ("plan_cache_mode", "force_custom_plan".to_string()),
                // These last two are session-wide only because this pool is
                // ours to open. `from_connection` has no such hook, so its
                // searches re-apply the same two per statement —
                // `ITERATIVE_SCAN_LOCAL` / `PLAN_CACHE_LOCAL`, via
                // `untuned_session_locals`. Keep the pairs in step.
            ])
        });
        let db = Database::connect(opts)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector connect failed: {e}")))?;

        cleanup_legacy_seaql_migrations(&db).await?;
        migrator::Migrator::up(&db, None)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector migration failed: {e}")))?;

        debug!("PgVectorAdapter initialised (dimension={dimension})");
        let features = probe_pgvector_features(&db).await;
        Ok(Self {
            db,
            dimension,
            owns_pool: true,
            tuned_sessions: true,
            known: RwLock::new(HashSet::new()),
            bulk: std::sync::Mutex::new(BulkLoad::default()),
            halfvec: features.halfvec,
            iterative_scan: features.iterative_scan,
        })
    }

    /// Wrap an existing SeaORM `DatabaseConnection` (must be Postgres).
    ///
    /// The caller is responsible for ensuring the database already exists
    /// (the connection proves it does). Only the pgvector extension and
    /// bookkeeping table are created if missing.
    ///
    /// The session tuning [`Self::new`] puts in its pool's connection options
    /// cannot be applied here — the pool is the caller's — so the searches carry
    /// it per statement instead (`tuned_sessions: false`; see
    /// [`Self::untuned_session_locals`]). An adapter built this way answers
    /// identically to one from [`Self::new`]; it just pays a transaction per
    /// search for the privilege.
    pub async fn from_connection(db: DatabaseConnection, dimension: usize) -> VectorDBResult<Self> {
        cleanup_legacy_seaql_migrations(&db).await?;
        migrator::Migrator::up(&db, None)
            .await
            .map_err(|e| VectorDBError::StorageError(format!("PGVector migration failed: {e}")))?;

        let features = probe_pgvector_features(&db).await;
        Ok(Self {
            db,
            dimension,
            owns_pool: false,
            tuned_sessions: false,
            known: RwLock::new(HashSet::new()),
            bulk: std::sync::Mutex::new(BulkLoad::default()),
            halfvec: features.halfvec,
            iterative_scan: features.iterative_scan,
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
        // A bulk load cut short by close() still leaves every collection
        // indexed *and* analysed — the same two steps `end_bulk_load` runs, in
        // the same order, because close() is the other way a load can end
        // (`BulkLoadGuard`'s `Drop` closes the scope without being able to
        // await either of them). Skipping the `ANALYZE` would leave a
        // collection filled faster than autovacuum's naptime planned from
        // `reltuples = -1` or a count from its first batch, which is the exact
        // problem that step was added for.
        let pending = self.take_deferred(true);
        let written = self.take_written(true);
        let built = self.build_deferred(pending).await;
        let analyzed = self.analyze(written).await;
        let maintained = built.and(analyzed);
        if !self.owns_pool {
            maintained?;
            debug!("PgVectorAdapter::close is a no-op for a caller-owned connection");
            return Ok(());
        }
        let closed =
            self.db.close_by_ref().await.map_err(|e| {
                VectorDBError::StorageError(format!("PGVector pool close failed: {e}"))
            });
        maintained?;
        closed
    }

    /// Deferred index builds to run now: all of them when `force` (close) or
    /// when the last open scope just ended.
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn take_deferred(&self, force: bool) -> Vec<(String, usize)> {
        // lock poison is unrecoverable
        let mut b = self.bulk.lock().expect("bulk-load state lock");
        if !force && b.depth > 0 {
            return Vec::new();
        }
        if force {
            b.depth = 0;
        }
        let mut out: Vec<(String, usize)> = b.deferred.drain().collect();
        out.sort();
        out
    }

    /// Collections written in the bulk-load scopes that just ended (all
    /// open scopes when `force`), for the end-of-load `ANALYZE`.
    ///
    /// Also drops the cached starting row counts: they are per load, and the
    /// next one must count again against a collection this one has grown.
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn take_written(&self, force: bool) -> Vec<String> {
        // lock poison is unrecoverable
        let mut b = self.bulk.lock().expect("bulk-load state lock");
        if !force && b.depth > 0 {
            return Vec::new();
        }
        b.base_rows.clear();
        let mut out: Vec<String> = b.written.drain().map(|(k, _)| k).collect();
        out.sort();
        out
    }

    /// `ANALYZE` each of `colls`: a collection filled faster than
    /// autovacuum's naptime is otherwise planned with `reltuples = -1` or a
    /// count from its first batch.
    async fn analyze(&self, colls: Vec<String>) -> VectorDBResult<()> {
        for coll in colls {
            self.db
                .execute_unprepared(&format!(r#"ANALYZE "{coll}""#))
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    /// Build the HNSW index of every collection whose index a bulk load
    /// dropped: one in-memory parallel build each. All are attempted; the
    /// first error is returned (a collection left unindexed still answers
    /// every search by exact scan, and `create_missing_vector_indexes`
    /// repairs it).
    async fn build_deferred(&self, pending: Vec<(String, usize)>) -> VectorDBResult<()> {
        let storage = |e: sea_orm::DbErr| VectorDBError::StorageError(e.to_string());
        let mut first_err = None;
        for (coll, dimension) in pending {
            let res: VectorDBResult<()> = async {
                let rows = self
                    .db
                    .query_one(Statement::from_string(
                        DatabaseBackend::Postgres,
                        format!(r#"SELECT count(*) AS n FROM "{coll}""#),
                    ))
                    .await
                    .map_err(storage)?
                    .and_then(|r| r.try_get::<i64>("", "n").ok())
                    .unwrap_or(0);
                let txn = self.db.begin().await.map_err(storage)?;
                txn.execute_unprepared(&format!(
                    "SET LOCAL maintenance_work_mem = '{}kB'; \
                     SET LOCAL max_parallel_maintenance_workers = {}",
                    Self::hnsw_build_budget_kb(rows, dimension),
                    tuning::maintenance_workers(BULK_BUILD_WORKERS)
                ))
                .await
                .map_err(storage)?;
                txn.execute_unprepared(&Self::vector_index_ddl(
                    &coll,
                    dimension,
                    false,
                    self.halfvec,
                ))
                .await
                .map_err(storage)?;
                txn.commit().await.map_err(storage)?;
                debug!("bulk load: built HNSW index on {coll} ({rows} rows)");
                Ok(())
            }
            .await;
            if let Err(e) = res {
                warn!("bulk load: HNSW build on {coll} failed: {e}");
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Inside a bulk-load scope: whether `coll` (HNSW-indexed, `base_rows`
    /// rows when this load first touched it) should stop maintaining its index
    /// now that `incoming` more points are being written; if so it is recorded
    /// as deferred and the caller drops the index. `None` outside a scope.
    ///
    /// The collection's *current* size is taken as `base_rows + written`, not
    /// re-counted: `written` is the points this load has handed it, so the sum
    /// is exact when every point is a new row and an over-estimate when some
    /// are overwrites — and over-estimating only defers later, which is the
    /// safe direction (the cost is bounded by one extra index build, as the
    /// [`BULK_DEFER_RATIO`] note explains). It reproduces what the per-batch
    /// `count(*)` returned in the all-new case exactly: `w >= 0.25 (base + w)`,
    /// i.e. a third of the collection's starting rows.
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn bulk_should_defer(
        &self,
        coll: &str,
        incoming: i64,
        base_rows: i64,
        dimension: usize,
    ) -> Option<bool> {
        // lock poison is unrecoverable
        let mut b = self.bulk.lock().expect("bulk-load state lock");
        if b.depth == 0 {
            return None;
        }
        if b.deferred.contains_key(coll) {
            return Some(false);
        }
        let w = b.written.entry(coll.to_string()).or_insert(0);
        *w = w.saturating_add(incoming);
        let written = *w;
        if bulk_defer_reached(written, base_rows) {
            b.deferred.insert(coll.to_string(), dimension);
            return Some(true);
        }
        Some(false)
    }

    /// `coll`'s row count when this bulk load first wrote to it, counted once
    /// and cached in the scope state ([`BulkLoad::base_rows`]).
    ///
    /// `None` outside a scope — the caller only asks inside one.
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    async fn bulk_base_rows(&self, coll: &str) -> VectorDBResult<i64> {
        {
            // lock poison is unrecoverable
            let b = self.bulk.lock().expect("bulk-load state lock");
            if let Some(&cached) = b.base_rows.get(coll) {
                return Ok(cached);
            }
        }
        let rows = self
            .db
            .query_one(Statement::from_string(
                DatabaseBackend::Postgres,
                format!(r#"SELECT count(*) AS n FROM "{coll}""#),
            ))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?
            .and_then(|r| r.try_get::<i64>("", "n").ok())
            .unwrap_or(0);
        // lock poison is unrecoverable
        self.bulk
            .lock()
            .expect("bulk-load state lock")
            .base_rows
            .entry(coll.to_string())
            .or_insert(rows);
        Ok(rows)
    }

    /// Remember `coll` as written inside the open bulk-load scope, if any.
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn note_bulk_write(&self, coll: &str) {
        // lock poison is unrecoverable
        let mut b = self.bulk.lock().expect("bulk-load state lock");
        if b.depth > 0 {
            b.written.entry(coll.to_string()).or_insert(0);
        }
    }

    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn is_deferred(&self, coll: &str) -> bool {
        // lock poison is unrecoverable
        self.bulk
            .lock()
            .expect("bulk-load state lock")
            .deferred
            .contains_key(coll)
    }

    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn is_known(&self, coll: &str) -> bool {
        // lock poison is unrecoverable
        self.known
            .read()
            .expect("collection cache lock")
            .contains(coll)
    }

    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn remember(&self, coll: &str) {
        // lock poison is unrecoverable
        self.known
            .write()
            .expect("collection cache lock")
            .insert(coll.to_string());
    }

    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn forget(&self, coll: &str) {
        // lock poison is unrecoverable
        self.known
            .write()
            .expect("collection cache lock")
            .remove(coll);
    }

    /// Whether `e` says `coll`'s table is gone; if so, evict it from the
    /// collection cache so the next `has_collection` asks the database.
    fn forget_if_missing(&self, coll: &str, e: &VectorDBError) -> bool {
        let msg = e.to_string();
        let missing = msg.contains("does not exist") && msg.contains(coll);
        if missing {
            self.forget(coll);
        }
        missing
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
    /// Two collections whose names agree in their first 50 characters therefore
    /// collide on one index name. That is inherited from the collection names
    /// themselves — they are table names under the same limit — so it is not
    /// introduced here.
    ///
    /// `halfvec` picks which of the two index shapes is being named, and the
    /// names must differ because the *definitions* do: a `_halfvec_hnsw` built
    /// against the old full-precision ordering would never be used, and
    /// `CREATE INDEX IF NOT EXISTS` would see the name and leave it. On a
    /// pgvector older than [`HALFVEC_MIN_VERSION`] the name is the legacy
    /// `_vector_hnsw` — which is also exactly the index such a store already
    /// carries from before the half-precision change, so it is reused rather
    /// than rebuilt.
    fn vector_index_name(coll: &str, halfvec: bool) -> String {
        // `_halfvec_hnsw`: an expression index over `vector::halfvec(dim)`
        // (see [`Self::vector_index_ddl`]); the new name makes
        // `create_missing_vector_indexes` build it on stores that still carry
        // the full-precision `_vector_hnsw` index, which the halfvec-ordered
        // searches no longer use.
        Self::index_name(
            coll,
            if halfvec {
                HNSW_HALFVEC_SUFFIX
            } else {
                HNSW_VECTOR_SUFFIX
            },
        )
    }

    /// `coll` plus `suffix`, cut to [`PG_MAX_IDENTIFIER_BYTES`] by trimming the
    /// *collection* part — never the suffix.
    ///
    /// Appending first and truncating afterwards ate the suffix instead, and
    /// the two suffixes this adapter uses then met: at a 62-byte collection
    /// name both the HNSW and the GIN index came out as `coll + "_"`, and at 63
    /// bytes both came out as the table's own name. The consequences are
    /// silent, because `CREATE INDEX IF NOT EXISTS` only reports a *NOTICE*
    /// when the name is taken — the GIN membership index is simply never built,
    /// every filtered search scans, and `backfill_set_names_index` probing the
    /// same name finds the valid HNSW index, answers `Some(true)` and so
    /// `create_missing_vector_indexes` can never repair it. (At 63 bytes the
    /// name collides with the table, so neither index is ever built.)
    ///
    /// Reserving the suffix instead makes a collision between two *kinds* of
    /// index impossible by construction: each name ends in its own suffix.
    /// Slicing bytes is safe because [`Self::validate_identifier`] has already
    /// restricted the name to ASCII `[A-Za-z0-9_]`.
    fn index_name(coll: &str, suffix: &str) -> String {
        let mut name = coll.to_string();
        name.truncate(PG_MAX_IDENTIFIER_BYTES.saturating_sub(suffix.len()));
        name.push_str(suffix);
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
    ///
    /// On top of that, everything [`Self::untuned_session_locals`] carries for a
    /// connection this adapter did not open.
    ///
    /// `None` when no setting is needed at all, which is the common case:
    /// `tuned_sessions` pools open every connection with
    /// `hnsw.ef_search = HNSW_EF_SEARCH_SESSION` (and the rest of the tuning)
    /// already, so an ANN search up to that `top_k` is one plain statement
    /// rather than `BEGIN; SET LOCAL …; SELECT; COMMIT`. Pure in
    /// `tuned_sessions` so the decision is testable without a server.
    fn ann_search_locals(
        tuned_sessions: bool,
        iterative_scan: bool,
        top_k: usize,
    ) -> Option<String> {
        let mut locals: Vec<String> = Vec::new();
        if top_k > HNSW_EF_SEARCH_MAX {
            locals.push(Self::exact_scan_locals().to_string());
        } else if !tuned_sessions || top_k.saturating_mul(HNSW_EF_PER_K) > HNSW_EF_SEARCH_SESSION {
            let ef = top_k
                .saturating_mul(HNSW_EF_PER_K)
                .clamp(HNSW_EF_SEARCH_DEFAULT, HNSW_EF_SEARCH_MAX);
            locals.push(format!("SET LOCAL hnsw.ef_search = {ef}"));
        }
        locals.extend(
            Self::untuned_session_locals(tuned_sessions, iterative_scan)
                .iter()
                .map(|s| (*s).to_string()),
        );
        (!locals.is_empty()).then(|| locals.join("; "))
    }

    /// The `SET LOCAL` equivalents of the connection options [`Self::new`]
    /// applies to its own pool, for a connection it did not open.
    ///
    /// [`Self::from_connection`] has no hook to widen: the caller opened the
    /// connections, and in the single-shared-Postgres layout that caller is the
    /// relational store, so its `ConnectOptions` are not ours to change. A plain
    /// `SET` would be worse than nothing — it outlives the statement and leaks
    /// to whatever borrows that pooled connection next. So the settings ride
    /// along in the transaction the searches already open for `ef_search`, which
    /// is why this costs no extra round trip *and* no extra transaction: with
    /// `tuned_sessions == false`, [`Self::ann_search_locals`] can never return
    /// `None` anyway.
    ///
    /// Both have a named consequence when missing, which is why they are not
    /// left to the `new()` pool (the constructor's own comments measure them):
    ///
    /// - `hnsw.iterative_scan = relaxed_order`: without it an HNSW scan that
    ///   runs its beam dry before the `LIMIT` — dead tuples after deletes, or a
    ///   degenerate graph over near-identical vectors — returns *fewer* than
    ///   `top_k` rows and says nothing. Raising `ef_search` does not fix that
    ///   case; `a_search_returns_top_k_rows_even_above_the_default_ef_search`
    ///   measured 33 of 100 rows at `ef_search = 200` without it.
    /// - `plan_cache_mode = force_custom_plan`: a generic plan cannot see the
    ///   `NodeSet` array of [`VectorDB::search_similar_filtered`], costs the GIN
    ///   prefilter blind and falls back to scanning every wide chunk row — 40 ms
    ///   p50 against the custom plan's 4.4 ms, measured on a 2k-of-14k-row
    ///   filter at 100k rows.
    ///
    /// Empty for `tuned_sessions`, where the pool's connection options already
    /// carry both and repeating them would cost the searches a transaction they
    /// do not otherwise need.
    ///
    /// `iterative_scan` is [`PgVectorFeatures::iterative_scan`] and is a hard
    /// gate, not a preference: below [`ITERATIVE_SCAN_MIN_VERSION`] that `SET
    /// LOCAL` is an error rather than a no-op, and it would take the search down
    /// with it. `plan_cache_mode` is core Postgres (12+), so it needs no gate.
    fn untuned_session_locals(
        tuned_sessions: bool,
        iterative_scan: bool,
    ) -> &'static [&'static str] {
        match (tuned_sessions, iterative_scan) {
            (true, _) => &[],
            (false, true) => &[ITERATIVE_SCAN_LOCAL, PLAN_CACHE_LOCAL],
            (false, false) => &[PLAN_CACHE_LOCAL],
        }
    }

    /// `SET LOCAL`s for [`VectorDB::search_similar_filtered`], which needs none
    /// of the ANN levers — it keeps the HNSW index out of the query on purpose —
    /// but is the very path `plan_cache_mode = force_custom_plan` exists for.
    ///
    /// `None` for this adapter's own pools, so the filtered search stays the
    /// single plain statement it is today.
    fn filtered_search_locals(tuned_sessions: bool) -> Option<String> {
        (!tuned_sessions).then(|| PLAN_CACHE_LOCAL.to_string())
    }

    /// [`Self::query_all_with_locals`] when `locals` is `Some`, else one plain
    /// statement.
    async fn query_all_maybe_locals(
        &self,
        locals: Option<String>,
        stmt: Statement,
    ) -> VectorDBResult<Vec<sea_orm::QueryResult>> {
        match locals {
            Some(locals) => self.query_all_with_locals(&locals, stmt).await,
            None => self
                .db
                .query_all(stmt)
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string())),
        }
    }

    /// The `ORDER BY` expression an ANN similarity search selects candidates
    /// with, over `dim`-dimensional vectors at `top_k`, against the bound query
    /// vector `param` (`"$1"`, or a correlated column on the batch path).
    ///
    /// Half-precision **only where an HNSW index can actually be used**. The
    /// fp16 cast exists to match the `halfvec_cosine_ops` index; it costs ~0.1%
    /// of the distance, which is a fine trade against an approximate index but
    /// not against a path this adapter *declares exact*, where it would reorder
    /// rows at the `LIMIT` boundary for nothing. The outer `ORDER BY score DESC`
    /// does not repair that — it only re-sorts a candidate set already chosen.
    /// Four cases, and in all four the plain `vector <=> …` is both exact and
    /// no slower, because there is no index to match:
    ///
    /// - `dim > MAX_INDEXABLE_DIMENSION`: pgvector cannot index the collection,
    ///   so it is on the sequential scan `backends.md` promises returns the true
    ///   top k.
    /// - `top_k > HNSW_EF_SEARCH_MAX`: [`Self::ann_search_locals`] deliberately
    ///   returns [`Self::exact_scan_locals`] to guarantee the true top-k, which
    ///   an fp16 ordering would then quietly spoil at the boundary.
    /// - No `halfvec` type at all (pgvector below [`HALFVEC_MIN_VERSION`]),
    ///   where the index is the full-precision one and the cast would not even
    ///   parse.
    /// - `indexless`: this collection's index is dropped right now, for a
    ///   bulk-load scope this adapter opened ([`Self::is_deferred`]). That is
    ///   the case [`VectorDB::begin_bulk_load`] documents as "a hint, never a
    ///   semantic change … a backend that defers index maintenance serves
    ///   searches by exact scan meanwhile" — so the scan has to *be* exact, and
    ///   an fp16 candidate order would make entering a bulk load reorder rows
    ///   at the `LIMIT` boundary, which is precisely what that contract
    ///   forbids. The flag costs one uncontended in-memory mutex (the
    ///   per-collection state `upsert_points` already reads on every batch), not
    ///   a round trip.
    ///
    /// Residual, still not covered: a collection whose index was *never* built —
    /// a `create_collection` whose best-effort `CREATE INDEX` failed, or rows
    /// written before this adapter grew an index — answers by exact scan while
    /// still being ordered in fp16. That one is in the catalog, not in memory,
    /// so a search cannot know it without a round trip; it is outside the
    /// bulk-load contract, bounded by the same ~0.1% boundary effect, and
    /// `create_missing_vector_indexes` is the documented repair.
    fn candidate_order_expr(
        param: &str,
        dim: usize,
        top_k: usize,
        halfvec: bool,
        indexless: bool,
    ) -> String {
        if halfvec && !indexless && dim <= MAX_INDEXABLE_DIMENSION && top_k <= HNSW_EF_SEARCH_MAX {
            format!("vector::halfvec({dim}) <=> {param}::halfvec({dim})")
        } else {
            format!("vector <=> {param}::vector")
        }
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
    /// The opclass is a cosine one because every search site orders by the
    /// cosine operator `<=>`; an opclass that does not match the operator *and
    /// the expression* in the `ORDER BY` is simply never used by the planner,
    /// which would leave the sequential scan in place while looking like it had
    /// been fixed. `halfvec` picks which pair — `halfvec_cosine_ops` over
    /// `vector::halfvec(n)`, or `vector_cosine_ops` over the column — and must
    /// be the caller's `self.halfvec`, so that the index and the searches agree.
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
        halfvec: bool,
    ) -> VectorDBResult<bool> {
        if dimension > MAX_INDEXABLE_DIMENSION {
            debug!(
                "collection {coll} is {dimension}-dimensional, over pgvector's \
                 {MAX_INDEXABLE_DIMENSION}-d index ceiling — keeping the exact scan"
            );
            return Ok(false);
        }

        let index = Self::vector_index_name(coll, halfvec);
        let ddl = Self::vector_index_ddl(coll, dimension, concurrently, halfvec);

        db.execute_unprepared(&ddl)
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        // "ensured", not "created": the statement carries `IF NOT EXISTS`, so it
        // is a no-op when the index is already there — including when two
        // backfills race — and claiming a creation would misreport that.
        debug!("ensured HNSW index {index} on {coll} (dim={dimension})");
        Ok(true)
    }

    /// Name of the GIN index over `cognee_vector_set_names(metadata)`,
    /// trimmed like [`Self::vector_index_name`] — see [`Self::index_name`] for
    /// why the suffix is the part that must survive.
    fn set_names_index_name(coll: &str) -> String {
        Self::index_name(coll, SET_NAMES_SUFFIX)
    }

    /// `CREATE INDEX` for `coll`'s NodeSet-membership GIN index.
    fn set_names_index_ddl(coll: &str, concurrently: bool) -> String {
        let index = Self::set_names_index_name(coll);
        let concurrent_kw = if concurrently { " CONCURRENTLY" } else { "" };
        format!(
            r#"CREATE INDEX{concurrent_kw} IF NOT EXISTS "{index}"
               ON "{coll}" USING gin (cognee_vector_set_names(metadata))"#
        )
    }

    /// `CREATE INDEX` for `coll`'s HNSW index (see [`Self::create_vector_index`]).
    ///
    /// The index is over `vector::halfvec(dimension)` — half-precision copies
    /// of the stored vectors — while the column keeps its `vector` type (the
    /// schema the Python SDK reads and writes). Searches order by the same
    /// expression to use it and score with the full-precision distance, then
    /// re-sort, so only candidate selection sees fp16 (~0.1% of the distance).
    /// Measured on a 209k-row 384-d EdgeType index: 233 MB vs 408 MB and
    /// ~25% faster top-100 scans.
    ///
    /// `halfvec = false` — a `vector` extension older than
    /// [`HALFVEC_MIN_VERSION`], where the type does not exist — builds the
    /// full-precision index under the legacy `_vector_hnsw` name instead, which
    /// is what [`Self::candidate_order_expr`] orders by on that version.
    fn vector_index_ddl(coll: &str, dimension: usize, concurrently: bool, halfvec: bool) -> String {
        let index = Self::vector_index_name(coll, halfvec);
        let concurrent_kw = if concurrently { " CONCURRENTLY" } else { "" };
        // Without `halfvec` (pgvector < 0.7) the half-precision cast does not
        // even parse, so the index goes over the column itself with
        // `vector_cosine_ops` — matching the `vector <=> $1::vector` ordering
        // the searches use on that version.
        let column = if halfvec {
            format!("(vector::halfvec({dimension})) halfvec_cosine_ops")
        } else {
            "vector vector_cosine_ops".to_string()
        };
        format!(
            r#"CREATE INDEX{concurrent_kw} IF NOT EXISTS "{index}"
               ON "{coll}" USING hnsw ({column})
               WITH (m = {m}, ef_construction = {ef})"#,
            m = tuning::hnsw_m(),
            ef = tuning::hnsw_ef_construction(),
        )
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
    pub async fn create_missing_vector_indexes(&self) -> VectorDBResult<VectorIndexBackfill> {
        let query = Query::select()
            .columns([VColl::CollectionName, VColl::Dimension])
            .from(VColl::Table)
            .to_owned();

        let rows = self
            .db
            .query_all(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        let mut report = VectorIndexBackfill::default();
        for row in &rows {
            // Skip a malformed bookkeeping row rather than propagating, so one
            // bad entry cannot leave every collection after it unindexed —
            // that is what this method's contract promises, and returning
            // `Err` here broke it.
            let coll: String = match row.try_get("", "collection_name") {
                Ok(coll) => coll,
                Err(e) => {
                    warn!(
                        "could not read a collection name from vector_collections, skipping that row: {e}"
                    );
                    report.failed += 1;
                    continue;
                }
            };
            let dimension: i32 = match row.try_get("", "dimension") {
                Ok(dim) => dim,
                Err(e) => {
                    warn!("could not read the dimension for collection {coll}, skipping it: {e}");
                    report.failed += 1;
                    continue;
                }
            };

            // A row in the bookkeeping table came from `create_collection`, which
            // validated the name before creating the table — but re-validate
            // rather than trust the table, since this name is interpolated into
            // DDL and the table is reachable by anything with the connection.
            if let Err(e) = Self::validate_identifier(&coll) {
                warn!("collection name {coll} is not a safe identifier, skipping it: {e}");
                report.failed += 1;
                continue;
            }

            // The NodeSet-membership GIN index (collections created before it
            // existed have none). Independent of the HNSW index below: it
            // applies at any dimension.
            self.backfill_set_names_index(&coll, &mut report).await;

            // Check first rather than leaning on `IF NOT EXISTS`, so the count
            // reports work actually done and an already-indexed collection is
            // not handed a redundant CONCURRENTLY build.
            let index = Self::vector_index_name(&coll, self.halfvec);
            let state = match Self::vector_index_state(&self.db, &index).await {
                Ok(state) => state,
                Err(e) => {
                    warn!("could not read index state for collection {coll}, skipping it: {e}");
                    report.failed += 1;
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
                    report.failed += 1;
                    continue;
                }
            }

            match Self::create_vector_index(
                &self.db,
                &coll,
                dimension.max(0) as usize,
                true,
                self.halfvec,
            )
            .await
            {
                Ok(true) => report.built += 1,
                Ok(false) => {}
                Err(e) => {
                    warn!("could not index collection {coll}, skipping it: {e}");
                    report.failed += 1;
                }
            }
        }

        Ok(report)
    }

    /// Build `coll`'s NodeSet-membership GIN index if it is missing or was
    /// left invalid, the way the HNSW backfill does (`CONCURRENTLY`, logged and
    /// counted as failed rather than aborting the run).
    async fn backfill_set_names_index(&self, coll: &str, report: &mut VectorIndexBackfill) {
        let index = Self::set_names_index_name(coll);
        let state = match Self::vector_index_state(&self.db, &index).await {
            Ok(state) => state,
            Err(e) => {
                warn!("could not read the state of {index}, skipping it: {e}");
                report.failed += 1;
                return;
            }
        };
        if state == Some(true) {
            return;
        }
        if state == Some(false)
            && let Err(e) = self
                .db
                .execute_unprepared(&format!(r#"DROP INDEX CONCURRENTLY "{index}""#))
                .await
        {
            warn!("could not drop invalid index {index}, skipping it: {e}");
            report.failed += 1;
            return;
        }
        match self
            .db
            .execute_unprepared(&Self::set_names_index_ddl(coll, true))
            .await
        {
            Ok(_) => report.built += 1,
            Err(e) => {
                warn!("could not build {index}: {e}");
                report.failed += 1;
            }
        }
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

    /// Batched upsert shared by [`VectorDB::index_points`] (`merge_membership`)
    /// and [`VectorDB::upsert_raw_vectors`] (verbatim replace).
    ///
    /// One multi-row `INSERT … ON CONFLICT` per [`WRITE_BATCH`] points, with
    /// the vector bound as a binary `real[]` (cast to `vector`) rather than a
    /// formatted text literal. With `merge_membership` the stored row's
    /// dataset membership is unioned in server-side ([`MERGED_METADATA`]), so
    /// the per-batch read-before-write of the old path is gone.
    ///
    /// A conflicting row is only rewritten when its vector or metadata
    /// actually changes: a no-op update still writes a new heap tuple *and*
    /// a new HNSW entry (an index insert is the single most expensive part of
    /// an upsert), and cognify re-indexes every recurring point.
    ///
    /// Duplicate ids in one call are folded in input order first — one
    /// statement cannot touch the same `ON CONFLICT` target twice (Postgres
    /// aborts it with "ON CONFLICT DO UPDATE command cannot affect row a
    /// second time") — through the same [`crate::models::dedup_points_by_id`]
    /// / [`crate::models::dedup_points_by_id_last_wins`] helpers the LanceDB
    /// adapter uses, so the outcome equals applying the points one by one.
    async fn upsert_points(
        &self,
        coll: &str,
        points: &[VectorPoint],
        merge_membership: bool,
    ) -> VectorDBResult<()> {
        // The shared in-batch fold (`crate::models`), which the LanceDB
        // adapter uses too: one point per distinct id, first-appearance order,
        // last occurrence winning, with `index_points` additionally unioning
        // the duplicates' dataset membership. It runs here, before the
        // rebuild decision and before `write_points` batches, so the row
        // counts the decision is made on are the rows actually written and an
        // id repeated across a batch boundary is still written exactly once.
        let points = &if merge_membership {
            dedup_points_by_id(points)
        } else {
            dedup_points_by_id_last_wins(points)
        };

        let dimension = points.first().map_or(0, |p| p.vector.len());
        self.note_bulk_write(coll);
        if dimension <= MAX_INDEXABLE_DIMENSION && self.is_deferred(coll) {
            // Index dropped for the rest of this bulk load.
            return self
                .write_points_concurrently(coll, points, merge_membership)
                .await;
        }
        if dimension <= MAX_INDEXABLE_DIMENSION
            && self.bulk.lock().map(|b| b.depth > 0).unwrap_or(false)
        {
            let index = Self::vector_index_name(coll, self.halfvec);
            if Self::vector_index_state(&self.db, &index).await? == Some(true) {
                // Counted once per collection per load, not per batch.
                let base_rows = self.bulk_base_rows(coll).await?;
                let incoming = i64::try_from(points.len()).unwrap_or(i64::MAX);
                if self.bulk_should_defer(coll, incoming, base_rows, dimension) == Some(true) {
                    self.db
                        .execute_unprepared(&format!(r#"DROP INDEX IF EXISTS "{index}""#))
                        .await
                        .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
                    debug!(
                        "bulk load: dropped HNSW index on {coll} ({base_rows} rows at the start \
                         of the load) until the load ends"
                    );
                }
                return self
                    .write_points_concurrently(coll, points, merge_membership)
                    .await;
            }
        }
        if points.len() >= HNSW_REBUILD_MIN_ROWS && dimension <= MAX_INDEXABLE_DIMENSION {
            let index = Self::vector_index_name(coll, self.halfvec);
            if Self::vector_index_state(&self.db, &index).await? == Some(true) {
                return self
                    .upsert_rebuilding_index(coll, &index, points, merge_membership)
                    .await;
            }
        }
        if dimension <= MAX_INDEXABLE_DIMENSION {
            return self
                .write_points_concurrently(coll, points, merge_membership)
                .await;
        }
        Self::write_points(&self.db, coll, points, merge_membership).await
    }

    /// [`Self::write_points`] for a large batch into an HNSW-indexed
    /// collection: if the batch would add at least [`HNSW_REBUILD_RATIO`] x
    /// the collection's current rows to the index, drop the index, write the
    /// rows, and build it again — all in one transaction.
    ///
    /// An incremental HNSW insert is a graph search plus neighbour-page
    /// rewrites per row (~1.7 ms at 20k 384-d rows here), while `CREATE
    /// INDEX` builds in memory and in parallel (~0.15 ms per row), so for a
    /// bulk load a rebuild is ~10x cheaper. The rebuild holds an `ACCESS
    /// EXCLUSIVE` lock on the collection until commit, so concurrent searches
    /// of *this* collection wait for the load instead of reading a
    /// half-indexed table; a failure rolls the whole thing back, index
    /// included. The index is rebuilt under the same name and parameters.
    async fn upsert_rebuilding_index(
        &self,
        coll: &str,
        index: &str,
        points: &[VectorPoint],
        merge_membership: bool,
    ) -> VectorDBResult<()> {
        let storage = |e: sea_orm::DbErr| VectorDBError::StorageError(e.to_string());
        let ids: Vec<Uuid> = points.iter().map(|p| p.id).collect();
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                format!(
                    r#"SELECT (SELECT count(*) FROM "{coll}") AS total,
                              (SELECT count(*) FROM "{coll}" WHERE id = ANY($1::uuid[])) AS present"#
                ),
                [ids.into()],
            ))
            .await
            .map_err(storage)?;
        let (total, present) = match row {
            Some(r) => (
                r.try_get::<i64>("", "total").map_err(storage)?,
                r.try_get::<i64>("", "present").map_err(storage)?,
            ),
            None => (0, 0),
        };
        // Index inserts this batch can cause: every new row plus, as an upper
        // bound, every existing row it rewrites (an update writes a new tuple
        // and so a new index entry). A row whose vector and metadata are
        // unchanged is skipped by `write_points` and costs none, so this
        // over-counts re-indexed points; the ratio leaves room for that.
        let incoming = i64::try_from(points.len()).unwrap_or(i64::MAX);
        if (incoming as f64) < HNSW_REBUILD_RATIO * total as f64 {
            return self
                .write_points_concurrently(coll, points, merge_membership)
                .await;
        }

        let rows_after = total.saturating_add(incoming.saturating_sub(present));
        let dimension = points.first().map_or(0, |p| p.vector.len());
        let txn = self.db.begin().await.map_err(storage)?;
        txn.execute_unprepared(&format!(r#"DROP INDEX "{index}""#))
            .await
            .map_err(storage)?;
        Self::write_points(&txn, coll, points, merge_membership).await?;
        txn.execute_unprepared(&format!(
            "SET LOCAL maintenance_work_mem = '{}kB'; \
             SET LOCAL max_parallel_maintenance_workers = {}",
            Self::hnsw_build_budget_kb(rows_after, dimension),
            tuning::maintenance_workers(HNSW_BUILD_WORKERS)
        ))
        .await
        .map_err(storage)?;
        txn.execute_unprepared(&Self::vector_index_ddl(
            coll,
            dimension,
            false,
            self.halfvec,
        ))
        .await
        .map_err(storage)?;
        txn.commit().await.map_err(storage)?;
        debug!(
            "rebuilt HNSW index {index} on {coll} after a {incoming}-point load ({rows_after} rows)"
        );
        Ok(())
    }

    /// `maintenance_work_mem` (kB) for an in-memory HNSW build of `rows`
    /// `dimension`-d vectors: the vector plus `2 * m` neighbour slots and
    /// element overhead per row, with headroom, clamped to
    /// [`HNSW_BUILD_BUDGET_MIN_KB`, `HNSW_BUILD_BUDGET_MAX_KB`]. A build that
    /// outgrows the budget does not fail — pgvector finishes it on disk, much
    /// more slowly — so this only has to be roughly right.
    fn hnsw_build_budget_kb(rows: i64, dimension: usize) -> i64 {
        let per_row = (dimension as i64) * 4 + i64::from(tuning::hnsw_m()) * 2 * 10 + 200;
        let bytes = rows.max(0).saturating_mul(per_row).saturating_mul(3) / 2;
        (bytes / 1024).clamp(HNSW_BUILD_BUDGET_MIN_KB, tuning::build_budget_max_kb())
    }

    /// Write `points` into `coll` (see [`Self::upsert_points`]) on `conn`.
    /// `points` must already be folded to one entry per distinct id —
    /// [`Self::upsert_points`] does that for every caller.
    async fn write_points<C: ConnectionTrait>(
        conn: &C,
        coll: &str,
        points: &[VectorPoint],
        merge_membership: bool,
    ) -> VectorDBResult<()> {
        for stmt in Self::upsert_statements(coll, points, merge_membership, WRITE_BATCH) {
            conn.execute(stmt)
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    /// [`Self::write_points`] on the pool, as up to [`HNSW_INSERT_WRITERS`]
    /// statements in flight at once, for batches that go into a live HNSW
    /// index.
    ///
    /// pgvector maintains the index inside each inserting backend, one row
    /// at a time (~1.2 ms per 384-d row into a 209k-row EdgeType index here),
    /// and concurrent backends insert in parallel: 3 000 rows took 3.65 s
    /// from one statement, 1.53 s from four and no less from eight. The
    /// statements touch disjoint ids (duplicates are folded first) and each
    /// commits on its own — exactly as the sequential chunks already did — so
    /// running them concurrently changes no outcome; the first error is
    /// returned once all have finished.
    async fn write_points_concurrently(
        &self,
        coll: &str,
        points: &[VectorPoint],
        merge_membership: bool,
    ) -> VectorDBResult<()> {
        use futures_util::StreamExt;
        let per_writer = points.len().div_ceil(HNSW_INSERT_WRITERS);
        let batch = per_writer.clamp(HNSW_INSERT_MIN_BATCH, WRITE_BATCH);
        let stmts = Self::upsert_statements(coll, points, merge_membership, batch);
        if stmts.len() <= 1 {
            for stmt in stmts {
                self.db
                    .execute(stmt)
                    .await
                    .map_err(|e| VectorDBError::StorageError(e.to_string()))?;
            }
            return Ok(());
        }
        let results: Vec<Result<sea_orm::ExecResult, sea_orm::DbErr>> =
            futures_util::stream::iter(stmts.into_iter().map(|stmt| self.db.execute(stmt)))
                .buffer_unordered(HNSW_INSERT_WRITERS)
                .collect()
                .await;
        for r in results {
            r.map_err(|e| VectorDBError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    /// The upsert statements for `points` (see [`Self::upsert_points`]): one
    /// multi-row `INSERT … ON CONFLICT` per `batch` ids. `points` must already
    /// be folded to one entry per distinct id — [`Self::upsert_points`] does
    /// that for every caller — so the statements touch disjoint rows.
    fn upsert_statements(
        coll: &str,
        points: &[VectorPoint],
        merge_membership: bool,
        batch: usize,
    ) -> Vec<Statement> {
        let metadata = if merge_membership {
            MERGED_METADATA
        } else {
            "EXCLUDED.metadata"
        };
        let on_conflict = format!(
            " ON CONFLICT (id) DO UPDATE SET vector = EXCLUDED.vector, metadata = {metadata} \
             WHERE t.vector IS DISTINCT FROM EXCLUDED.vector \
                OR t.metadata IS DISTINCT FROM {metadata}"
        );
        let mut stmts = Vec::with_capacity(points.len().div_ceil(batch.max(1)));
        for chunk in points.chunks(batch.max(1)) {
            let mut sql = format!(r#"INSERT INTO "{coll}" AS t (id, vector, metadata) VALUES "#);
            let mut values: Vec<sea_orm::Value> = Vec::with_capacity(chunk.len() * 3);
            for pt in chunk {
                let pt = pt.clone();
                let p = values.len();
                if p > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!(
                    "(${}::uuid, ${}::real[]::vector, ${}::jsonb)",
                    p + 1,
                    p + 2,
                    p + 3
                ));
                values.push(pt.id.into());
                values.push(pt.vector.into());
                // Chunk and summary text is injected into point metadata, so
                // this `jsonb` cast has the same NUL exposure as the graph
                // tables — see `cognee_utils::sanitize`.
                values.push(
                    sanitize_json(serde_json::Value::Object(pt.metadata.into_iter().collect()))
                        .into(),
                );
            }
            sql.push_str(&on_conflict);
            stmts.push(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &sql,
                values,
            ));
        }
        stmts
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

    /// Delegates to the inherent
    /// [`PgVectorAdapter::create_missing_vector_indexes`], so a holder of an
    /// `Arc<dyn VectorDB>` — the CLI's `vector-reindex`, which never learns
    /// which backend it was handed — can run the backfill without downcasting.
    /// The inherent function stays, and `cognee` re-exports the concrete type,
    /// so an embedder that already has a `PgVectorAdapter` keeps calling it
    /// directly.
    async fn create_missing_vector_indexes(&self) -> VectorDBResult<VectorIndexBackfill> {
        PgVectorAdapter::create_missing_vector_indexes(self).await
    }

    /// Opens a bulk-load scope: from here until the matching
    /// [`end_bulk_load`](VectorDB::end_bulk_load), a collection that has
    /// taken [`BULK_DEFER_RATIO`] x its rows in this scope has its HNSW index
    /// dropped (searches of it run as exact scans meanwhile, so results stay
    /// correct) and rebuilt once, in parallel, when the last scope ends.
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    async fn begin_bulk_load(&self) -> VectorDBResult<()> {
        // lock poison is unrecoverable
        self.bulk.lock().expect("bulk-load state lock").depth += 1;
        Ok(())
    }

    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    async fn end_bulk_load(&self) -> VectorDBResult<()> {
        {
            // lock poison is unrecoverable
            let mut b = self.bulk.lock().expect("bulk-load state lock");
            b.depth = b.depth.saturating_sub(1);
        }
        let written = self.take_written(false);
        let pending = self.take_deferred(false);
        let built = self.build_deferred(pending).await;
        let analyzed = self.analyze(written).await;
        built.and(analyzed)
    }

    /// Close the scope without running the deferred work — the `Drop` half of
    /// the pairing (see [`BulkLoadGuard`](crate::BulkLoadGuard)).
    ///
    /// Only the depth is reconciled here, and that is the point: a depth stuck
    /// above zero is unrecoverable, because every later batch then takes the
    /// `b.depth > 0` branch of [`Self::upsert_points`], drops the index of any
    /// collection past [`BULK_DEFER_RATIO`] and never reaches an
    /// `end_bulk_load` to build it again. The `deferred` / `written` maps are
    /// deliberately left in place: a `Drop` cannot await, so the index build
    /// and the `ANALYZE` wait for the next scope's `end_bulk_load`, for
    /// [`Self::close`], or for `create_missing_vector_indexes`. Until then the
    /// affected collections answer by exact scan, which is correct — that is
    /// the same state an interrupted load leaves, and it is pinned by
    /// `an_interrupted_bulk_load_leaves_correct_rows_that_the_backfill_reindexes`.
    #[allow(clippy::expect_used, reason = "lock poison is unrecoverable")]
    fn abandon_bulk_load(&self) {
        // lock poison is unrecoverable
        let mut b = self.bulk.lock().expect("bulk-load state lock");
        b.depth = b.depth.saturating_sub(1);
        if b.depth == 0 && !b.deferred.is_empty() {
            warn!(
                collections = b.deferred.len(),
                "bulk load abandoned (dropped future or panic): the deferred HNSW \
                 builds are queued for the next bulk load or close(); searches stay \
                 correct by exact scan meanwhile"
            );
        }
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
        //
        // Vectors are kept in the heap row rather than TOASTed: pgvector gives
        // the column `EXTERNAL` storage, so a 384-d vector (1.5 kB) next to a
        // chunk's text in `metadata` crosses the 2 kB TOAST threshold and is
        // moved out of line. Every sequential scan (the exact filtered search,
        // and plain searches of small collections, which the planner rightly
        // serves by scan + sort) then fetches each vector from the TOAST table,
        // and the planner, which does not cost detoasting, cannot see it:
        // measured on 8.6k Entity rows, a scan + top-100 sort took 14-16 ms
        // TOASTed vs 3.7-4.8 ms inline, and an HNSW build 1.6 s vs 0.66 s.
        // `toast_tuple_target = 8160` (the maximum) leaves any row that fits
        // in a page alone; `STORAGE MAIN` makes the vector the last thing moved
        // out when a row does not. Sent as one simple-query string, so the two
        // statements commit or fail together.
        let ddl = format!(
            r#"CREATE TABLE "{coll}" (
                id UUID PRIMARY KEY,
                vector vector({dimension}),
                metadata JSONB NOT NULL DEFAULT '{{}}'
            ) WITH (toast_tuple_target = 8160);
            ALTER TABLE "{coll}" ALTER COLUMN vector SET STORAGE MAIN"#
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
        if let Err(e) =
            Self::create_vector_index(&self.db, &coll, dimension, false, self.halfvec).await
        {
            warn!(
                "collection {coll} was created without an ANN index, so its searches will \
                 be sequential scans until the backfill runs — `cognee-cli vector-reindex`, \
                 or create_missing_vector_indexes(): {e}"
            );
        }

        // NodeSet-membership index for `search_similar_filtered`. Best-effort
        // for the same reason as the HNSW index: the filtered search is exact
        // with or without it, only slower, and the backfill adds it later.
        if let Err(e) = self
            .db
            .execute_unprepared(&Self::set_names_index_ddl(&coll, false))
            .await
        {
            warn!(
                "collection {coll} was created without its NodeSet index, so filtered searches \
                 scan it until the backfill runs: {e}"
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

        self.remember(&coll);
        debug!("created collection {coll} (dim={dimension})");
        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let coll = Self::collection_name(data_type, field_name)?;
        if self.is_known(&coll) {
            return Ok(true);
        }

        let inner = Query::select()
            .expr(Expr::val(1))
            .from(VColl::Table)
            .and_where(Expr::col(VColl::CollectionName).eq(coll.clone()))
            .to_owned();

        let query = Query::select()
            .expr_as(Expr::exists(inner), Alias::new("exists"))
            .to_owned();

        let row = self
            .db
            .query_one(self.build(&query))
            .await
            .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

        let exists = match row {
            Some(r) => r
                .try_get::<bool>("", "exists")
                .map_err(|e| VectorDBError::StorageError(e.to_string()))?,
            None => false,
        };
        if exists {
            self.remember(&coll);
        }
        Ok(exists)
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

        // Point IDs are content-addressed, so the same point is re-indexed
        // once per dataset. A plain `metadata = EXCLUDED.metadata` overwrite
        // would drop earlier datasets' `dataset_id` (cross-dataset dedup bug),
        // so the stored membership is unioned in, mirroring the in-memory /
        // lancedb adapters and Python's union semantics.
        if let Err(e) = self.upsert_points(&coll, points, true).await {
            self.forget_if_missing(&coll, &e);
            return Err(e);
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

        // Unlike `index_points`, prior dataset membership is NOT unioned in;
        // the incoming metadata is written verbatim (full replace on conflict).
        if let Err(e) = self.upsert_points(&coll, points, false).await {
            // The self-create guard above asks `has_collection`, which answers
            // from a positive-only cache: a collection dropped behind this
            // adapter's back — a second adapter, the CLI, the Python SDK — is
            // still remembered, so the guard skips the create and the write
            // fails on a table that is not there. Before the cache that case
            // was handled transparently, and for a system-owned collection like
            // `TruthCentroid_vector` nothing else ever creates it, so failing
            // once and recovering on the *next* call is a regression worth
            // undoing. `forget_if_missing` has just evicted it, so recreate and
            // retry exactly once; anything else propagates untouched.
            if !self.forget_if_missing(&coll, &e) {
                return Err(e);
            }
            match self
                .create_collection(data_type, field_name, expected_dim)
                .await
            {
                // Lost a race to recreate it: the collection is there either
                // way, which is all the retry needs.
                Ok(()) | Err(VectorDBError::CollectionExists(_)) => {}
                Err(create_err) => return Err(create_err),
            }
            self.upsert_points(&coll, points, false)
                .await
                .inspect_err(|e| {
                    self.forget_if_missing(&coll, e);
                })?;
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
        // `LIMIT` is a literal, not a bind parameter: sqlx prepares and caches
        // the statement, and after five executions Postgres may switch it to a
        // generic plan, which has to cost `LIMIT $2` without knowing the value
        // and assumes 10% of the table. From a few thousand rows that makes a
        // sequential scan plus sort look cheaper than the HNSW index scan —
        // measured 15–23 ms vs 1.7 ms on a 4 320-row Entity collection. A
        // literal keeps the generic plan on the index. `top_k` is a `usize`,
        // so interpolating it carries no injection risk.
        let limit = i64::try_from(top_k).unwrap_or(i64::MAX);
        let dim = query_vector.len();
        // The outer ORDER BY restores exact distance order over the (at most
        // `top_k`) rows an iterative `relaxed_order` scan returns.
        let order =
            Self::candidate_order_expr("$1", dim, top_k, self.halfvec, self.is_deferred(&coll));
        let sql = format!(
            r#"SELECT id, score, metadata FROM (
                 SELECT id, 1 - (vector <=> $1::vector) AS score, metadata
                 FROM "{coll}"
                 ORDER BY {order}
                 LIMIT {limit}) r
               ORDER BY {NAN_LAST}, score DESC"#
        );

        // `ef_search` must cover `top_k`, or the index scan ends early and the
        // LIMIT is silently unmet — see `ann_search_locals`.
        let rows = self
            .query_all_maybe_locals(
                Self::ann_search_locals(self.tuned_sessions, self.iterative_scan, top_k),
                Statement::from_sql_and_values(DatabaseBackend::Postgres, &sql, [vec_str.into()]),
            )
            .await
            .inspect_err(|e| {
                self.forget_if_missing(&coll, e);
            })?;

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
        // Exact filter-then-limit, as one statement. The NodeSet predicate
        // runs *before* the ORDER BY distance LIMIT, so every returned row is
        // in-set and none is crowded out — exact at any size. It is phrased
        // over `cognee_vector_set_names(metadata)` (see the migrator), which
        // the collection's GIN index answers directly: `&&` = any requested
        // name (OR), `@>` = all of them (AND), with exactly
        // `node_filter::metadata_matches_node_filter`'s semantics.
        //
        // The HNSW index must stay out of this query: pgvector post-filters an
        // index scan (it yields roughly `hnsw.ef_search` candidates by distance
        // alone and the WHERE clause is applied to *those*), so a selective
        // filter would return fewer than `top_k` rows, or none —
        // `test_search_similar_filtered_filter_then_limit` pins that with 64
        // zero-distance out-of-set rows crowding 2 in-set ones. Ordering by
        // `distance + 0` is not an expression the HNSW opclass can order by, so
        // the planner cannot pick the index scan, while the GIN bitmap scan
        // (the old `SET LOCAL enable_bitmapscan = off` ruled that out too)
        // stays available. `id` breaks distance ties deterministically.
        let predicate = if node_name_filter_operator == "AND" {
            "@>"
        } else {
            "&&"
        };
        let limit = i64::try_from(top_k).unwrap_or(i64::MAX);
        let sql = format!(
            r#"SELECT id, 1 - (vector <=> $1::vector) AS score, metadata
               FROM "{coll}"
               WHERE cognee_vector_set_names(metadata) {predicate} $2::text[]
               ORDER BY (vector <=> $1::vector) + 0, id
               LIMIT {limit}"#
        );
        // This is the query `plan_cache_mode = force_custom_plan` exists for —
        // the `$2::text[]` of names a generic plan has to cost blind. On this
        // adapter's own pools that is already a connection option, so
        // `filtered_search_locals` is `None` and the search stays one plain
        // statement; on a caller-owned connection it rides in as a `SET LOCAL`.
        let rows = self
            .query_all_maybe_locals(
                Self::filtered_search_locals(self.tuned_sessions),
                Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    &sql,
                    [vec_str.into(), requested.to_vec().into()],
                ),
            )
            .await
            .inspect_err(|e| {
                self.forget_if_missing(&coll, e);
            })?;

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

        // One `uuid[]` parameter per `ID_BATCH` ids: a constant statement
        // text (cached prepared statement) instead of a 100-placeholder
        // `IN (…)` list per round trip.
        let mut results = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(ID_BATCH) {
            let sql = format!(r#"SELECT id, metadata FROM "{coll}" WHERE id = ANY($1::uuid[])"#);
            let values: Vec<sea_orm::Value> = vec![chunk.to_vec().into()];
            let rows = match self
                .db
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    &sql,
                    values,
                ))
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))
            {
                Ok(rows) => rows,
                // Dropped since `has_collection` (cached) said it exists: the
                // same "missing → empty" answer as the pre-check — but only
                // for the ids this call had not reached yet. Earlier `ID_BATCH`
                // chunks really did find their rows, and `vec![]` reported that
                // partial success as total absence, which reads to a caller as
                // "none of these ids exist" rather than "the collection went
                // away mid-read". Returning what was found keeps the documented
                // "ids not present are silently absent" semantics.
                Err(e) if self.forget_if_missing(&coll, &e) => return Ok(results),
                Err(e) => return Err(e),
            };
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
        let dim = query_vectors.first().map_or(self.dimension, Vec::len);
        let array_literal = query_vectors
            .iter()
            .map(|v| format!("'{}'::vector", Self::format_vector(v)))
            .collect::<Vec<_>>()
            .join(", ");
        let order =
            Self::candidate_order_expr("q.vec", dim, top_k, self.halfvec, self.is_deferred(&coll));

        let sql = format!(
            r#"SELECT q.idx AS idx, t.id AS id, t.score AS score, t.metadata AS metadata
               FROM unnest(ARRAY[{array_literal}]) WITH ORDINALITY AS q(vec, idx)
               CROSS JOIN LATERAL (
                   SELECT id, 1 - (vector <=> q.vec) AS score, metadata
                   FROM "{coll}"
                   ORDER BY {order}
                   LIMIT {top_k}
               ) t
               ORDER BY q.idx, (t.score = 'NaN'::float8), t.score DESC"#
        );

        // Each LATERAL subquery is its own index scan with its own `LIMIT
        // {top_k}`, so this path needs the same `ef_search` floor as
        // `search_similar` or every one of them ends early.
        let rows = self
            .query_all_maybe_locals(
                Self::ann_search_locals(self.tuned_sessions, self.iterative_scan, top_k),
                Statement::from_string(DatabaseBackend::Postgres, sql),
            )
            .await
            .inspect_err(|e| {
                self.forget_if_missing(&coll, e);
            })?;

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

        // Evicted first: whatever happens below, the next `has_collection`
        // asks the database.
        self.forget(&coll);
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

        // `= ANY($1::uuid[])` per `ID_BATCH` ids rather than one bind
        // parameter per id, which failed past PostgreSQL's 65 535.
        for chunk in point_ids.chunks(ID_BATCH) {
            self.db
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    format!(r#"DELETE FROM "{coll}" WHERE id = ANY($1::uuid[])"#),
                    [chunk.to_vec().into()],
                ))
                .await
                .map_err(|e| VectorDBError::StorageError(e.to_string()))
                .inspect_err(|e| {
                    self.forget_if_missing(&coll, e);
                })?;
        }

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
            vec![
                Box::new(CreatePgVectorExtension),
                Box::new(CreateSetNamesFunction),
            ]
        }
    }

    /// `cognee_vector_set_names(metadata)`: the NodeSet names of a point's
    /// `belongs_to_set` under `crate::node_filter`'s semantics — a bare string
    /// entry is its own name, an object contributes its `name` when that is a
    /// string, anything else (and a missing / non-array `belongs_to_set`)
    /// contributes nothing. IMMUTABLE, so each collection carries a GIN
    /// expression index over it and `search_similar_filtered` becomes an
    /// index lookup (`&&` for OR, `@>` for AND) instead of a
    /// `jsonb_array_elements` scan of every row.
    struct CreateSetNamesFunction;

    impl MigrationName for CreateSetNamesFunction {
        fn name(&self) -> &str {
            "m20260929_000001_create_set_names_function"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for CreateSetNamesFunction {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .get_connection()
                .execute_unprepared(super::SET_NAMES_FUNCTION_DDL)
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .get_connection()
                .execute_unprepared("DROP FUNCTION IF EXISTS cognee_vector_set_names(jsonb)")
                .await?;
            Ok(())
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
            assert_eq!(version_count(&db, "seaql_migrations_pgvector").await, 2);

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
