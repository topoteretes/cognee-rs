//! Python-parity `check_pipeline_run_qualification`.
//!
//! Reads the latest `pipeline_runs` row for `(dataset_id, pipeline_name)` and
//! returns a verdict the caller acts on:
//!
//! - `Proceed` — no previous row, or the latest is `INITIATED` or `ERRORED`.
//! - `AlreadyRunning(PipelineRun)` — latest is `STARTED`; caller should reject.
//! - `AlreadyCompleted(PipelineRun)` — latest is `COMPLETED`; caller should
//!   short-circuit without re-running.
//!
//! Source of truth: [`check_pipeline_run_qualification.py`][py]. Locked
//! decision 3 ships this gate at the `cognify` and `memify` entry points;
//! ingestion is intentionally excluded.
//!
//! [py]: https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/check_pipeline_run_qualification.py

use cognee_database::{DatabaseError, PipelineRun, PipelineRunRepository, PipelineRunStatus};
use uuid::Uuid;

/// Verdict from a qualification check.
#[derive(Debug, Clone)]
pub enum Qualification {
    /// No previous run, or the latest is `INITIATED` or `ERRORED` — proceed.
    Proceed,
    /// Latest row is `STARTED` — reject; caller should return an error.
    AlreadyRunning(PipelineRun),
    /// Latest row is `COMPLETED` — short-circuit; caller should not re-run.
    AlreadyCompleted(PipelineRun),
}

/// Rust mirror of Python's `check_pipeline_run_qualification`.
///
/// Reads the most recent `pipeline_runs` row for `(dataset_id,
/// pipeline_name)` via [`PipelineRunRepository::get_pipeline_run_by_dataset`]
/// (added in task 08-06) and maps it to a [`Qualification`].
///
/// `INITIATED` and `ERRORED` map to `Proceed` to match Python's behaviour
/// (see [Python source][py]); only `STARTED` rejects and only `COMPLETED`
/// short-circuits.
///
/// [py]: https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/check_pipeline_run_qualification.py
pub async fn check_pipeline_run_qualification(
    repo: &dyn PipelineRunRepository,
    dataset_id: Uuid,
    pipeline_name: &str,
) -> Result<Qualification, DatabaseError> {
    let latest = repo
        .get_pipeline_run_by_dataset(dataset_id, pipeline_name)
        .await?;
    Ok(match latest {
        None => Qualification::Proceed,
        Some(run) => match run.status {
            PipelineRunStatus::Initiated | PipelineRunStatus::Errored => Qualification::Proceed,
            PipelineRunStatus::Started => Qualification::AlreadyRunning(run),
            PipelineRunStatus::Completed => Qualification::AlreadyCompleted(run),
        },
    })
}
