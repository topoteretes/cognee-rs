//! Round-trip tests for `SeaOrmPipelineRunRepository` against an in-memory
//! SQLite database.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    connect, initialize, ops,
};
use cognee_models::Dataset;
use uuid::Uuid;

async fn make_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

fn make_repo(db: Arc<DatabaseConnection>) -> SeaOrmPipelineRunRepository {
    SeaOrmPipelineRunRepository::new(db)
}

/// Pre-create a dataset row so FK constraints on `pipeline_runs.dataset_id` pass.
async fn create_dataset(db: &DatabaseConnection, id: Uuid) {
    let owner = Uuid::new_v4();
    let dataset = Dataset::new("test".to_string(), owner, None, id);
    ops::datasets::create_dataset(db, dataset)
        .await
        .expect("create_dataset for FK setup");
}

// ---------------------------------------------------------------------------
// (a) log_pipeline_run returns a fresh Uuid per call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn log_pipeline_run_returns_fresh_uuid() {
    let db = make_db().await;
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;
    let repo = make_repo(Arc::clone(&db));

    let pipeline_run_id = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    let id1 = repo
        .log_pipeline_run(
            pipeline_run_id,
            pipeline_id,
            "test_pipeline",
            Some(dataset_id),
            PipelineRunStatus::Initiated,
            None,
        )
        .await
        .expect("log run 1");

    let id2 = repo
        .log_pipeline_run(
            pipeline_run_id,
            pipeline_id,
            "test_pipeline",
            Some(dataset_id),
            PipelineRunStatus::Started,
            None,
        )
        .await
        .expect("log run 2");

    assert_ne!(id1, id2, "each call must return a distinct primary key");
}

// ---------------------------------------------------------------------------
// (b) latest_status returns the most recent row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn latest_status_returns_most_recent_row() {
    let db = make_db().await;
    let dataset_id = Uuid::new_v4();
    create_dataset(&db, dataset_id).await;
    let repo = make_repo(Arc::clone(&db));

    let pipeline_run_id = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    // Write Initiated, then Started, then Completed.
    for status in [
        PipelineRunStatus::Initiated,
        PipelineRunStatus::Started,
        PipelineRunStatus::Completed,
    ] {
        repo.log_pipeline_run(
            pipeline_run_id,
            pipeline_id,
            "p1",
            Some(dataset_id),
            status,
            None,
        )
        .await
        .expect("log");
    }

    let statuses = repo
        .latest_status(&[dataset_id], "p1")
        .await
        .expect("latest_status");

    assert_eq!(
        statuses.get(&dataset_id).cloned(),
        Some(PipelineRunStatus::Completed),
        "latest row should be Completed"
    );
}

// ---------------------------------------------------------------------------
// (c) latest_status batch returns latest per dataset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn latest_status_batch_returns_per_dataset() {
    let db = make_db().await;
    let ds1 = Uuid::new_v4();
    let ds2 = Uuid::new_v4();
    create_dataset(&db, ds1).await;
    create_dataset(&db, ds2).await;
    let repo = make_repo(Arc::clone(&db));

    let pipeline_id = Uuid::new_v4();

    // ds1: Initiated → Started
    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Started] {
        repo.log_pipeline_run(
            Uuid::new_v4(),
            pipeline_id,
            "batch_p",
            Some(ds1),
            status,
            None,
        )
        .await
        .expect("log ds1");
    }

    // ds2: Initiated → Completed
    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Completed] {
        repo.log_pipeline_run(
            Uuid::new_v4(),
            pipeline_id,
            "batch_p",
            Some(ds2),
            status,
            None,
        )
        .await
        .expect("log ds2");
    }

    let statuses: HashMap<Uuid, PipelineRunStatus> = repo
        .latest_status(&[ds1, ds2], "batch_p")
        .await
        .expect("latest_status batch");

    assert_eq!(
        statuses.get(&ds1).cloned(),
        Some(PipelineRunStatus::Started)
    );
    assert_eq!(
        statuses.get(&ds2).cloned(),
        Some(PipelineRunStatus::Completed)
    );
}

// ---------------------------------------------------------------------------
// (d) reset_orphans rewrites INITIATED/STARTED to ERRORED and counts them
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reset_orphans_rewrites_initiated_and_started() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let pr1 = Uuid::new_v4();
    let pr2 = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    // pr1: INITIATED (orphan)
    repo.log_pipeline_run(
        pr1,
        pipeline_id,
        "orphan_p",
        Some(ds),
        PipelineRunStatus::Initiated,
        None,
    )
    .await
    .expect("log pr1");

    // pr2: STARTED (orphan)
    repo.log_pipeline_run(
        pr2,
        pipeline_id,
        "orphan_p",
        Some(ds),
        PipelineRunStatus::Started,
        None,
    )
    .await
    .expect("log pr2");

    let count = repo
        .reset_orphans("server_restart_orphan")
        .await
        .expect("reset_orphans");

    assert_eq!(count, 2, "both orphans should be rewritten");

    // After reset, latest status should be Errored for both.
    let statuses = repo
        .latest_status(&[ds], "orphan_p")
        .await
        .expect("latest_status after reset");

    assert_eq!(statuses.get(&ds).cloned(), Some(PipelineRunStatus::Errored));
}

// ---------------------------------------------------------------------------
// (e) reset_orphans does NOT rewrite a row with a COMPLETED successor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reset_orphans_does_not_rewrite_completed_successor() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let pr = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    // Write INITIATED → COMPLETED (completed pipeline run, not an orphan).
    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Completed] {
        repo.log_pipeline_run(pr, pipeline_id, "done_p", Some(ds), status, None)
            .await
            .expect("log");
    }

    let count = repo
        .reset_orphans("should_not_match")
        .await
        .expect("reset_orphans");

    // The most recent row for this pipeline_run_id is COMPLETED, not an orphan.
    assert_eq!(count, 0, "completed pipeline should not be rewritten");
}

// ---------------------------------------------------------------------------
// list_recent — basic smoke test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_recent_returns_rows_in_desc_order() {
    let db = make_db().await;
    let ds = Uuid::new_v4();
    create_dataset(&db, ds).await;
    let repo = make_repo(Arc::clone(&db));

    let pipeline_id = Uuid::new_v4();

    for status in [PipelineRunStatus::Initiated, PipelineRunStatus::Completed] {
        repo.log_pipeline_run(
            Uuid::new_v4(),
            pipeline_id,
            "list_p",
            Some(ds),
            status,
            None,
        )
        .await
        .expect("log");
    }

    let rows = repo.list_recent(Some(ds), 10).await.expect("list_recent");

    assert_eq!(rows.len(), 2);
    // First row is the most recent (Completed).
    assert_eq!(rows[0].status, PipelineRunStatus::Completed);
}
