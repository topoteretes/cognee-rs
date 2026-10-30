#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `PipelineRunRepository::graph_build_evidence` against an in-memory SQLite
//! database.

use std::sync::Arc;

use cognee_database::pipelines::GraphBuildEvidence;
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    connect, initialize, ops,
};
use cognee_models::Dataset;
use serde_json::json;
use uuid::Uuid;

async fn make_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

async fn dataset(db: &DatabaseConnection) -> Uuid {
    let id = Uuid::new_v4();
    let dataset = Dataset::new("test".to_string(), Uuid::new_v4(), None, id);
    ops::datasets::create_dataset(db, dataset)
        .await
        .expect("create_dataset for FK setup");
    id
}

async fn log(
    repo: &SeaOrmPipelineRunRepository,
    dataset_id: Uuid,
    pipeline_name: &str,
    status: PipelineRunStatus,
    run_info: Option<serde_json::Value>,
) {
    repo.log_pipeline_run(
        Uuid::new_v4(),
        Uuid::new_v4(),
        pipeline_name,
        Some(dataset_id),
        status,
        run_info,
    )
    .await
    .expect("log_pipeline_run");
}

const STAGING: &[&str] = &["add_pipeline"];
const IGNORED: &[&str] = &["forget", "improve"];

async fn evidence(repo: &SeaOrmPipelineRunRepository, ids: &[Uuid]) -> GraphBuildEvidence {
    repo.graph_build_evidence(ids, STAGING, IGNORED)
        .await
        .expect("graph_build_evidence")
}

#[tokio::test]
async fn a_dataset_without_runs_has_no_evidence() {
    let db = make_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let id = dataset(&db).await;
    assert_eq!(evidence(&repo, &[id]).await, GraphBuildEvidence::default());
}

#[tokio::test]
async fn a_completed_cognify_built_the_graph() {
    let db = make_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let id = dataset(&db).await;
    log(
        &repo,
        id,
        "cognify_pipeline",
        PipelineRunStatus::Completed,
        None,
    )
    .await;
    let found = evidence(&repo, &[id]).await;
    assert!(found.graph_built);
    assert!(found.any_run);
}

#[tokio::test]
async fn a_staging_run_is_a_run_but_builds_no_graph() {
    let db = make_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let id = dataset(&db).await;
    log(
        &repo,
        id,
        "add_pipeline",
        PipelineRunStatus::Completed,
        None,
    )
    .await;
    let expected = GraphBuildEvidence {
        graph_built: false,
        latest_build_error: None,
        any_run: true,
    };
    assert_eq!(evidence(&repo, &[id]).await, expected);
}

#[tokio::test]
async fn an_errored_build_reports_its_error_and_a_staging_error_does_not() {
    let db = make_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let staged = dataset(&db).await;
    let errored = json!({"error": "staging failed"});
    log(
        &repo,
        staged,
        "add_pipeline",
        PipelineRunStatus::Errored,
        Some(errored),
    )
    .await;
    assert_eq!(evidence(&repo, &[staged]).await.latest_build_error, None);

    let built = dataset(&db).await;
    let errored = json!({"error": "LLM unreachable"});
    log(
        &repo,
        built,
        "cognify_pipeline",
        PipelineRunStatus::Errored,
        Some(errored),
    )
    .await;
    let found = evidence(&repo, &[built]).await;
    assert_eq!(
        found.latest_build_error,
        Some(Some("LLM unreachable".to_string()))
    );
    assert!(!found.graph_built);
}

#[tokio::test]
async fn change_records_are_not_runs() {
    let db = make_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let id = dataset(&db).await;
    log(&repo, id, "forget", PipelineRunStatus::Completed, None).await;
    log(&repo, id, "improve", PipelineRunStatus::Completed, None).await;
    assert_eq!(evidence(&repo, &[id]).await, GraphBuildEvidence::default());
}

#[tokio::test]
async fn runs_on_other_datasets_do_not_count() {
    let db = make_db().await;
    let repo = SeaOrmPipelineRunRepository::new(Arc::clone(&db));
    let built = dataset(&db).await;
    let empty = dataset(&db).await;
    log(
        &repo,
        built,
        "cognify_pipeline",
        PipelineRunStatus::Completed,
        None,
    )
    .await;
    assert_eq!(
        evidence(&repo, &[empty]).await,
        GraphBuildEvidence::default()
    );
    assert!(evidence(&repo, &[built, empty]).await.graph_built);
}
