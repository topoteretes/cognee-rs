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
}
