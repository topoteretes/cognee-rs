#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Cognify with the cascade extraction strategy (SDK-1052, extraction-28):
//! three stages per chunk, each `rounds` times, feeding the usual expansion.
//!
//! Run with: cargo test --package cognee-cognify --test cascade_extraction

use async_trait::async_trait;
use std::sync::{Arc, Mutex};

use cognee_cognify::{CognifyConfig, ExtractionStrategy, cognify};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::mock::MockEmbeddingEngine;
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::error::{LlmError, LlmResult};
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, MessageRole};
use cognee_models::{Data, Entity};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use tempfile::TempDir;
use uuid::Uuid;

/// Answers each cascade stage by its system prompt; counts the stage calls.
struct CascadeLlm {
    stage_calls: Mutex<Vec<&'static str>>,
}

#[async_trait]
impl Llm for CascadeLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(LlmError::FeatureNotSupported(
            "structured output only".to_string(),
        ))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<serde_json::Value> {
        let system = messages
            .iter()
            .find(|m| matches!(m.role, MessageRole::System))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let stage = if system.contains("node identification") {
            "nodes"
        } else if system.contains("focusing on relationships") {
            "relationships"
        } else if system.contains("extraction of graph triplets") {
            "triplets"
        } else if system.contains("# 2. Timestamps") {
            "single_pass"
        } else {
            return Ok(serde_json::json!({"summary": "s", "description": "d"}));
        };
        // lock poison is unrecoverable
        self.stage_calls.lock().unwrap().push(stage);
        Ok(match stage {
            "nodes" => serde_json::json!({"nodes": ["Ada Lovelace", "Analytical Engine"]}),
            "relationships" => serde_json::json!({
                "nodes": ["Charles Babbage"],
                "relationship_names": ["wrote_notes_on"]
            }),
            "triplets" => serde_json::json!({
                "nodes": [
                    {"id": "ada", "name": "Ada Lovelace", "type": "Person", "description": "Mathematician"},
                    {"id": "engine", "name": "Analytical Engine", "type": "Machine", "description": ""}
                ],
                "edges": [
                    {"source_node_id": "ada", "target_node_id": "engine", "relationship_name": "wrote_notes_on"}
                ]
            }),
            _ => serde_json::json!({"nodes": [], "edges": []}),
        })
    }

    fn model(&self) -> &str {
        "cascade"
    }
}

async fn make_db(dataset_id: Uuid, owner_id: Uuid) -> Arc<DatabaseConnection> {
    let conn = connect("sqlite::memory:").await.expect("connect");
    initialize(&conn).await.expect("initialize");
    cognee_database::ops::datasets::create_dataset(
        &conn,
        cognee_models::Dataset::new("dated".into(), owner_id, None, dataset_id),
    )
    .await
    .expect("seed dataset");
    Arc::new(conn)
}

async fn ingest(text: &str, name: &str, storage: &Arc<MockStorage>, owner_id: Uuid) -> Data {
    let id = Uuid::new_v4();
    let location = storage
        .store(text.as_bytes(), &format!("{name}-{id}.txt"))
        .await
        .expect("store");
    Data::builder(
        id,
        name,
        location,
        name,
        "txt",
        "text/plain",
        format!("hash-{name}"),
        owner_id,
    )
    .build()
}

#[tokio::test]
async fn cascade_strategy_runs_three_stages_and_stores_their_graph() {
    let temp_dir = TempDir::new().expect("TempDir");
    let owner_id = Uuid::nil();
    let storage = Arc::new(MockStorage::new());
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> =
        Arc::new(LadybugAdapter::new(&graph_path).await.expect("ladybug"));
    graph_db.initialize().await.expect("initialize graph");

    let llm = Arc::new(CascadeLlm {
        stage_calls: Mutex::new(Vec::new()),
    });
    let dataset_id = Uuid::new_v4();
    let db = make_db(dataset_id, owner_id).await;
    let data = ingest(
        "Ada Lovelace wrote notes on the Analytical Engine.",
        "ada.txt",
        &storage,
        owner_id,
    )
    .await;

    cognify(
        vec![data],
        dataset_id,
        None,
        None,
        None,
        Arc::clone(&llm) as Arc<dyn Llm>,
        storage,
        Arc::clone(&graph_db),
        Arc::new(MockVectorDB::new()),
        Arc::new(MockEmbeddingEngine::new(16)),
        Arc::clone(&db) as Arc<dyn cognee_database::RelationalDb>,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("pool")),
        Arc::new(NoOpOntologyResolver::new()),
        &CognifyConfig::default()
            .with_extraction_strategy(ExtractionStrategy::Cascade { rounds: 1 }),
    )
    .await
    .expect("cascade cognify must complete against the fixture");

    // One chunk, one round: one call per stage, and never the single-pass prompt.
    let calls = llm.stage_calls.lock().unwrap().clone();
    assert_eq!(calls, vec!["nodes", "relationships", "triplets"]);

    let (nodes, edges) = graph_db.get_graph_data().await.expect("graph data");
    let ada = Entity::id_for("Ada Lovelace").to_string();
    let engine = Entity::id_for("Analytical Engine").to_string();
    assert!(
        nodes
            .iter()
            .any(|(id, p)| *id == ada && p.get("type").and_then(|v| v.as_str()) == Some("Entity"))
    );
    assert!(
        edges
            .iter()
            .any(|(s, t, rel, _)| *s == ada && *t == engine && rel == "wrote_notes_on")
    );
}
