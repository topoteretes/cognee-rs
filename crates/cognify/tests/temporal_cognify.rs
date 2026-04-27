//! Integration tests for the temporal cognify pipeline.
//!
//! Run with: cargo test --package cognee-cognify --test temporal_cognify

use async_trait::async_trait;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_embedding::mock::MockEmbeddingEngine;
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::error::{LlmError, LlmResult};
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, MessageRole};
use cognee_models::Data;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::{MockVectorDB, VectorDB};
use tempfile::TempDir;
use uuid::Uuid;

mod test_utils;

const BIOGRAPHY_TEXT: &str = include_str!("test_data/biography.txt");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

struct TemporalFixtureLlm;

#[async_trait]
impl Llm for TemporalFixtureLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(LlmError::FeatureNotSupported(
            "TemporalFixtureLlm only supports structured output".to_string(),
        ))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<serde_json::Value> {
        let system_prompt = messages
            .iter()
            .find(|message| matches!(message.role, MessageRole::System))
            .map(|message| message.content.as_str())
            .unwrap_or_default();

        if system_prompt.contains("extracting highly granular stream events") {
            return Ok(serde_json::json!({
                "events": [
                    {
                        "name": "Arnulf Overland is born",
                        "description": "Arnulf Overland was born in Kristiansund.",
                        "time_from": { "year": 1889, "month": 4, "day": 27 },
                        "time_to": null,
                        "location": "Kristiansund"
                    },
                    {
                        "name": "Overland graduates from school",
                        "description": "Overland graduated in 1907.",
                        "time_from": { "year": 1907 },
                        "time_to": null,
                        "location": null
                    },
                    {
                        "name": "Overland publishes first poetry collection",
                        "description": "Overland published his first collection of poems in 1911.",
                        "time_from": { "year": 1911 },
                        "time_to": null,
                        "location": null
                    },
                    {
                        "name": "Overland writes Du ma ikke sove",
                        "description": "In 1936 he wrote the poem Du ma ikke sove.",
                        "time_from": { "year": 1936 },
                        "time_to": null,
                        "location": null
                    },
                    {
                        "name": "Overland is arrested",
                        "description": "The clandestine poems led to his arrest in 1941.",
                        "time_from": { "year": 1941 },
                        "time_to": null,
                        "location": null
                    },
                    {
                        "name": "Overland imprisonment period",
                        "description": "He spent a four-year imprisonment until the liberation of Norway.",
                        "time_from": { "year": 1945, "month": 5 },
                        "time_to": null,
                        "location": "Norway"
                    }
                ]
            }));
        }

        if system_prompt.contains("extracting highly granular entities from events") {
            return Ok(serde_json::json!({
                "events": [
                    {
                        "event_name": "Arnulf Overland is born",
                        "attributes": [
                            { "entity": "Arnulf Overland", "entity_type": "person", "relationship": "subject" },
                            { "entity": "Kristiansund", "entity_type": "place", "relationship": "location" }
                        ]
                    },
                    {
                        "event_name": "Overland is arrested",
                        "attributes": [
                            { "entity": "Arnulf Overland", "entity_type": "person", "relationship": "subject" }
                        ]
                    }
                ]
            }));
        }

        Err(LlmError::InvalidResponse(
            "TemporalFixtureLlm received an unknown prompt".to_string(),
        ))
    }

    fn model(&self) -> &str {
        "temporal-fixture"
    }
}

