//! Server startup and shutdown lifecycle hooks.
//!
//! The closed `cognee-http-cloud` crate provides its own bootstrap that
//! seeds the `principals` / `users` / `tenants` tables; OSS keeps the
//! sync-registry sweep + pipeline-registry shutdown that are DB-free.

use thiserror::Error;
use uuid::Uuid;

/// Errors that can occur during server lifecycle transitions.
#[derive(Debug, Error)]
pub enum LifecycleError {
    /// Database migration failed.
    #[error("migration failed: {0}")]
    MigrationFailed(String),

    /// Bootstrap of default principals failed.
    #[error("bootstrap failed: {0}")]
    BootstrapFailed(String),
}

/// All-zero UUID — matches Python's `default_user_id`.
const DEFAULT_USER_ID_HEX: &str = "00000000000000000000000000000000";

/// How long [`on_shutdown`] waits for already-dispatched telemetry POSTs to leave
/// the process. Deliberately short: a SIGTERM must not be held up by an analytics
/// collector.
#[cfg(feature = "telemetry")]
const TELEMETRY_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// How long any single store close may take before shutdown moves on.
///
/// Generous — a large embedded checkpoint is legitimately slow — but finite. A
/// pool close waits for its checked-out connections to come back, so a task
/// parked in a driver read, or a handler that escaped the pipeline registry, would
/// otherwise hold SIGTERM open until the supervisor loses patience and sends
/// SIGKILL. Being killed is strictly worse than skipping one close: the kill
/// skips *every* remaining close, plus the telemetry flush.
const STORE_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Await one store's close under [`STORE_CLOSE_TIMEOUT`], logging the outcome.
///
/// Never propagates: shutdown continues to the next store either way, which is the
/// whole reason each one is bounded separately rather than the sequence as a whole.
async fn bounded<E: std::fmt::Display>(
    what: &str,
    close: impl std::future::Future<Output = Result<(), E>>,
) {
    match tokio::time::timeout(STORE_CLOSE_TIMEOUT, close).await {
        Ok(Ok(())) => tracing::info!("{what} closed"),
        Ok(Err(e)) => tracing::warn!("closing the {what} failed (non-fatal): {e}"),
        Err(_) => tracing::warn!(
            "closing the {what} did not finish within {STORE_CLOSE_TIMEOUT:?}; \
             continuing shutdown so the remaining resources still get their turn"
        ),
    }
}

/// Called once before the router is handed to `axum::serve`.
///
/// OSS-side bootstrap is a no-op: the synthetic default user is
/// DB-free (no `principals`/`users`/`user_tenants` rows to seed). Closed
/// `cognee-http-cloud` provides its own startup hook that seeds the
/// `(default_user, default_tenant)` rows per `tenants.md §6`.
pub async fn on_startup(_state: &crate::state::AppState) -> Result<(), LifecycleError> {
    tracing::info!("Backend server has started");
    Ok(())
}

/// Convenience accessor — for callers that need the well-known IDs.
pub fn default_user_id() -> Uuid {
    Uuid::parse_str(DEFAULT_USER_ID_HEX).unwrap_or(Uuid::nil())
}

/// Called on graceful shutdown (SIGTERM / SIGINT).
///
/// Order is the whole design here, and it is the reverse of startup: **drain the
/// work first, then close the stores.** Closing a store while the pipeline
/// registry still has tasks running would make every in-flight cognify fail
/// against a closed handle and emit a burst of errors indistinguishable from a
/// crash — the exact failure the relational-close comment below was written to
/// avoid. So the registry shutdown and the sync abort stay first, and the graph /
/// vector / relational closes come after.
///
/// Closing through `&self` is what makes this possible at all: `lib.graph_db` and
/// `lib.vector_db` are `Arc` clones held by handlers and pipeline builders, so
/// there is nothing here to drop — and for a Postgres store a retained `Arc` is
/// precisely the case where the pool would otherwise stay open for the life of
/// the process.
///
/// # Deliberate limitation
///
/// [`crate::components::ComponentHandles`] stores its slots as plain
/// `Option<Arc<…>>`, so this hook **cannot** release the `reqwest` connection
/// pools behind `llm` / `embedding_engine` / `transcriber` / `responses_client`,
/// nor an ONNX session: doing so needs interior mutability in those fields, which
/// is a breaking change for embedders. The standalone binary exits immediately
/// after this returns, so the OS reclaims them; an **in-process embedder that
/// rebuilds the router without exiting keeps that gap**. Recorded here rather than
/// papered over with an interior-mutability layer.
///
/// Also pre-existing and worth knowing: without the `bin` feature there is no
/// graceful-shutdown wiring at all (see `lib.rs`), so this function never runs and
/// nothing is closed.
pub async fn on_shutdown(state: &crate::state::AppState) {
    tracing::info!("Backend server is shutting down");

    if let Err(e) = state.pipelines.shutdown().await {
        tracing::warn!("pipeline registry shutdown failed (non-fatal): {e}");
    } else {
        tracing::info!("pipeline registry shutdown complete");
    }

    // Abort every in-flight cloud sync — the durable-row "mark failed"
    // step moved closed alongside `SyncOperationRepository`.
    let aborted = state.sync.abort_all();
    if !aborted.is_empty() {
        tracing::info!(
            "aborted {} in-flight cloud sync(s) on shutdown",
            aborted.len()
        );
    }

    // Close the stores now that the work using them has drained.
    //
    // Dropping is not closing, and here there is nothing to drop anyway: these are
    // `Arc` clones held by handlers and pipeline builders, so a store has to be
    // closed through `&self` or not at all. The relational pool orphans its
    // `-wal`/`-shm` sidecars (topoteretes/cognee-rs#132), an embedded graph leaves
    // an un-checkpointed `<db>.wal` and a write lock on its file, and a Postgres
    // graph/vector adapter owns a pool that a retained `Arc` keeps open for the
    // life of the process. All three are no-ops for backends that own nothing
    // closable (the in-memory brute-force store, LanceDB).
    if let Some(lib) = state.lib.as_ref() {
        // Relational first, and every close bounded — see STORE_CLOSE_TIMEOUT and
        // the ordering note in this function's docs.
        bounded("relational database", cognee_database::close(&lib.database)).await;
        if let Some(graph) = lib.graph_db.as_ref() {
            bounded("graph database", graph.close()).await;
        }
        if let Some(vector) = lib.vector_db.as_ref() {
            bounded("vector database", vector.close()).await;
        }
    }

    // Last of all, let the analytics POSTs that are already in flight finish.
    // `send_telemetry` is fire-and-forget, so the shutdown event itself — the one
    // that says why the server stopped — is otherwise discarded when the process
    // exits (measured: 0 of 1 delivered without a flush, 1 of 1 with one).
    // Hard-bounded: a slow or blackholed collector must never hold up a SIGTERM.
    #[cfg(feature = "telemetry")]
    if !cognee_telemetry::flush(TELEMETRY_FLUSH_TIMEOUT).await {
        tracing::debug!(
            "telemetry still in flight after {TELEMETRY_FLUSH_TIMEOUT:?}; \
             dropping the remainder rather than delaying shutdown"
        );
    }
}
