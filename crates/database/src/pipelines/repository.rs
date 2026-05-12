use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::types::{DatabaseError, PipelineRun, PipelineRunStatus};

/// Domain alias used in the trait signature.
pub type PipelineRunRow = PipelineRun;
/// Type alias for the database error used in this module.
type DbError = DatabaseError;

/// Row returned by [`PipelineRunRepository::list_recent_with_attribution`].
///
/// Joins `pipeline_runs ⨝ datasets ⨝ users` so the activity router can show
/// "who/what/which dataset" attribution in one query (no N+1).
#[derive(Debug, Clone)]
pub struct PipelineRunWithAttributionRow {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub status: PipelineRunStatus,
    pub pipeline_run_id: Uuid,
    pub pipeline_name: String,
    pub pipeline_id: Uuid,
    pub dataset_id: Option<Uuid>,
    pub dataset_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub owner_email: Option<String>,
}

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

    /// Recent runs *with attribution* (dataset + owner). Powers
    /// `GET /api/v1/activity/pipeline-runs`. Single SELECT joining
    /// `pipeline_runs ⨝ datasets ⨝ users` (LEFT JOIN both ways so orphaned
    /// runs whose dataset has been deleted still surface).
    ///
    /// Optional `dataset_id` narrows to a single dataset; `None` returns
    /// rows across every dataset on the server.
    ///
    /// Default impl falls back to [`Self::list_recent`] without the join — used
    /// only by mock implementations.
    async fn list_recent_with_attribution(
        &self,
        dataset_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<PipelineRunWithAttributionRow>, DbError> {
        let rows = self.list_recent(dataset_id, limit).await?;
        Ok(rows
            .into_iter()
            .map(|r| PipelineRunWithAttributionRow {
                id: r.id,
                created_at: r.created_at,
                status: r.status,
                pipeline_run_id: r.pipeline_run_id,
                pipeline_name: r.pipeline_name,
                pipeline_id: r.pipeline_id,
                dataset_id: r.dataset_id,
                dataset_name: None,
                owner_id: None,
                owner_email: None,
            })
            .collect())
    }

    /// Restart-orphan reset: rewrite any row stuck in `INITIATED` / `STARTED`
    /// without a more recent successor to `ERRORED` with the given `reason`.
    ///
    /// Returns the number of rows rewritten.
    async fn reset_orphans(&self, reason: &str) -> Result<u64, DbError>;

    /// Upsert a single payload field for a run. Concurrent calls with the
    /// same `(run_id, key)` are last-write-wins per row; calls with different
    /// keys do not contend.
    async fn set_payload_field(
        &self,
        run_id: Uuid,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), DbError>;

    /// Read all payload fields for a run as a `serde_json::Map`. Returns an
    /// empty map (not `None`) when the run has no payload events; returns
    /// `Err` only on actual DB failures.
    async fn get_payload(
        &self,
        run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DbError>;

    /// Return one `(pipeline_name, latest_status)` pair per distinct pipeline
    /// name that has at least one row for `dataset_id`. "Latest" is by
    /// `created_at DESC`.
    ///
    /// Used by `cognee_lib::api::pipeline_runs::reset_dataset_pipeline_run_status`
    /// to decide which `(dataset_id, pipeline_name)` pairs need a fresh
    /// `INITIATED` row, skipping ones that are already pending. Will be
    /// superseded by `get_pipeline_runs_by_dataset` once action item 08-06
    /// lands; see [docs/telemetry/08/05-reset-helpers.md §3](../../../docs/telemetry/08/05-reset-helpers.md).
    async fn list_pipeline_names_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<(String, PipelineRunStatus)>, DbError>;
}
