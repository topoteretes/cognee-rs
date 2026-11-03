//! [`PipelineWatcher`] that persists the `pipeline_runs` trail
//! (`STARTED`, then `COMPLETED` or `ERRORED`, as Python's `run_tasks` writes it)
//! through [`PipelineRunRepository`] without an in-memory event channel.
//!
//! Used by library convenience functions (`cognee_cognify::cognify`,
//! `cognee_ingestion::AddPipeline::add`)
//! that do not flow through the http-server's `DefaultPipelineRunRegistry`
//! but still want the audit trail (gap 08-07, locked decision 11).
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
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use uuid::Uuid;

use cognee_database::{PipelineRunRepository, PipelineRunStatus as DbStatus};

use crate::pipeline::{
    PipelineRunInfo, PipelineRunStatus as CoreStatus, PipelineStatus, PipelineWatcher, TaskStatus,
    WatcherError,
};

use super::{run_info_for_completed, run_info_for_errored, run_info_for_started};

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
    /// Set when a `COMPLETED` / `ERRORED` write failed. See
    /// [`Self::terminal_write_failed`].
    terminal_write_failed: AtomicBool,
}

impl DbPipelineWatcher {
    /// Create a watcher that persists through `repo`.
    pub fn new(repo: Arc<dyn PipelineRunRepository>) -> Self {
        Self {
            repo,
            terminal_write_failed: AtomicBool::new(false),
        }
    }

    /// Whether the run's terminal row is missing: the last terminal
    /// (`COMPLETED` / `ERRORED`) write failed. A failed `COMPLETED` write
    /// followed by a successful `ERRORED` one (the executor's response to it)
    /// leaves this `false`.
    ///
    /// The write is best-effort, but its outcome decides the run's ownership
    /// marker: Python marks the marker closed only after
    /// `log_pipeline_run_complete` / `log_pipeline_run_error` returned
    /// (`run_tasks.py:342,403`). After a failed write the run is still
    /// `STARTED` in the database, so the marker must stay for startup recovery
    /// to find and retire the run; closing it would leave the run unrecoverable.
    pub fn terminal_write_failed(&self) -> bool {
        self.terminal_write_failed.load(Ordering::SeqCst)
    }

    /// Mirrors `ScopedRunWatcher::core_to_db_status` — no cross-crate
    /// dependency from `cognee-database` back to `cognee-core`, so the
    /// translation lives at the seam.
    fn core_to_db_status(status: &CoreStatus) -> DbStatus {
        match status {
            CoreStatus::Started => DbStatus::Started,
            CoreStatus::Completed => DbStatus::Completed,
            CoreStatus::Errored => DbStatus::Errored,
        }
    }
}

#[async_trait]
impl PipelineWatcher for DbPipelineWatcher {
    // ── Required no-op methods (DB-watcher doesn't care about per-task
    //    granularity; the pipeline-run lifecycle hooks below cover
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

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        // Python parity: `log_pipeline_run_start` writes no data items
        // (COG-6089); the row carries the run's recovery token when it has one.
        let run_info = Some(run_info_for_started(run.recovery_lock));
        if let Err(e) = self
            .repo
            .log_pipeline_run_entry(cognee_database::PipelineRunEntry {
                pipeline_run_id: run.run_id,
                pipeline_id: run.pipeline_id,
                pipeline_name: &run.pipeline_name,
                dataset_id: run.dataset_id,
                status: Self::core_to_db_status(&run.status),
                run_info,
                user_id: run.user_id,
            })
            .await
        {
            tracing::warn!(
                run_id = %run.run_id,
                "DbPipelineWatcher: DB write for Started failed (non-fatal): {e}"
            );
        }
    }

    async fn on_pipeline_run_completed(
        &self,
        run: &PipelineRunInfo,
        _output_count: usize,
    ) -> Result<(), WatcherError> {
        // Python parity: `log_pipeline_run_complete` writes `{}`, and a failed
        // write fails the run (the executor then writes ERRORED).
        let run_info = Some(run_info_for_completed());
        if let Err(e) = self
            .repo
            .log_pipeline_run_entry(cognee_database::PipelineRunEntry {
                pipeline_run_id: run.run_id,
                pipeline_id: run.pipeline_id,
                pipeline_name: &run.pipeline_name,
                dataset_id: run.dataset_id,
                status: DbStatus::Completed,
                run_info,
                user_id: run.user_id,
            })
            .await
        {
            self.terminal_write_failed.store(true, Ordering::SeqCst);
            tracing::warn!(
                run_id = %run.run_id,
                "DbPipelineWatcher: DB write for Completed failed: {e}"
            );
            return Err(WatcherError(e.to_string()));
        }
        Ok(())
    }

    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
        // Python parity: `log_pipeline_run_error` writes `{"error": …}`.
        let run_info = Some(run_info_for_errored(error));
        if let Err(e) = self
            .repo
            .log_pipeline_run_entry(cognee_database::PipelineRunEntry {
                pipeline_run_id: run.run_id,
                pipeline_id: run.pipeline_id,
                pipeline_name: &run.pipeline_name,
                dataset_id: run.dataset_id,
                status: DbStatus::Errored,
                run_info,
                user_id: run.user_id,
            })
            .await
        {
            self.terminal_write_failed.store(true, Ordering::SeqCst);
            tracing::warn!(
                run_id = %run.run_id,
                "DbPipelineWatcher: DB write for Errored failed (non-fatal): {e}"
            );
        } else {
            // The ERRORED row superseded a failed COMPLETED write: the run is
            // terminal in the database, so its marker may close (Python closes
            // it after `log_pipeline_run_error` returned).
            self.terminal_write_failed.store(false, Ordering::SeqCst);
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
