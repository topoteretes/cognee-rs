#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Default cognify of dated text yields shared Timestamp nodes (SDK-1052,
//! temporal-memify-10) — no separate temporal pipeline involved.
//!
//! Run with: cargo test --package cognee-cognify --test default_cognify_timestamps

use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::mock::MockEmbeddingEngine;
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::error::{LlmError, LlmResult};
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_llm::{Llm, MessageRole};
use cognee_models::{Data, Timestamp};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use tempfile::TempDir;
use uuid::Uuid;

const MARIE_TEXT: &str = "Marie Curie was born in 1867. On 26 April 1986 the reactor exploded. \
The following night of 27 April, engineers worked.";
const PIERRE_TEXT: &str = "Pierre Curie corresponded with colleagues in 1867.";

/// Answers graph extraction with a fixed dated graph per document and every
/// other structured call with a summary. Records every graph-extraction system
/// prompt so the test can see the date hints.
struct DatedGraphLlm {
    graph_prompts: Mutex<Vec<String>>,
}

#[async_trait]
impl Llm for DatedGraphLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        Err(LlmError::FeatureNotSupported(
            "DatedGraphLlm only supports structured output".to_string(),
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
        let user = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        if !system.contains("# 2. Timestamps") {
            return Ok(serde_json::json!({"summary": "s", "description": "d"}));
        }
        // lock poison is unrecoverable
        self.graph_prompts.lock().unwrap().push(system);
        let (person, relationship) = if user.contains("Marie") {
            ("Marie Curie", "born_at")
        } else {
            ("Pierre Curie", "corresponded_at")
        };
        Ok(serde_json::json!({
            "nodes": [
                {"id": person, "name": person, "type": "Person", "description": "A physicist"},
                {"id": "1867", "name": "1867", "type": "Timestamp", "description": "A year"},
                {"id": "Paris", "name": "Paris", "type": "Timestamp", "description": "Not a time"}
            ],
            "edges": [
                {"source_node_id": person, "target_node_id": "1867", "relationship_name": relationship}
            ]
        }))
    }

    fn model(&self) -> &str {
        "dated-graph"
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
async fn default_cognify_writes_shared_timestamp_nodes() {
    let temp_dir = TempDir::new().expect("TempDir");
    let owner_id = Uuid::nil();
    let storage = Arc::new(MockStorage::new());
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> =
        Arc::new(LadybugAdapter::new(&graph_path).await.expect("ladybug"));
    graph_db.initialize().await.expect("initialize graph");

    let llm = Arc::new(DatedGraphLlm {
        graph_prompts: Mutex::new(Vec::new()),
    });
    let dataset_id = Uuid::new_v4();
    let db = make_db(dataset_id, owner_id).await;
    let marie = ingest(MARIE_TEXT, "marie.txt", &storage, owner_id).await;
    let pierre = ingest(PIERRE_TEXT, "pierre.txt", &storage, owner_id).await;

    cognify(
        vec![marie, pierre],
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
        &CognifyConfig::default(),
    )
    .await
    .expect("default cognify must complete against the fixture");

    // ── One shared Timestamp node, with its period ──────────────────────────
    let (nodes, edges) = graph_db.get_graph_data().await.expect("graph data");
    let timestamps: Vec<_> = nodes
        .iter()
        .filter(|(_, props)| props.get("type").and_then(|v| v.as_str()) == Some("Timestamp"))
        .collect();
    assert_eq!(timestamps.len(), 1, "one node for 1867: {timestamps:?}");
    let (ts_id, props) = timestamps[0];
    let expected = Timestamp::from_text("1867").unwrap();
    assert_eq!(*ts_id, expected.base.id.to_string());
    assert_eq!(props["timestamp_str"], "1867");
    assert_eq!(props["precision"], "year");
    assert_eq!(props["time_at"].as_i64(), Some(expected.time_at));
    assert_eq!(props["time_until"].as_i64(), Some(expected.time_until));

    // "Paris" typed Timestamp does not parse, so it stays an entity.
    assert!(nodes.iter().any(|(_, p)| {
        p.get("type").and_then(|v| v.as_str()) == Some("Entity")
            && p.get("name").and_then(|v| v.as_str()) == Some("paris")
    }));

    // ── `*_at` edges from each person, `contains` from each chunk ───────────
    let into_ts: Vec<_> = edges.iter().filter(|(_, tgt, _, _)| tgt == ts_id).collect();
    let rels: HashSet<&str> = into_ts.iter().map(|(_, _, rel, _)| rel.as_str()).collect();
    assert!(
        rels.contains("born_at") && rels.contains("corresponded_at"),
        "{rels:?}"
    );
    let contains: Vec<_> = into_ts
        .iter()
        .filter(|(_, _, rel, _)| rel == "contains")
        .collect();
    assert_eq!(contains.len(), 2, "both chunks link to the Timestamp");
    for (_, _, _, props) in &contains {
        assert_eq!(
            props.get("edge_text").and_then(|v| v.as_str()),
            Some("Document chunk mentions 1867: A year")
        );
    }

    // ── One ownership row per producing file, no vector fields ──────────────
    let rows = cognee_database::ops::graph_storage::get_nodes_by_dataset(&db, dataset_id)
        .await
        .expect("ledger rows");
    let ts_rows: Vec<_> = rows.iter().filter(|r| r.slug == expected.base.id).collect();
    let owners: HashSet<Uuid> = ts_rows.iter().map(|r| r.data_id).collect();
    assert_eq!(owners.len(), 2, "both files own the shared Timestamp");
    assert!(ts_rows.iter().all(|r| r.node_type == "Timestamp"));
    assert!(
        ts_rows
            .iter()
            .all(|r| r.indexed_fields == serde_json::json!([]))
    );

    // ── The date hint reached the default prompt ────────────────────────────
    let prompts = llm.graph_prompts.lock().unwrap();
    let hinted: HashMap<bool, usize> = prompts.iter().fold(HashMap::new(), |mut acc, p| {
        *acc.entry(p.contains("\"27 April\" -> 1986-04-27"))
            .or_default() += 1;
        acc
    });
    assert_eq!(
        hinted.get(&true),
        Some(&1),
        "only Marie's chunk carries the hint"
    );
}
