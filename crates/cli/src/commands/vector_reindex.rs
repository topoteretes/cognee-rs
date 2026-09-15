//! Backfill the pgvector ANN index onto collections that lack one.
//!
//! The index is built by `create_collection`, not by a migration, which leaves
//! two ways for a collection to end up without one:
//!
//! 1. It predates the index existing at all (collections created before
//!    SDK-515), so it has only its btree primary key.
//! 2. It was created *after*, but the index build failed. That build is
//!    best-effort by design — propagating the error would leave the table
//!    created and unregistered, and every retry would then fail at
//!    `CREATE TABLE` with `already exists`, wedging the collection for good. So
//!    it logs a warning and continues: pgvector too old for the `hnsw` access
//!    method, a role without the rights, too little `maintenance_work_mem`.
//!
//! Neither case is visible from the outside. The collection answers every query
//! correctly, by sequential scan; only latency changes. This command is the
//! repair for both, and the only one — the backfill is an adapter method with
//! no caller outside tests until now.
//!
//! It does the work rather than reporting it first. `pipeline-unblock` reports
//! first because it cannot tell a dead claim holder from a live run and only
//! the operator can; there is no such ambiguity here. The build is online,
//! idempotent, and skips every collection that already has a usable index, so a
//! report-only mode would just cost a second invocation to reach the same
//! place.

use std::sync::Arc;

use cognee::{ComponentManager, PipelineContext};
use tracing::info;

use crate::cli::VectorReindexArgs;
use crate::error::CliError;

/// Build the missing ANN indexes and report how many were built.
pub fn run(_args: VectorReindexArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    // Read the provider before any async work, both so the settings read guard
    // is dropped before the awaits below (clippy::await_holding_lock) and so
    // the non-pgvector case never opens a connection it has no use for.
    let provider = cm.settings().vector_db_provider.to_lowercase();

    // Every other backend inherits the defaulted trait method, which returns
    // `Ok(0)`. Reporting that as "0 indexes created" would be true and useless
    // — indistinguishable from a pgvector store that was already fully indexed
    // — so name the reason instead. The runtime default is `lancedb`, so this
    // is the branch most invocations take.
    if provider != "pgvector" {
        info!(
            "Vector backend is '{provider}', which has no ANN index to backfill — \
             nothing to do. This command exists for the pgvector backend, whose \
             index is built per collection and can be missing. Set \
             VECTOR_DB_PROVIDER=pgvector (or `cognee-cli config set \
             vector_db_provider '\"pgvector\"'`) to point it at one."
        );
        return Ok(());
    }

    crate::teardown::run_command(Arc::clone(&cm), async move {
        let vector_db = cm
            .vector_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        info!(
            "Building missing vector indexes. This runs online (CREATE INDEX \
             CONCURRENTLY) and does not block reads or writes, but it can take a \
             long time on a large collection."
        );

        // Called directly on the trait object, with nothing wrapping it in a
        // transaction: the adapter issues `CREATE INDEX CONCURRENTLY` on the
        // pool, which Postgres rejects inside one.
        let created = vector_db
            .create_missing_vector_indexes()
            .await
            .map_err(|error| CliError::Runtime(format!("Vector reindex failed: {error}")))?;

        if created == 0 {
            info!(
                "No vector collection was missing an index — every collection is \
                 already indexed, or is too wide for pgvector to index (over 2000 \
                 dimensions) and keeps its exact scan."
            );
        } else {
            info!("Built {created} missing vector index(es).");
        }

        Ok(())
    })
}
