#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression test for issue #57: entity node ids must be deterministic so the
//! same entity merges across cognify runs instead of duplicating.
//!
//! Before the fix, `Entity`/`EntityType` got random `Uuid::new_v4()` ids, so
//! cognifying the same content twice produced *two* distinct graph nodes per
//! entity (the graph DB upserts by id, so distinct ids never merge). This test
//! cognifies the same data twice against one persistent graph and asserts the
//! entity node ids are identical across runs and the graph does not grow.
//!
//! Runs fully offline (mock LLM / embeddings / graph / vector) — no external
//! model or API required, so it is a real CI gate.

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, IngestDb, connect, initialize, ops};
use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_ingestion::AddPipeline;
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, Message};
use cognee_models::{DataInput, Entity, EntityType};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockVectorDB;
use cognee_vector::VectorDB;
use tempfile::TempDir;
use uuid::Uuid;

const TEXT: &str = "\
Alice is a senior software engineer at TechCorp, a technology company. \
She has worked on the cloud platform for three years.";

/// LLM stub that always extracts the same two entities regardless of input, so
/// two separate cognify runs resolve to the same entities.
#[derive(Clone)]
struct FixedGraphLlm;

#[async_trait::async_trait]
impl Llm for FixedGraphLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> cognee_llm::LlmResult<GenerationResponse> {
        Ok(GenerationResponse {
            content: String::new(),
            model: self.model().to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        _options: Option<GenerationOptions>,
    ) -> cognee_llm::LlmResult<serde_json::Value> {
        Ok(serde_json::json!({
            "nodes": [
                { "id": "alice", "name": "Alice", "type": "Person",
                  "description": "A software engineer." },
                { "id": "techcorp", "name": "TechCorp", "type": "Organization",
                  "description": "A technology company." }
            ],
            "edges": [
                { "source_node_id": "alice", "target_node_id": "techcorp",
                  "relationship_name": "works_at" }
            ]
        }))
    }

    fn model(&self) -> &str {
        "fixed-graph-fixture"
    }
}

#[tokio::test]
async fn test_entity_ids_are_stable_across_cognify_runs() {
    let temp_dir = TempDir::new().expect("temp dir");

    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let db_path = temp_dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create sqlite db file");
    let db_url = format!("sqlite://{}", db_path.display());
    let db = connect(&db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let database: Arc<DatabaseConnection> = Arc::new(db);

    // Persistent across both cognify runs — the point of the test. Keep a
    // concrete handle for `node_count()` (not on the trait).
    let mock_graph = Arc::new(MockGraphDB::new());
    let graph_db: Arc<dyn GraphDBTrait> = mock_graph.clone();
    graph_db.initialize().await.expect("graph_db.initialize");

    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
    let embedding_engine: Arc<dyn EmbeddingEngine> = Arc::new(MockEmbeddingEngine::new(8));
    let llm: Arc<dyn Llm> = Arc::new(FixedGraphLlm);
    let ontology: Arc<dyn cognee_ontology::OntologyResolver> =
        Arc::new(NoOpOntologyResolver::new());

    let owner_id = Uuid::nil();
    let dataset_name = "entity_id_determinism";
    // Keep the graph focused on entities — no summary/triplet nodes.
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

    let data_items = ingest
        .add(
            vec![DataInput::Text(TEXT.to_string())],
            dataset_name,
            owner_id,
            None,
        )
        .await
        .expect("ingest");

    let dataset = ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
        .await
        .expect("get_dataset_by_name")
        .expect("dataset exists");

    let run = |items: Vec<cognee_models::Data>| {
        let (llm, storage, graph_db, vector_db, embedding_engine, database, ontology, config) = (
            llm.clone(),
            storage.clone(),
            graph_db.clone(),
            vector_db.clone(),
            embedding_engine.clone(),
            database.clone(),
            ontology.clone(),
            config.clone(),
        );
        async move {
            cognify(
                items,
                dataset.id,
                Some(owner_id),
                None,
                None,
                llm,
                storage,
                graph_db,
                vector_db,
                embedding_engine,
                database,
                Arc::new(cognee_database::NoopPipelineRunRepository::new())
                    as Arc<dyn cognee_database::PipelineRunRepository>,
                Arc::new(cognee_core::RayonThreadPool::with_default_threads().unwrap())
                    as Arc<dyn cognee_core::CpuPool>,
                ontology,
                &config,
            )
            .await
            .expect("cognify")
        }
    };

    // ── First cognify run ───────────────────────────────────────────────────
    let result_1 = run(data_items.clone()).await;
    assert!(
        !result_1.already_completed,
        "run 1 should extract, not skip"
    );
    assert!(
        !result_1.entities.is_empty(),
        "run 1 should produce entities"
    );

    let ids_1 = entity_ids(&result_1);
    let node_count_1 = mock_graph.node_count();

    // The entity ids must match the deterministic class-namespaced scheme,
    // keyed on the LLM node id (matches Python `Entity.id_for(node_id)`).
    assert!(
        ids_1.contains(&Entity::id_for("alice")),
        "Alice entity id present"
    );
    assert!(
        ids_1.contains(&Entity::id_for("techcorp")),
        "TechCorp entity id present"
    );
    // EntityType nodes use the EntityType-namespaced id (no collision with entities).
    assert!(
        graph_db
            .has_node(&EntityType::id_for("Person").to_string())
            .await
            .unwrap(),
        "Person EntityType node present"
    );

    // ── Second cognify run over the SAME data, SAME graph ────────────────────
    let result_2 = run(data_items).await;
    let ids_2 = entity_ids(&result_2);
    let node_count_2 = mock_graph.node_count();

    // Core regression: same entities → same ids across runs.
    assert_eq!(
        ids_1, ids_2,
        "entity ids must be identical across cognify runs (deterministic)"
    );

    // End-to-end: re-cognifying does not grow the graph. Chunk/document ids were
    // already deterministic; only the (previously random) entity ids could have
    // caused growth. Pre-fix this doubled the entity nodes.
    assert_eq!(
        node_count_1, node_count_2,
        "graph node count must be stable across re-cognify (entities merge, not duplicate)"
    );
}

/// Collect the set of entity node ids from a cognify result.
fn entity_ids(result: &cognee_cognify::CognifyResult) -> std::collections::BTreeSet<Uuid> {
    result.entities.iter().map(|p| p.entity.base.id).collect()
}
