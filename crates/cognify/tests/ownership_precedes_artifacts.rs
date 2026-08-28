#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! End-to-end proof of invariant I1, for **both** pipelines: an artifact never
//! exists in the graph or vector store without an ownership record naming the
//! run that created it.
//!
//! The per-stage tests in `tasks.rs` pin the ordering at each seam. What they
//! cannot show is that the run id the stages stamp is the *same* value a sweep
//! will later select on: the two are related only by a chain of plumbing —
//! executor → `PipelineContext::run_id` → task → ownership row, and separately
//! executor → `DbPipelineWatcher` → `pipeline_runs.pipeline_run_id`. This file
//! runs a real `cognify()` against mock stores and a real
//! `SeaOrmPipelineRunRepository` and asserts the two ends agree — once on the
//! standard branch and once on the temporal one, which has its own persistence
//! stage and therefore its own chain of plumbing to get wrong.
//!
//! Offline: `MockLlm` / `MockStorage` / `MockGraphDB` / `MockVectorDB` /
//! `MockEmbeddingEngine`. No network, no LLM key, no skip path.

mod rollback_harness;

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, CognifyResult, cognify};
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository, connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_vector::VectorDB;
use serde_json::json;
use uuid::Uuid;

const FIXTURE_TEXT: &str = "Alice works at Acme. Acme builds tools.";

fn canned_graph_response() -> String {
    json!({
        "nodes": [
            {"id": "alice", "name": "Alice", "type": "PERSON", "description": "A person."},
            {"id": "acme", "name": "Acme", "type": "ORGANIZATION", "description": "A company."}
        ],
        "edges": [{
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "works_at",
            "description": "Alice works at Acme."
        }]
    })
    .to_string()
}

/// Run one cognify against mock stores, returning its result, the database and
/// the pipeline-run repository the executor wrote through.
async fn run_cognify(
    dataset_id: Uuid,
    owner_id: Uuid,
) -> (
    CognifyResult,
    Arc<DatabaseConnection>,
    Arc<dyn PipelineRunRepository>,
) {
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::new(vec![
        canned_graph_response(),
        json!({"summary": "Alice and Acme.", "description": "A short description."}).to_string(),
    ]));
    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
    let embedding_engine: Arc<dyn cognee_embedding::EmbeddingEngine> =
        Arc::new(MockEmbeddingEngine::new(8));

    let data_id = Uuid::new_v4();
    let location = storage
        .store(FIXTURE_TEXT.as_bytes(), &format!("ownership-{data_id}"))
        .await
        .expect("MockStorage::store");
    let data_item = Data::builder(
        data_id,
        "ownership.txt",
        location,
        "ownership.txt",
        "txt",
        "text/plain",
        "test-hash-ownership",
        owner_id,
    )
    .build();

    let db: Arc<DatabaseConnection> = {
        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        create_dataset(
            &conn,
            Dataset::new("ownership".into(), owner_id, None, dataset_id),
        )
        .await
        .expect("seed dataset");
        Arc::new(conn)
    };

    // A real repository, not the noop: this test's whole point is comparing the
    // run id on the ownership rows against the one in `pipeline_runs`.
    let repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));
    let thread_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool"));

    let result = cognify(
        vec![data_item],
        dataset_id,
        Some(owner_id),
        None,
        None,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        Arc::clone(&db),
        Arc::clone(&repo),
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &CognifyConfig::default(),
    )
    .await
    .expect("cognify must succeed against the mock backends");

    (result, db, repo)
}

