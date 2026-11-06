#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! SDK-1037 P4 end to end: a real `cognify` run, through the pipeline
//! executor, records `provenance_edge_evidence` rows under the run id its
//! `pipeline_runs` `COMPLETED` row carries, and an `include_references`
//! search over the resulting graph resolves the edge it used to the chunk
//! that produced it.
//!
//! The lookup only counts rows whose `pipeline_run_id` has a completed run,
//! so if the executor's run id and the ledger's ever diverged, the search
//! would silently find no source.
//!
//! Offline: mock LLM, embeddings and stores, in-memory SQLite.

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::ops::data::create_data;
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::ops::edge_evidence::get_edge_evidence_records;
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    SearchHistoryDb, connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_search::orchestration::{SearchOrchestrator, SearchTypeRegistry};
use cognee_search::retrievers::{GraphCompletionRetriever, SearchRetriever};
use cognee_search::types::{EvidenceKind, EvidenceRole, SearchOutput, SearchRequest, SearchType};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_utils::id_generation::generate_edge_object_id;
use cognee_vector::VectorDB;
use sea_orm::EntityTrait;
use serde_json::json;
use uuid::Uuid;

const ANSWER: &str = "Alice works with Bob.";

#[tokio::test]
async fn a_real_cognify_run_backs_the_source_evidence_of_a_search() {
    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    let data_id = Uuid::new_v4();

    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let location = storage
        .store(b"Alice works with Bob.", "e2e-alice")
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

    let extraction_llm: Arc<dyn cognee_llm::Llm> = Arc::new(
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
    let repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));
    let thread_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("pool"));

    cognify(
        vec![item],
        dataset_id,
        Some(owner_id),
        None,
        None,
        extraction_llm,
        Arc::clone(&storage),
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding),
        Arc::clone(&db) as _,
        Arc::clone(&repo),
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &CognifyConfig::default(),
    )
    .await
    .expect("cognify succeeds");

    // The executor wrote the run's COMPLETED row ...
    let runs = repo
        .list_recent(Some(dataset_id), 50)
        .await
        .expect("list runs");
    let completed: Vec<Uuid> = runs
        .iter()
        .filter(|run| run.status == PipelineRunStatus::Completed)
        .map(|run| run.pipeline_run_id)
        .collect();
    assert_eq!(completed.len(), 1, "{runs:?}");
    let run_id = completed[0];

    // ... and the ledger rows carry that same run id.
    let rows = cognee_database::entities::provenance_edge_evidence::Entity::find()
        .all(db.as_ref())
        .await
        .expect("read the ledger");
    assert!(!rows.is_empty(), "cognify captured no edge evidence");
    for row in &rows {
        assert_eq!(
            row.pipeline_run_id
                .as_deref()
                .map(|id| Uuid::parse_str(id).expect("run id")),
            Some(run_id),
            "{row:?}"
        );
    }

    // The extracted edge, as the graph holds it.
    let (nodes, edges) = graph_db.get_graph_data().await.expect("read the graph");
    let (source_id, target_id, _, _) = edges
        .iter()
        .find(|(_, _, relationship, _)| relationship == "works_with")
        .expect("the extracted edge is in the graph");
    let edge_id = generate_edge_object_id(source_id, target_id, "works_with");
    let chunk_id = nodes
        .iter()
        .find(|(_, props)| props.get("type").and_then(|t| t.as_str()) == Some("DocumentChunk"))
        .map(|(id, _)| Uuid::parse_str(id).expect("chunk id"))
        .expect("the chunk node");
    let records = get_edge_evidence_records(&db, dataset_id, &[edge_id], 5, 50)
        .await
        .expect("lookup");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].chunk_id, chunk_id);
    assert_eq!(records[0].data_id, data_id);

    // A graph completion search over the same stores cites that chunk.
    let answer_llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::new(vec![ANSWER.to_string()]));
    let retriever = GraphCompletionRetriever::new(
        vector_db, embedding, graph_db, answer_llm, None, None, None, None, None, None, None,
    );
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(retriever) as Arc<dyn SearchRetriever>);
    let orchestrator = SearchOrchestrator::new(registry)
        .with_database(Arc::clone(&db) as Arc<dyn SearchHistoryDb>);
    let mut request: SearchRequest =
        serde_json::from_value(json!({"query_text": "Who does Alice work with?"})).unwrap();
    request.search_type = SearchType::GraphCompletion;
    request.dataset_ids = Some(vec![dataset_id]);
    request.user_id = Some(owner_id);
    request.include_references = true;
    let response = orchestrator.search(&request).await.expect("search");

    // Every graph edge the answer used resolves to the one chunk that
    // produced it, the extracted `works_with` edge among them.
    let sources: Vec<_> = response
        .evidence
        .iter()
        .filter(|reference| {
            reference.kind == EvidenceKind::Segment
                && reference.role == EvidenceRole::SupportsAssertion
        })
        .collect();
    assert!(
        !sources.is_empty(),
        "no source evidence: {:?}",
        response.evidence
    );
    for source in &sources {
        assert_eq!(
            source.chunk_id.as_deref(),
            Some(chunk_id.to_string().as_str())
        );
        assert_eq!(
            source.data_id.as_deref(),
            Some(data_id.to_string().as_str())
        );
    }
    assert!(
        sources
            .iter()
            .any(|source| source.assertion_id.as_deref() == Some(edge_id.to_string().as_str())),
        "the extracted edge has no source: {sources:?}"
    );
    let SearchOutput::Text(text) = &response.result else {
        panic!("expected a text answer, got {:?}", response.result);
    };
    assert!(
        text.contains(&format!("(data_id: {data_id}, chunk_id: {chunk_id})")),
        "{text}"
    );
}
