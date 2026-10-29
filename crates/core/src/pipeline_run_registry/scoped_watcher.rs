use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use uuid::Uuid;

use cognee_database::{PipelineRunRepository, PipelineRunStatus as DbStatus};

use crate::pipeline::{
    PipelineRunInfo, PipelineRunStatus as CoreStatus, PipelineStatus, PipelineWatcher, TaskStatus,
};

use super::types::{RunEvent, RunEventKind, RunPhase};

/// A thin broadcast handle that lets `ScopedRunWatcher` publish events into a
/// run's slot channel without holding a reference to the full registry.
///
/// Callers construct one from `DefaultPipelineRunRegistry::watcher_for(run_id)`.
pub struct PerRunSink {
    pub(crate) event_tx: tokio::sync::broadcast::Sender<RunEvent>,
    pub(crate) phase_tx: tokio::sync::watch::Sender<RunPhase>,
}

impl PerRunSink {
    /// Create a new `PerRunSink` with the given channel senders.
    pub fn from_parts(
        event_tx: tokio::sync::broadcast::Sender<RunEvent>,
        phase_tx: tokio::sync::watch::Sender<RunPhase>,
    ) -> Self {
        Self { event_tx, phase_tx }
    }
}

impl PerRunSink {
    /// Publish an event to all current subscribers. Broadcast failures (no
    /// receivers, or channel full) are silently ignored — the registry
    /// documents that slow subscribers may miss events.
    pub fn publish(&self, event: RunEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Update the current phase snapshot.
    pub fn set_phase(&self, phase: RunPhase) {
        let _ = self.phase_tx.send(phase);
    }
}

/// `PipelineWatcher` proxy that forwards lifecycle events to a run's slot.
///
/// A `ScopedRunWatcher` is constructed by `DefaultPipelineRunRegistry::
/// watcher_for(run_id)` and injected into the `TaskContext` before calling the
/// work future. Library functions that call `watcher.on_pipeline_run_started`
/// etc. automatically publish events and write durable rows without knowing
/// about the registry.
///
/// # Repository write failures
///
/// A repository write failure does **not** abort the pipeline — it is logged
/// via `tracing::warn!` and execution continues. This matches Python's
/// behaviour where DB failures are non-fatal.
pub struct ScopedRunWatcher {
    run_id: Uuid,
    sink: PerRunSink,
    db: Arc<dyn PipelineRunRepository>,
}

impl ScopedRunWatcher {
    pub fn new(run_id: Uuid, sink: PerRunSink, db: Arc<dyn PipelineRunRepository>) -> Self {
        Self { run_id, sink, db }
    }
}

/// Translate a `cognee_core::PipelineRunStatus` to the database enum.
/// No dependency from cognee-database back to cognee-core — the mapping
/// lives here at the seam.
fn core_to_db_status(status: &CoreStatus) -> DbStatus {
    match status {
        CoreStatus::Started => DbStatus::Started,
        CoreStatus::Completed => DbStatus::Completed,
        CoreStatus::Errored => DbStatus::Errored,
    }
}

#[async_trait]
impl PipelineWatcher for ScopedRunWatcher {
    // ── Required no-op methods ────────────────────────────────────────────

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

    // ── Rich lifecycle events ─────────────────────────────────────────────

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        // 1. Write durable row — non-fatal on failure.
        //    Python parity: `log_pipeline_run_start` writes no data items
        //    (COG-6089); the row carries the run's recovery token when it has one.
        let run_info = Some(super::run_info_for_started(run.recovery_lock));
        let db_result = self
            .db
            .log_pipeline_run_entry(cognee_database::PipelineRunEntry {
                pipeline_run_id: run.run_id,
                pipeline_id: run.pipeline_id,
                pipeline_name: &run.pipeline_name,
                dataset_id: run.dataset_id,
                status: core_to_db_status(&run.status),
                run_info,
                user_id: run.user_id,
            })
            .await;

        if let Err(e) = db_result {
            tracing::warn!(
                run_id = %self.run_id,
                "ScopedRunWatcher: DB write for Started failed (non-fatal): {e}"
            );
        }

        // 2. Publish live event.
        self.sink.set_phase(RunPhase::Running);
        self.sink.publish(RunEvent {
            run_id: self.run_id,
            kind: RunEventKind::Started,
            payload: serde_json::Value::Null,
            at: Utc::now(),
        });
    }

    async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, _output_count: usize) {
        // 1. Write durable row — non-fatal on failure.
        //    Python parity: `log_pipeline_run_complete` writes `{}`.
        let run_info = Some(super::run_info_for_completed());
        let db_result = self
            .db
            .log_pipeline_run_entry(cognee_database::PipelineRunEntry {
                pipeline_run_id: run.run_id,
                pipeline_id: run.pipeline_id,
                pipeline_name: &run.pipeline_name,
                dataset_id: run.dataset_id,
                status: DbStatus::Completed,
                run_info,
                user_id: run.user_id,
            })
            .await;

        if let Err(e) = db_result {
            tracing::warn!(
                run_id = %self.run_id,
                "ScopedRunWatcher: DB write for Completed failed (non-fatal): {e}"
            );
        }

        // 2. Publish live event.
        self.sink.set_phase(RunPhase::Completed);
        self.sink.publish(RunEvent {
            run_id: self.run_id,
            kind: RunEventKind::Completed,
            payload: serde_json::Value::Null,
            at: Utc::now(),
        });
    }

    async fn on_payload_field(&self, run_id: Uuid, key: &str, value: serde_json::Value) {
        if let Err(e) = self.db.set_payload_field(run_id, key, value).await {
            tracing::warn!(
                run_id = %run_id,
                key = %key,
                "ScopedRunWatcher: DB write for payload field failed (non-fatal): {e}"
            );
        }
    }

    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
        // 1. Write durable row — non-fatal on failure.
        //    Python parity: `log_pipeline_run_error` writes `{"error": …}`.
        let run_info = Some(super::run_info_for_errored(error));
        let db_result = self
            .db
            .log_pipeline_run_entry(cognee_database::PipelineRunEntry {
                pipeline_run_id: run.run_id,
                pipeline_id: run.pipeline_id,
                pipeline_name: &run.pipeline_name,
                dataset_id: run.dataset_id,
                status: DbStatus::Errored,
                run_info,
                user_id: run.user_id,
            })
            .await;

        if let Err(e) = db_result {
            tracing::warn!(
                run_id = %self.run_id,
                "ScopedRunWatcher: DB write for Errored failed (non-fatal): {e}"
            );
        }

        // 2. Publish live event.
        self.sink.set_phase(RunPhase::Errored {
            message: error.to_string(),
        });
        self.sink.publish(RunEvent {
            run_id: self.run_id,
            kind: RunEventKind::Errored {
                message: error.to_string(),
            },
            payload: serde_json::Value::Null,
            at: Utc::now(),
        });
    }
}