/// Every ownership row a run writes carries that run's id — and it is the same
/// value the executor recorded in `pipeline_runs.pipeline_run_id`.
#[tokio::test]
async fn cognify_stamps_every_ownership_row_with_the_runs_id() {
    let dataset_id = Uuid::new_v4();
    let (_result, db, repo) = run_cognify(dataset_id, Uuid::new_v4()).await;

    let runs = repo
        .list_recent(Some(dataset_id), 50)
        .await
        .expect("list pipeline runs");
    assert!(
        !runs.is_empty(),
        "the executor must have written the run's status trail"
    );
    let run_ids: Vec<Uuid> = runs.iter().map(|row| row.pipeline_run_id).collect();
    let run_id = run_ids[0];
    assert!(
        run_ids.iter().all(|id| *id == run_id),
        "one cognify is one pipeline run"
    );

    let nodes = get_nodes_by_dataset(&db, dataset_id).await.expect("query");
    assert!(
        !nodes.is_empty(),
        "the run must have written ownership rows"
    );
    for row in &nodes {
        assert_eq!(
            row.pipeline_run_id,
            Some(run_id),
            "node row {} ({}) must name the run that created it",
            row.slug,
            row.node_type
        );
    }

    let edges = get_edges_by_dataset(&db, dataset_id).await.expect("query");
    assert!(!edges.is_empty(), "the run must have written edge rows");
    for row in &edges {
        assert_eq!(
            row.pipeline_run_id,
            Some(run_id),
            "edge row {} must name the run that created it",
            row.relationship_name
        );
    }
}

/// The result carries the run id, which is how the post-pipeline DLT teardown —
/// running outside the executor, with no `TaskContext` — attributes its own
/// ownership rows. The two synthetic results carry `None`, because no run
/// produced them.
#[tokio::test]
async fn cognify_result_carries_the_pipeline_run_id() {
    let dataset_id = Uuid::new_v4();
    let (result, _db, repo) = run_cognify(dataset_id, Uuid::new_v4()).await;

    let runs = repo
        .list_recent(Some(dataset_id), 50)
        .await
        .expect("list pipeline runs");
    assert_eq!(result.pipeline_run_id, Some(runs[0].pipeline_run_id));

    assert_eq!(CognifyResult::empty().pipeline_run_id, None);
    assert_eq!(
        CognifyResult::already_completed(Uuid::new_v4()).pipeline_run_id,
        None
    );
}

/// The same two-ended agreement on the temporal branch.
///
/// Temporal used to write no ownership records at all: nodes, edges and
/// `Event_name` points went straight to the stores, so nothing could name what
/// a temporal run had created and a temporal sweep removed nothing while
/// reporting success. This is the end-to-end proof that the run id the temporal
/// persistence stage stamps is the one a sweep selects on.
#[tokio::test]
async fn temporal_cognify_stamps_every_ownership_row_with_the_runs_id() {
    use rollback_harness::{Harness, TemporalFixtureLlm, temporal_config};

    let mut harness = Harness::new().await;
    let only = harness.add_file("Alice joined Acme in 2020.").await;

    harness
        .run_over(
            &temporal_config(),
            &[only],
            Arc::new(TemporalFixtureLlm::new()),
        )
        .await
        .expect("the temporal run must succeed against the mock backends");

    let runs = harness
        .repo
        .list_recent(Some(harness.dataset_id), 50)
        .await
        .expect("list pipeline runs");
    assert!(
        !runs.is_empty(),
        "the executor must have written the run's status trail"
    );
    let run_id = runs[0].pipeline_run_id;
    assert!(
        runs.iter().all(|row| row.pipeline_run_id == run_id),
        "one cognify is one pipeline run"
    );

    let nodes = harness.ledger_nodes().await;
    assert!(
        !nodes.is_empty(),
        "the temporal run must have written ownership rows"
    );
    for row in &nodes {
        assert_eq!(
            row.pipeline_run_id,
            Some(run_id),
            "node row {} ({}) must name the run that created it",
            row.slug,
            row.node_type
        );
        assert_eq!(
            row.data_id, only,
            "and the data item that produced it — a nil id would be a phantom item to unmark"
        );
    }

    let edges = harness.ledger_edges().await;
    assert!(!edges.is_empty(), "the temporal edges are claimed too");
    for row in &edges {
        assert_eq!(row.pipeline_run_id, Some(run_id));
        assert_eq!(row.data_id, only);
    }
}
