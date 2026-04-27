//! Server startup and shutdown lifecycle hooks.
//!
//! `on_startup` is called by `build_router` before returning the assembled
//! `Router`.  For P0 the body is minimal — database migration and principal
//! bootstrap land in later phases.

use thiserror::Error;

/// Errors that can occur during server lifecycle transitions.
#[derive(Debug, Error)]
pub enum LifecycleError {
    /// Database migration failed.
    #[error("migration failed: {0}")]
    MigrationFailed(String),
}

/// Called once before the router is handed to `axum::serve`.
///
/// P0: logs the startup message.  Actual SeaORM migrations and default-principal
/// bootstrap land in later phases when the `lib` slot on `AppState` is wired.
pub async fn on_startup(state: &crate::state::AppState) -> Result<(), LifecycleError> {
    // P1: run_startup_migrations(&state.lib.db()).await?;
    // P5: bootstrap_default_principals(&state.lib).await?;
    let _ = state; // suppress unused-variable warning until the field is wired
    tracing::debug!("startup migrations skipped: lib slot not yet wired");
    tracing::info!("Backend server has started");
    Ok(())
}

/// Called on graceful shutdown (SIGTERM / SIGINT).
///
/// P3: calls `state.pipelines.shutdown()` to abort in-flight runs and write
/// `DATASET_PROCESSING_ERRORED` rows so a restart doesn't show stale `STARTED`
/// state.  Per pipelines.md §12.
pub async fn on_shutdown(state: &crate::state::AppState) {
    tracing::info!("Backend server is shutting down");

    // Abort every in-flight pipeline run and write ERRORED rows.
    // The default RegistryConfig.abort_writes_errored_row=true causes a
    // DATASET_PROCESSING_ERRORED row to be inserted for each in-flight run with
    // run_info = {"reason": "server_shutdown"}.
    if let Err(e) = state.pipelines.shutdown().await {
        tracing::warn!("pipeline registry shutdown failed (non-fatal): {e}");
    } else {
        tracing::info!("pipeline registry shutdown complete");
    }
}
