//! [`PipelineWatcher`] that persists the four-state `pipeline_runs` trail
//! through [`PipelineRunRepository`] without an in-memory event channel.
//!
//! Used by library convenience functions (`cognee_cognify::cognify`,
//! `cognee_cognify::memify::memify`, `cognee_ingestion::AddPipeline::add`)
//! that do not flow through the http-server's `DefaultPipelineRunRegistry`
//! but still want the four-state audit trail (gap 08-07, locked decision 11).
//!
//! The HTTP server uses [`super::ScopedRunWatcher`] instead, which also
//! publishes to the in-memory `RunEvent` channel for live subscribers.
//!
//! # Repository write failures
//!
//! A repository write failure does **not** abort the pipeline — it is
//! logged via `tracing::warn!` and execution continues. Mirrors
//! [`super::ScopedRunWatcher`].

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use cognee_database::{PipelineRunRepository, PipelineRunStatus as DbStatus};

use crate::pipeline::{
    PipelineRunInfo, PipelineRunStatus as CoreStatus, PipelineStatus, PipelineWatcher, TaskStatus,
};

use super::{run_info_for_errored, run_info_for_initiated, run_info_for_running};

/// [`PipelineWatcher`] that writes `pipeline_runs` rows through a
/// [`PipelineRunRepository`]. Does **not** broadcast `RunEvent`s.
///
/// Construct one inside each library convenience function with the
/// caller-supplied `Arc<dyn PipelineRunRepository>` and pass it as the
/// `watcher` to [`crate::pipeline::execute`]. Embedded callers that don't
/// have a database pass `Arc::new(NoopPipelineRunRepository::new())`, so
/// the inner writes are cheap no-ops.
pub struct DbPipelineWatcher {
    repo: Arc<dyn PipelineRunRepository>,
}

impl DbPipelineWatcher {
    /// Create a watcher that persists through `repo`.
    pub fn new(repo: Arc<dyn PipelineRunRepository>) -> Self {
        Self { repo }
    }

    /// Mirrors `ScopedRunWatcher::core_to_db_status` — no cross-crate
    /// dependency from `cognee-database` back to `cognee-core`, so the
    /// translation lives at the seam.
    fn core_to_db_status(status: &CoreStatus) -> DbStatus {
        match status {
            CoreStatus::Initiated => DbStatus::Initiated,
            CoreStatus::Started => DbStatus::Started,
            CoreStatus::Completed => DbStatus::Completed,
            CoreStatus::Errored => DbStatus::Errored,
        }
    }
}

#[async_trait]
impl PipelineWatcher for DbPipelineWatcher {
    // ── Required no-op methods (DB-watcher doesn't care about per-task
    //    granularity; the four pipeline-run lifecycle hooks below cover
    //    everything `pipeline_runs` needs). ─────────────────────────────

    async fn on_pipeline(&self, _pipeline_id: Uuid, _status: PipelineStatus) {}

    async fn on_task(
        &self,
        _pipeline_id: Uuid,
        _task_index: usize,
        _task_name: Option<&str>,
        _total_tasks: usize,
        _status: TaskStatus,
    ) {
    }

    // ── Rich lifecycle events (DB persistence only — no event channel). ──

    async fn on_pipeline_run_initiated(&self, run: &PipelineRunInfo) {
        // Python parity: `run_info = {}` (per `log_pipeline_run_initiated.py`).
        let run_info = Some(run_info_for_initiated());
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                run.run_id,
                run.pipeline_id,
                &run.pipeline_name,
                run.dataset_id,
                DbStatus::Initiated,
                run_info,
            )
            .await
        {
            tracing::warn!(
                run_id = %run.run_id,
                "DbPipelineWatcher: DB write for Initiated failed (non-fatal): {e}"
            );
        }
    }

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        // Python parity: `run_info = {"data": data_info(data)}`.
        let run_info = Some(run_info_for_running(&run.data_ids));
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                run.run_id,
                run.pipeline_id,
                &run.pipeline_name,
                run.dataset_id,
                Self::core_to_db_status(&run.status),
                run_info,
            )
            .await
        {
            tracing::warn!(
                run_id = %run.run_id,
                "DbPipelineWatcher: DB write for Started failed (non-fatal): {e}"
            );
        }
    }

    async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, _output_count: usize) {
        // Python parity: same `{"data": data_info}` shape as STARTED.
        let run_info = Some(run_info_for_running(&run.data_ids));
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                run.run_id,
                run.pipeline_id,
                &run.pipeline_name,
                run.dataset_id,
                DbStatus::Completed,
                run_info,
            )
            .await
        {
            tracing::warn!(
                run_id = %run.run_id,
                "DbPipelineWatcher: DB write for Completed failed (non-fatal): {e}"
            );
        }
    }

    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
        // Python parity: `run_info = {"data": data_info(data), "error": str(e)}`.
        let run_info = Some(run_info_for_errored(&run.data_ids, error));
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                run.run_id,
                run.pipeline_id,
                &run.pipeline_name,
                run.dataset_id,
                DbStatus::Errored,
                run_info,
            )
            .await
        {
            tracing::warn!(
                run_id = %run.run_id,
                "DbPipelineWatcher: DB write for Errored failed (non-fatal): {e}"
            );
        }
    }

    async fn on_payload_field(&self, run_id: Uuid, key: &str, value: serde_json::Value) {
        if let Err(e) = self.repo.set_payload_field(run_id, key, value).await {
            tracing::warn!(
                run_id = %run_id,
                key = %key,
                "DbPipelineWatcher: DB write for payload field failed (non-fatal): {e}"
            );
        }
    }
}
