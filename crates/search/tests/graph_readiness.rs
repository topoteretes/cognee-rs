#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `SearchOrchestrator::assess_memory_readiness` over an in-memory SQLite
//! database: which datasets it probes, and when it fails open.

use std::sync::Arc;

use cognee_database::{
    DatabaseConnection, IngestDb, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize, ops,
};
use cognee_models::Dataset;
use cognee_search::SearchOrchestrator;
use cognee_search::graph_warmup::{GraphWarmup, ReadinessState, WarmupConfig};
use cognee_search::orchestration::SearchTypeRegistry;
use serde_json::json;
use uuid::Uuid;

async fn make_db() -> Arc<DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

async fn owned_dataset(db: &DatabaseConnection, owner: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let dataset = Dataset::new("docs".to_string(), owner, None, id);
    ops::datasets::create_dataset(db, dataset)
        .await
        .expect("create_dataset");
    id
}

async fn log_cognify(
    db: &Arc<DatabaseConnection>,
    dataset_id: Uuid,
    status: PipelineRunStatus,
    run_info: Option<serde_json::Value>,
) {
    SeaOrmPipelineRunRepository::new(Arc::clone(db))
        .log_pipeline_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "cognify_pipeline",
            Some(dataset_id),
            status,
            run_info,
        )
        .await
        .expect("log_pipeline_run");
}

fn orchestrator(db: &Arc<DatabaseConnection>) -> SearchOrchestrator {
    let pipeline_runs = Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(db)));
    let warmup = GraphWarmup::new(pipeline_runs, WarmupConfig::default());
    SearchOrchestrator::new(SearchTypeRegistry::new())
        .with_dataset_resolver(Arc::clone(db) as Arc<dyn IngestDb>)
        .with_graph_warmup(Arc::new(warmup))
}

#[tokio::test]
async fn a_dataset_is_cold_until_its_graph_is_built() {
    let db = make_db().await;
    let owner = Uuid::new_v4();
    let id = owned_dataset(&db, owner).await;
    let orchestrator = orchestrator(&db);

    let probe = orchestrator
        .assess_memory_readiness(owner, None, Some(&[id]))
        .await;
    assert_eq!(probe.state, ReadinessState::NeverBuilt);

    log_cognify(&db, id, PipelineRunStatus::Completed, None).await;
    let probe = orchestrator
        .assess_memory_readiness(owner, None, Some(&[id]))
        .await;
    assert_eq!(probe.state, ReadinessState::Warm);
}

#[tokio::test]
async fn a_failed_build_on_any_readable_dataset_is_reported() {
    let db = make_db().await;
    let owner = Uuid::new_v4();
    let id = owned_dataset(&db, owner).await;
    let error = json!({"error": "LLM unreachable"});
    log_cognify(&db, id, PipelineRunStatus::Errored, Some(error)).await;

    let probe = orchestrator(&db)
        .assess_memory_readiness(owner, None, None)
        .await;
    assert_eq!(probe.state, ReadinessState::BuildFailed);
    assert_eq!(probe.error_message.as_deref(), Some("LLM unreachable"));
}

#[tokio::test]
async fn another_owners_dataset_is_not_probed() {
    let db = make_db().await;
    let theirs = owned_dataset(&db, Uuid::new_v4()).await;
    log_cognify(&db, theirs, PipelineRunStatus::Completed, None).await;

    let caller = Uuid::new_v4();
    let probe = orchestrator(&db)
        .assess_memory_readiness(caller, None, Some(&[theirs]))
        .await;
    assert_eq!(probe.state, ReadinessState::NeverBuilt);
}

#[tokio::test]
async fn the_probe_fails_open() {
    let db = make_db().await;
    let owner = Uuid::new_v4();
    let id = owned_dataset(&db, owner).await;
    let ids = Some(&[id][..]);

    let unprobed = SearchOrchestrator::new(SearchTypeRegistry::new());
    let probe = unprobed.assess_memory_readiness(owner, None, ids).await;
    assert!(probe.is_warm(), "no probe wired");

    let pipeline_runs = Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));
    let warmup = Arc::new(GraphWarmup::new(pipeline_runs, WarmupConfig::default()));
    let unscoped = SearchOrchestrator::new(SearchTypeRegistry::new()).with_graph_warmup(warmup);
    let probe = unscoped.assess_memory_readiness(owner, None, ids).await;
    assert!(probe.is_warm(), "nothing says what the caller reads");

    let failing = Arc::new(cognee_database::pipelines::NoopPipelineRunRepository::new());
    let warmup = Arc::new(GraphWarmup::new(failing, WarmupConfig::default()));
    let broken = SearchOrchestrator::new(SearchTypeRegistry::new())
        .with_dataset_resolver(Arc::clone(&db) as Arc<dyn IngestDb>)
        .with_graph_warmup(warmup);
    let probe = broken.assess_memory_readiness(owner, None, ids).await;
    assert!(probe.is_warm(), "the repository cannot answer");
}