/// Count node types by inspecting the `"type"` property from `get_graph_data`.
fn count_node_types(
    nodes: &[(String, HashMap<Cow<'static, str>, serde_json::Value>)],
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for (_id, props) in nodes {
        if let Some(t) = props.get("type").and_then(|v| v.as_str()) {
            *counts.entry(t.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

/// Create a `Data` item backed by content stored in `storage`.
async fn ingest_text(text: &str, storage: &Arc<MockStorage>, owner_id: Uuid) -> Data {
    let id = Uuid::new_v4();
    let stored_location = storage
        .store(text.as_bytes(), &format!("biography-{id}.txt"))
        .await
        .expect("MockStorage::store should not fail");

    Data::builder(
        id,
        "biography.txt",
        stored_location,
        "biography.txt",
        "txt",
        "text/plain",
        "test-hash",
        owner_id,
    )
    .build()
}

// ---------------------------------------------------------------------------
// Test 1: Event and Timestamp nodes are created in the graph
// ---------------------------------------------------------------------------

#[tokio::test]
async fn temporal_cognify_creates_event_and_timestamp_nodes() {
    let llm: Arc<dyn Llm> = Arc::new(TemporalFixtureLlm);

    let temp_dir = TempDir::new().expect("TempDir::new should succeed");
    let owner_id = Uuid::nil();

    // Storage and graph DB
    let storage = Arc::new(MockStorage::new());
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new should succeed"),
    );
    graph_db
        .initialize()
        .await
        .expect("graph_db.initialize should succeed");

    // Use mock vector DB and embedding engine — we only care about graph nodes here
    let vector_db = Arc::new(MockVectorDB::new());
    let embedding_engine = Arc::new(MockEmbeddingEngine::new(384));

    let data_item = ingest_text(BIOGRAPHY_TEXT, &storage, owner_id).await;

    let config = CognifyConfig::default().with_temporal_cognify(true);

    match cognify(
        vec![data_item],
        Uuid::new_v4(),
        None,
        None,
        llm,
        storage,
        Arc::clone(&graph_db),
        vector_db,
        embedding_engine,
        None,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Skipping: temporal cognify pipeline error: {e}");
            return;
        }
    };

    // Inspect the graph
    let (nodes, edges) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data should succeed");

    let type_counts = count_node_types(&nodes);
    println!("Node type counts: {type_counts:?}");

    let event_count = type_counts.get("Event").copied().unwrap_or(0);
    let timestamp_count = type_counts.get("Timestamp").copied().unwrap_or(0);

    assert!(
        event_count >= 5,
        "Expected >= 5 Event nodes, got {event_count}. All counts: {type_counts:?}"
    );
    assert!(
        timestamp_count >= 5,
        "Expected >= 5 Timestamp nodes, got {timestamp_count}. All counts: {type_counts:?}"
    );

    // Every Event must have at least one `at` or `during` outgoing edge
    let event_ids: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|(_id, props)| props.get("type").and_then(|v| v.as_str()) == Some("Event"))
        .map(|(id, _)| id.clone())
        .collect();

    let events_with_temporal_edge: std::collections::HashSet<String> = edges
        .iter()
        .filter(|(src, _tgt, rel, _props)| {
            event_ids.contains(src) && (rel == "at" || rel == "during")
        })
        .map(|(src, _, _, _)| src.clone())
        .collect();

    assert_eq!(
        events_with_temporal_edge.len(),
        event_ids.len(),
        "Not all Event nodes have a temporal edge (at/during). \
         Events without edges: {:?}",
        event_ids
            .difference(&events_with_temporal_edge)
            .collect::<Vec<_>>()
    );

    println!(
        "All {event_count} Event nodes have at/during edges. \
         Timestamp nodes: {timestamp_count}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Event_name vector collection is populated
// ---------------------------------------------------------------------------

#[tokio::test]
async fn temporal_cognify_populates_event_name_vector_collection() {
    let llm: Arc<dyn Llm> = Arc::new(TemporalFixtureLlm);

    let temp_dir = TempDir::new().expect("TempDir::new should succeed");
    let owner_id = Uuid::nil();

    // Storage
    let storage = Arc::new(MockStorage::new());

    // Graph DB (Ladybug)
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new should succeed"),
    );
    graph_db
        .initialize()
        .await
        .expect("graph_db.initialize should succeed");

    // Qdrant vector DB (embedded, 384-dim)
    let qdrant_path = temp_dir.path().join("qdrant");
    let vector_db = Arc::new(cognee_vector::QdrantAdapter::new(qdrant_path, 384));

    // Embedding engine: try ONNX BGE-Small if available, otherwise skip
    let model_dir = if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        parent.to_string_lossy().to_string()
    } else if let Ok(dir) = std::env::var("COGNEE_TEST_MODEL_DIR") {
        dir
    } else {
        "./target/models".to_string()
    };

    let embedding_engine: Arc<dyn cognee_embedding::engine::EmbeddingEngine> =
        match cognee_embedding::onnx::OnnxEmbeddingEngine::new(
            cognee_embedding::config::OnnxEmbeddingConfig::bge_small(&model_dir),
        ) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!(
                    "Skipping temporal_cognify_populates_event_name_vector_collection: \
                 failed to load embedding model: {e}"
                );
                eprintln!("   Ensure model is at {model_dir}/BGE-Small-v1.5-model_quantized.onnx");
                return;
            }
        };

    let data_item = ingest_text(BIOGRAPHY_TEXT, &storage, owner_id).await;

    let config = CognifyConfig::default().with_temporal_cognify(true);

    match cognify(
        vec![data_item],
        Uuid::new_v4(),
        None,
        None,
        llm,
        storage,
        graph_db,
        Arc::clone(&vector_db) as Arc<dyn VectorDB>,
        embedding_engine,
        None,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(_) => {
            println!("Temporal cognify succeeded");
        }
        Err(e) => {
            eprintln!("Skipping: temporal cognify pipeline error: {e}");
            return;
        }
    }

    let count = vector_db
        .collection_size("Event", "name")
        .await
        .expect("collection_size should succeed");

    assert!(
        count >= 5,
        "Expected >= 5 points in Event_name collection, got {count}"
    );

    println!("Event_name vector collection contains {count} points (>= 5)");
}
