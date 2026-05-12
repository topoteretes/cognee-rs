//! Python-parity reset helpers for the `pipeline_runs` table.
//!
//! Exposes [`reset_pipeline_run_status`] (single pipeline) and
//! [`reset_dataset_pipeline_run_status`] (every pipeline registered against
//! a dataset). Both write a fresh `INITIATED` row through the
//! [`PipelineRunRepository`] (decision 11: single point of truth). The
//! dataset-level helper short-circuits when the latest row for a pipeline is
//! already `INITIATED`, matching Python's
//! [`reset_dataset_pipeline_run_status`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py).
//!
//! See [docs/telemetry/08/05-reset-helpers.md](../../../docs/telemetry/08/05-reset-helpers.md)
//! for the full design.

use std::sync::Arc;

use uuid::Uuid;

use cognee_core::pipeline_run_registry::{
    ids::{pipeline_id, pipeline_run_id},
    run_info_for_initiated,
};
use cognee_database::{PipelineRunRepository, PipelineRunStatus};

use super::error::ApiError;

/// Insert a fresh `INITIATED` row for the `(user_id, dataset_id, pipeline_name)`
/// triple so a future re-cognify is not short-circuited by
/// `check_pipeline_run_qualification` (task 08-08).
///
/// `pipeline_id` and `pipeline_run_id` are derived deterministically using the
/// Python-parity helpers in [`cognee_core::pipeline_run_registry::ids`]:
///
/// - `pipeline_id = uuid5(OID, "{user_id}{pipeline_name}{dataset_id}")`
/// - `pipeline_run_id = uuid5(OID, "{pipeline_id}_{dataset_id}")`
///
/// `run_info` is the empty object `{}` (decision 5,
/// `crates/core/src/pipeline_run_registry/data_info.rs::run_info_for_initiated`).
///
/// Matches Python's
/// [`reset_pipeline_run_status`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/reset_pipeline_run_status.py).
///
/// # Errors
///
/// Returns [`ApiError::InvalidArgument`] if the DB write fails — wraps the
/// underlying `DatabaseError` message verbatim so callers can surface it.
pub async fn reset_pipeline_run_status(
    repo: Arc<dyn PipelineRunRepository>,
    user_id: Uuid,
    dataset_id: Uuid,
    pipeline_name: &str,
) -> Result<(), ApiError> {
    let pid = pipeline_id(user_id, dataset_id, pipeline_name);
    let prid = pipeline_run_id(pid, dataset_id);
    repo.log_pipeline_run(
        prid,
        pid,
        pipeline_name,
        Some(dataset_id),
        PipelineRunStatus::Initiated,
        Some(run_info_for_initiated()),
    )
    .await
    .map(|_| ())
    .map_err(|e| ApiError::InvalidArgument(format!("reset_pipeline_run_status: {e}")))
}

/// Walk every distinct `pipeline_name` that has at least one
/// `pipeline_runs` row for `dataset_id` and call
/// [`reset_pipeline_run_status`] for each, skipping ones whose latest
/// status is already `INITIATED`.
///
/// Matches Python's
/// [`reset_dataset_pipeline_run_status`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py).
///
/// Uses [`PipelineRunRepository::get_pipeline_runs_by_dataset`] (one latest
/// row per `pipeline_name`) to enumerate the work — see action item 08-06.
///
/// # Errors
///
/// Returns the first error from the underlying repository — the iteration
/// stops at the first failure.
pub async fn reset_dataset_pipeline_run_status(
    repo: Arc<dyn PipelineRunRepository>,
    user_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), ApiError> {
    let runs = repo
        .get_pipeline_runs_by_dataset(dataset_id)
        .await
        .map_err(|e| ApiError::InvalidArgument(format!("get_pipeline_runs_by_dataset: {e}")))?;

    for run in runs {
        if matches!(run.status, PipelineRunStatus::Initiated) {
            // Python skips runs already at INITIATED to avoid stacking
            // duplicate rows when prune fires repeatedly.
            continue;
        }
        reset_pipeline_run_status(repo.clone(), user_id, dataset_id, &run.pipeline_name).await?;
    }
    Ok(())
}
