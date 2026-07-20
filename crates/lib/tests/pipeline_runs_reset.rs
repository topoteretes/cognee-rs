#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for the Python-parity reset helpers (`api::pipeline_runs`).
//!
//! Covers (a) `reset_pipeline_run_status` writes a fresh `INITIATED` row,
//! (b) `reset_dataset_pipeline_run_status` skips pipelines already at
//! `INITIATED` and resets the rest, (c) idempotency (re-running the
//! dataset-level helper after a fresh reset is a no-op for all pipelines).

use std::sync::Arc;

use cognee::api::pipeline_runs::{reset_dataset_pipeline_run_status, reset_pipeline_run_status};
use cognee::database::{
    PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository, connect, initialize,
};
use serde_json::json;
use uuid::Uuid;

async fn make_repo() -> Arc<dyn PipelineRunRepository> {
    let db = connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    initialize(&db).await.expect("initialize schema");
    Arc::new(SeaOrmPipelineRunRepository::new(Arc::new(db)))
}

#[tokio::test]
async fn reset_pipeline_run_status_writes_initiated_row() {
    let repo = make_repo().await;
    let user_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    let pipeline_name = "cognify_pipeline";

    // Seed a COMPLETED row so the latest status would otherwise short-circuit.
    let pid =
        cognee::core::pipeline_run_registry::ids::pipeline_id(user_id, dataset_id, pipeline_name);
    let prid = cognee::core::pipeline_run_registry::ids::pipeline_run_id(pid, dataset_id);
    repo.log_pipeline_run(
        prid,
        pid,
        pipeline_name,
        Some(dataset_id),
        PipelineRunStatus::Completed,
        Some(json!({"data": "None"})),
    )
    .await
    .expect("seed COMPLETED row");

    reset_pipeline_run_status(Arc::clone(&repo), user_id, dataset_id, pipeline_name)
        .await
        .expect("reset_pipeline_run_status succeeds");

    // The latest status for the dataset must now be INITIATED.
    let latest = repo
        .latest_status(&[dataset_id], pipeline_name)
        .await
        .expect("latest_status query");
    assert_eq!(latest.get(&dataset_id), Some(&PipelineRunStatus::Initiated));
}

#[tokio::test]
async fn reset_dataset_pipeline_run_status_skips_already_initiated() {
    let repo = make_repo().await;
    let user_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    // Pipeline A: already INITIATED (should be skipped — no new row).
    // Pipeline B: COMPLETED (should be reset).
    for name in ["already_pending", "needs_reset"] {
        let pid = cognee::core::pipeline_run_registry::ids::pipeline_id(user_id, dataset_id, name);
        let prid = cognee::core::pipeline_run_registry::ids::pipeline_run_id(pid, dataset_id);
        let status = if name == "already_pending" {
            PipelineRunStatus::Initiated
        } else {
            PipelineRunStatus::Completed
        };
        let run_info = if name == "already_pending" {
            Some(json!({}))
        } else {
            Some(json!({"data": "None"}))
        };
        repo.log_pipeline_run(prid, pid, name, Some(dataset_id), status, run_info)
            .await
            .expect("seed row");
    }

    // Count rows before — every pipeline has exactly one row.
    let rows_before = repo
        .list_recent(Some(dataset_id), 100)
        .await
        .expect("list_recent");
    assert_eq!(rows_before.len(), 2);

    reset_dataset_pipeline_run_status(Arc::clone(&repo), user_id, dataset_id)
        .await
        .expect("reset_dataset_pipeline_run_status succeeds");

    let rows_after = repo
        .list_recent(Some(dataset_id), 100)
        .await
        .expect("list_recent");

    // Pipeline A unchanged (still 1 row). Pipeline B gained an INITIATED row
    // (now 2 rows). Total = 3.
    assert_eq!(
        rows_after.len(),
        3,
        "exactly one new row should be appended for `needs_reset`"
    );

    let latest_pending = repo
        .latest_status(&[dataset_id], "already_pending")
        .await
        .expect("latest");
    assert_eq!(
        latest_pending.get(&dataset_id),
        Some(&PipelineRunStatus::Initiated)
    );

    let latest_reset = repo
        .latest_status(&[dataset_id], "needs_reset")
        .await
        .expect("latest");
    assert_eq!(
        latest_reset.get(&dataset_id),
        Some(&PipelineRunStatus::Initiated)
    );
}

#[tokio::test]
async fn reset_dataset_pipeline_run_status_is_idempotent() {
    let repo = make_repo().await;
    let user_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    let pipeline_name = "cognify_pipeline";

    // Seed one COMPLETED row.
    let pid =
        cognee::core::pipeline_run_registry::ids::pipeline_id(user_id, dataset_id, pipeline_name);
    let prid = cognee::core::pipeline_run_registry::ids::pipeline_run_id(pid, dataset_id);
    repo.log_pipeline_run(
        prid,
        pid,
        pipeline_name,
        Some(dataset_id),
        PipelineRunStatus::Completed,
        Some(json!({"data": "None"})),
    )
    .await
    .expect("seed COMPLETED row");

    // First call: appends an INITIATED row (2 rows total).
    reset_dataset_pipeline_run_status(Arc::clone(&repo), user_id, dataset_id)
        .await
        .expect("first reset");
    let after_first = repo
        .list_recent(Some(dataset_id), 100)
        .await
        .expect("list_recent");
    assert_eq!(after_first.len(), 2);

    // Second call: pipeline is now at INITIATED — should be a no-op.
    reset_dataset_pipeline_run_status(Arc::clone(&repo), user_id, dataset_id)
        .await
        .expect("second reset");
    let after_second = repo
        .list_recent(Some(dataset_id), 100)
        .await
        .expect("list_recent");
    assert_eq!(
        after_second.len(),
        2,
        "second reset should not append duplicate INITIATED rows"
    );
}
