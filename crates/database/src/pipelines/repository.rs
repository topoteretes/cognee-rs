use std::collections::HashMap;

use async_trait::async_trait;
use uuid::Uuid;

use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};

/// Domain alias used in the trait signature.
pub type PipelineRunRow = PipelineRun;
/// Type alias for the database error used in this module.
type DbError = DatabaseError;

/// Persistence abstraction for pipeline run status rows.
///
/// Each status transition writes a **new row** rather than updating in place,
/// giving a full audit trail and matching Python's writing pattern.
///
/// Implementations must be `Send + Sync` so they can be stored behind an
/// `Arc<dyn PipelineRunRepository>` and shared across async tasks.
#[async_trait]
pub trait PipelineRunRepository: Send + Sync {
    /// Insert one row representing a status transition. Returns the new row's
    /// primary key (`pipeline_runs.id`), which is a freshly generated UUIDv4.
    async fn log_pipeline_run(
        &self,
        pipeline_run_id: Uuid,
        pipeline_id: Uuid,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
        status: PipelineRunStatus,
        run_info: Option<serde_json::Value>,
    ) -> Result<Uuid, DbError>;

    /// Latest status per dataset for a given pipeline name.
    ///
    /// Returns a map from `dataset_id` to the most recent
    /// `PipelineRunStatus` row for that dataset and pipeline name.
    async fn latest_status(
        &self,
        dataset_ids: &[Uuid],
        pipeline_name: &str,
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DbError>;

    /// Recent runs for the activity router, with optional dataset filter.
    async fn list_recent(
        &self,
        dataset_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<PipelineRunRow>, DbError>;

    /// Restart-orphan reset: rewrite any row stuck in `INITIATED` / `STARTED`
    /// without a more recent successor to `ERRORED` with the given `reason`.
    ///
    /// Returns the number of rows rewritten.
    async fn reset_orphans(&self, reason: &str) -> Result<u64, DbError>;
}
