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
    // Read the provider before any async work, so the settings read guard is
    // dropped before the awaits below (clippy::await_holding_lock). It is only
    // used to make the "nothing to do" message concrete — the decision itself
    // belongs to the backend, not to this string.
    let provider = cm.settings().vector_db_provider.to_lowercase();

    crate::teardown::run_command(Arc::clone(&cm), async move {
        let vector_db = cm
            .vector_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        info!(
            "Building any missing vector indexes. On pgvector this runs online \
             (CREATE INDEX CONCURRENTLY) and does not block reads or writes, but \
             it can take a long time on a large collection."
        );

        // Dispatch through the trait rather than gating on the provider string.
        // The defaulted method answers `Ok(0)` for a backend that has no ANN
        // index to build, and an out-of-tree adapter registered through the
        // component registry may override it. Comparing the provider name here
        // would tell such a backend it has no repair path — precisely the gap
        // this command exists to close. Called directly on the trait object,
        // with nothing wrapping it in a transaction: the pgvector adapter
        // issues `CREATE INDEX CONCURRENTLY` on the pool, which Postgres
        // rejects inside one.
        let report = vector_db
            .create_missing_vector_indexes()
            .await
            .map_err(|error| CliError::Runtime(format!("Vector reindex failed: {error}")))?;

        if report.built > 0 {
            info!("Built {} missing vector index(es).", report.built);
        }

        // A zero build count on its own is ambiguous: it is what a healthy,
        // fully-indexed store reports and equally what a store reports when
        // every build failed. Reporting the latter as success would tell an
        // operator their only repair path worked when it repaired nothing, so
        // the failures decide the exit status.
        if report.failed > 0 {
            return Err(CliError::Runtime(format!(
                "{} collection(s) still have no vector index — each was logged \
                 above with its reason. Common causes: the configured role \
                 cannot CREATE INDEX, or maintenance_work_mem is too small for \
                 the build. Fix the cause and re-run; the pass is idempotent, \
                 so the collections that did succeed are not rebuilt.",
                report.failed
            )));
        }

        if report.built == 0 {
            info!(
                "No vector index needed building. Either every collection on \
                 the '{provider}' backend is already indexed, or that backend \
                 builds no ANN index of its own — of the bundled backends only \
                 pgvector does, and its index is per collection, so it can go \
                 missing. A pgvector collection over 2000 dimensions also keeps \
                 its exact scan and is skipped."
            );
        }

        Ok(())
    })
}
