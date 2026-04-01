use async_trait::async_trait;
use uuid::Uuid;

use crate::task::TaskError;
/// Per-data-item status tracking for incremental pipeline execution.
///
/// The executor queries [`is_completed`](ExecStatusManager::is_completed) before
/// processing each item and calls [`mark_completed`](ExecStatusManager::mark_completed)
/// / [`mark_failed`](ExecStatusManager::mark_failed) afterwards.  This enables
/// safe resume after partial failures and prevents re-processing on re-runs.
///
/// Separate from [`PipelineWatcher`](crate::pipeline::PipelineWatcher) by design:
/// the watcher is a write-only observer, while this trait is bidirectional (the
/// executor reads `is_completed` to decide whether to skip).
#[async_trait]
pub trait ExecStatusManager: Send + Sync {
    /// Returns `true` if this item was already successfully processed
    /// for the given `(pipeline_name, dataset_id)` combination.
    async fn is_completed(
        &self,
        data_id: &str,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
    ) -> Result<bool, TaskError>;

    /// Mark the item as successfully completed.
    async fn mark_completed(
        &self,
        data_id: &str,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
    ) -> Result<(), TaskError>;

    /// Mark the item as failed (used for diagnostics / resume).
    async fn mark_failed(
        &self,
        data_id: &str,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
        error: &str,
    ) -> Result<(), TaskError>;

    /// Record provenance metadata for a processed data point.
    ///
    /// Called by the executor after each task succeeds (point 9 — provenance
    /// stamping).  `node_set` is an opaque label identifying the set of graph
    /// nodes produced by the task.
    async fn stamp_provenance(
        &self,
        data_id: &str,
        pipeline_name: &str,
        task_name: &str,
        user_id: Option<Uuid>,
        node_set: Option<&str>,
    ) -> Result<(), TaskError>;
}
/// No-op implementation used when incremental loading is disabled.
///
/// `is_completed` always returns `false` (process everything), all writes are
/// silent successes.
pub struct NoopExecStatusManager;

#[async_trait]
impl ExecStatusManager for NoopExecStatusManager {
    async fn is_completed(
        &self,
        _data_id: &str,
        _pipeline_name: &str,
        _dataset_id: Option<Uuid>,
    ) -> Result<bool, TaskError> {
        Ok(false)
    }

    async fn mark_completed(
        &self,
        _data_id: &str,
        _pipeline_name: &str,
        _dataset_id: Option<Uuid>,
    ) -> Result<(), TaskError> {
        Ok(())
    }

    async fn mark_failed(
        &self,
        _data_id: &str,
        _pipeline_name: &str,
        _dataset_id: Option<Uuid>,
        _error: &str,
    ) -> Result<(), TaskError> {
        Ok(())
    }

    async fn stamp_provenance(
        &self,
        _data_id: &str,
        _pipeline_name: &str,
        _task_name: &str,
        _user_id: Option<Uuid>,
        _node_set: Option<&str>,
    ) -> Result<(), TaskError> {
        Ok(())
    }
}
