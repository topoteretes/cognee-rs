#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `CognifyConfig::evidence_pipeline_run_id`: a host that records the run in
//! `pipeline_runs` itself (the HTTP server's dispatcher, which hands cognify a
//! no-op run repository) keys the edge evidence on its own run id, so the
//! lookup's COMPLETED filter finds it.
//!
//! Without the override the evidence carries the executor's random run id,
//! which the no-op repository never records, and the lookup drops every row —
//! the HTTP `include_references` parity failure this guards against.
//!
//! The override changes only the evidence rows: the ownership ledger stays on
//! the executor's run id, which rollback sweeps by.
//!
//! Offline: mock LLM, embeddings and stores, in-memory SQLite.

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::ops::data::create_data;
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::ops::edge_evidence::get_edge_evidence_records;
use cognee_database::{
    DatabaseConnection, NoopPipelineRunRepository, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_utils::id_generation::generate_edge_object_id;
use cognee_vector::VectorDB;
use sea_orm::EntityTrait;
use serde_json::json;
use uuid::Uuid;

struct Outcome {
    db: Arc<DatabaseConnection>,
    dataset_id: Uuid,
    edge_id: Uuid,
}

/// Cognify one document through a no-op run repository, as the HTTP server
/// does, then record `host_run_id` as COMPLETED the way its dispatcher does.
async fn cognify_with_noop_repo(config: &CognifyConfig, host_run_id: Uuid) -> Outcome {
    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    let data_id = Uuid::new_v4();

    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let location = storage
        .store(b"Alice works with Bob.", "override-alice")
        .await
        .expect("store");
    let item = Data::builder(
        data_id,
        "alice.txt",
        location,
        "alice.txt",
        "txt",
        "text/plain",
        "alice-hash",
        owner_id,
    )
    .build();
    let db: Arc<DatabaseConnection> = {
        let conn = connect("sqlite::memory:").await.expect("connect");
        initialize(&conn).await.expect("initialize");
        create_dataset(
            &conn,
            Dataset::new("people".into(), owner_id, None, dataset_id),
        )
        .await
        .expect("seed dataset");
        create_data(&conn, item.clone()).await.expect("seed data");
        attach_data_to_dataset(&conn, dataset_id, data_id)
            .await
            .expect("attach data");
        Arc::new(conn)
    };

    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(
        MockLlm::new(vec![
            json!({
                "nodes": [
                    {"id": "alice", "name": "Alice", "type": "PERSON", "description": "a person"},
                    {"id": "bob", "name": "Bob", "type": "PERSON", "description": "a person"}
                ],
                "edges": [
                    {"source_node_id": "alice", "target_node_id": "bob",
                     "relationship_name": "works_with"}
                ]
            })
            .to_string(),
        ])
        .with_summary_response(json!({"summary": "s", "description": "d"}).to_string()),
    );
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
    let embedding: Arc<dyn cognee_embedding::EmbeddingEngine> =
        Arc::new(MockEmbeddingEngine::new(8));
    let thread_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("pool"));

    cognify(
        vec![item],
        dataset_id,
        Some(owner_id),
        None,
        None,
        llm,
        storage,
        Arc::clone(&graph_db),
        vector_db,
        embedding,
        Arc::clone(&db) as _,
        NoopPipelineRunRepository::arc(),
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        config,
    )
    .await
    .expect("cognify succeeds");

    // The host's own run trail: COMPLETED under its id, nothing else.
    SeaOrmPipelineRunRepository::new(Arc::clone(&db))
        .log_pipeline_run(
            host_run_id,
            Uuid::new_v4(),
            "cognify_pipeline",
            Some(dataset_id),
            PipelineRunStatus::Completed,
            None,
        )
        .await
        .expect("record the host run");

    let (_, edges) = graph_db.get_graph_data().await.expect("read the graph");
    let (source_id, target_id, _, _) = edges
        .iter()
        .find(|(_, _, relationship, _)| relationship == "works_with")
        .expect("the extracted edge is in the graph");
    let edge_id = generate_edge_object_id(source_id, target_id, "works_with");

    Outcome {
        db,
        dataset_id,
        edge_id,
    }
}

async fn evidence_run_ids(db: &DatabaseConnection) -> Vec<Option<Uuid>> {
    cognee_database::entities::provenance_edge_evidence::Entity::find()
        .all(db)
        .await
        .expect("read the evidence")
        .into_iter()
        .map(|row| {
            row.pipeline_run_id
                .as_deref()
                .map(|id| Uuid::parse_str(id).expect("run id"))
        })
        .collect()
}

#[tokio::test]
async fn the_override_keys_evidence_on_the_hosts_completed_run() {
    let host_run_id = Uuid::new_v4();
    let config = CognifyConfig::default().with_evidence_pipeline_run_id(host_run_id);
    let out = cognify_with_noop_repo(&config, host_run_id).await;

    let run_ids = evidence_run_ids(&out.db).await;
    assert!(!run_ids.is_empty(), "cognify captured no edge evidence");
    assert!(
        run_ids.iter().all(|id| *id == Some(host_run_id)),
        "{run_ids:?}"
    );

    let records = get_edge_evidence_records(&out.db, out.dataset_id, &[out.edge_id], 5, 50)
        .await
        .expect("lookup");
    assert_eq!(records.len(), 1, "{records:?}");

    // Ownership is untouched: the ledger stays on the executor's own run id,
    // the id rollback sweeps by.
    let nodes = cognee_database::entities::node::Entity::find()
        .all(out.db.as_ref())
        .await
        .expect("read the node ledger");
    assert!(!nodes.is_empty(), "no ownership rows");
    for node in &nodes {
        let run_id = node
            .pipeline_run_id
            .as_deref()
            .map(|id| Uuid::parse_str(id).expect("run id"));
        assert!(run_id.is_some(), "{node:?}");
        assert_ne!(run_id, Some(host_run_id), "{node:?}");
    }
}

#[tokio::test]
async fn without_the_override_a_noop_repo_run_has_no_citable_evidence() {
    let host_run_id = Uuid::new_v4();
    let out = cognify_with_noop_repo(&CognifyConfig::default(), host_run_id).await;

    let run_ids = evidence_run_ids(&out.db).await;
    assert!(!run_ids.is_empty(), "cognify captured no edge evidence");
    assert!(
        run_ids
            .iter()
            .all(|id| id.is_some() && *id != Some(host_run_id)),
        "{run_ids:?}"
    );

    let records = get_edge_evidence_records(&out.db, out.dataset_id, &[out.edge_id], 5, 50)
        .await
        .expect("lookup");
    assert!(records.is_empty(), "{records:?}");
}
