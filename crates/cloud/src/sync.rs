//! Cloud-side flow for `POST /api/v1/sync`.
//!
//! Ports Python's [`_perform_background_sync`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L167-L229).
//!
//! This module is intentionally minimal: the HTTP-server handler builds the
//! `SyncTask` description (run_id, datasets, user) and hands it off to
//! [`run_background`]. The repository write hooks (`mark_started`,
//! `mark_completed`, `mark_failed`, `update_progress`) are invoked through a
//! [`SyncReporter`] trait so `cognee-cloud` does not need to depend on
//! `cognee-database`.
//!
//! The progress-callback `Arc<dyn Fn(u32) + Send + Sync>` keeps the in-memory
//! `SyncRegistry` (HTTP-server) and the persistent `sync_operations` row in
//! lock-step without `cognee-cloud` knowing about either.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Description of a single dataset being synced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetInfo {
    pub id: Uuid,
    pub name: String,
}

/// Reporter callback: the hooks needed to update the durable `sync_operations`
/// row from inside [`run_background`]. Implemented by an HTTP-server adapter
/// over the `SyncOperationRepository`.
#[async_trait]
pub trait SyncReporter: Send + Sync {
    async fn mark_started(&self, run_id: &str) -> Result<(), String>;
    async fn mark_completed(
        &self,
        run_id: &str,
        records_uploaded: i32,
        records_downloaded: i32,
        bytes_uploaded: i64,
        bytes_downloaded: i64,
    ) -> Result<(), String>;
    async fn mark_failed(&self, run_id: &str, error_message: &str) -> Result<(), String>;
    async fn update_progress(&self, run_id: &str, percent: u32) -> Result<(), String>;
}

/// Tick callback signaled when sync progress changes (in-memory + durable
/// updates fan out from the same `Arc<dyn Fn>`).
pub type ProgressCallback = Arc<dyn Fn(u32) + Send + Sync>;

/// Run the background sync pipeline.
///
/// **This is a no-op stub.** It marks the run started, ticks progress through
/// `[0, 80, 90, 95, 100]`, and marks it completed with zero records/bytes
/// transferred. No data is actually moved — the `POST /api/v1/sync` HTTP
/// route advertises a working sync endpoint but none of the diff/upload/
/// download/cognify orchestration is implemented yet.
///
/// The completion payload honestly reports zero records and zero bytes so
/// callers cannot conclude from the response body that data was transferred.
///
/// Full sync implementation (diff, upload, download, cognify) is deferred to
/// a future release. Tracked in `docs/roadmap/not-implemented.md`.
pub async fn run_background(
    run_id: String,
    _datasets: Vec<DatasetInfo>,
    _user_id: Uuid,
    reporter: Arc<dyn SyncReporter>,
    progress: ProgressCallback,
) -> Result<(), String> {
    if let Err(e) = reporter.mark_started(&run_id).await {
        tracing::warn!(error = %e, "sync mark_started failed (non-fatal)");
    }

    for pct in [0_u32, 80, 90, 95, 100] {
        progress(pct);
        if let Err(e) = reporter.update_progress(&run_id, pct).await {
            tracing::warn!(error = %e, "sync update_progress failed (non-fatal)");
        }
    }

    if let Err(e) = reporter.mark_completed(&run_id, 0, 0, 0, 0).await {
        tracing::warn!(error = %e, "sync mark_completed failed (non-fatal)");
    }
    Ok(())
}
