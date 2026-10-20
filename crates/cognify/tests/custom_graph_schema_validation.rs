#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! A runtime graph schema is validated before the cognify pipeline starts
//! (SDK-1124): an invalid schema, or one paired with `temporal_cognify`,
//! fails `cognify()` before any document is classified, chunked or sent to
//! the LLM, as Python builds its model class before running a task.
//!
//! Run with: cargo test --package cognee-cognify --test custom_graph_schema_validation

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cognee_cognify::{CognifyConfig, CognifyError, cognify};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::mock::MockEmbeddingEngine;
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::Llm;
use cognee_llm::error::{LlmError, LlmResult};
use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
use cognee_models::Data;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::MockStorage;
use cognee_vector::MockVectorDB;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

/// Counts every LLM call; none should happen.
#[derive(Default)]
struct CountingLlm {
    calls: AtomicUsize,
}

#[async_trait]
impl Llm for CountingLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(LlmError::FeatureNotSupported("not expected".to_string()))
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<serde_json::Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(LlmError::FeatureNotSupported("not expected".to_string()))
    }

    fn model(&self) -> &str {
        "counting"
    }
}

async fn make_db(dataset_id: Uuid, owner_id: Uuid) -> Arc<DatabaseConnection> {
    let conn = connect("sqlite::memory:").await.expect("connect");
    initialize(&conn).await.expect("initialize");
    cognee_database::ops::datasets::create_dataset(
        &conn,
        cognee_models::Dataset::new("schemas".into(), owner_id, None, dataset_id),
    )
    .await
    .expect("seed dataset");
    Arc::new(conn)
}

/// Runs `cognify()` over one data item whose content location does not exist,
/// so a pipeline that got as far as chunking would fail with a storage error
/// instead of the configuration error the caller expects.
async fn run(config: CognifyConfig) -> (Result<(), CognifyError>, usize, usize) {
    let temp_dir = TempDir::new().expect("TempDir");
    let owner_id = Uuid::nil();
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> =
        Arc::new(LadybugAdapter::new(&graph_path).await.expect("ladybug"));
    graph_db.initialize().await.expect("initialize graph");
    let llm = Arc::new(CountingLlm::default());
    let dataset_id = Uuid::new_v4();
    let db = make_db(dataset_id, owner_id).await;
    let data = Data::builder(
        Uuid::new_v4(),
        "missing.txt",
        "missing-location.txt",
        "missing.txt",
        "txt",
        "text/plain",
        "hash-missing",
        owner_id,
    )
    .build();

    let result = cognify(
        vec![data],
        dataset_id,
        None,
        None,
        None,
        Arc::clone(&llm) as Arc<dyn Llm>,
        Arc::new(MockStorage::new()),
        Arc::clone(&graph_db),
        Arc::new(MockVectorDB::new()),
        Arc::new(MockEmbeddingEngine::new(16)),
        Arc::clone(&db) as Arc<dyn cognee_database::RelationalDb>,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("pool")),
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    .map(|_| ());
    let (nodes, _) = graph_db.get_graph_data().await.expect("graph data");
    (result, llm.calls.load(Ordering::SeqCst), nodes.len())
}

fn assert_config_error(result: Result<(), CognifyError>, needle: &str) {
    match result {
        Err(CognifyError::ConfigError(message)) => {
            assert!(message.contains(needle), "unexpected message: {message}");
        }
        other => panic!("expected a config error naming {needle:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn an_invalid_graph_schema_fails_before_the_pipeline_runs() {
    let (result, llm_calls, nodes) = run(CognifyConfig::default().with_graph_schema(json!({
        "title": "research_graph",
        "properties": {"name": {"type": "string"}}
    })))
    .await;
    assert_config_error(result, "research_graph");
    assert_eq!(llm_calls, 0, "no chunk may reach the LLM");
    assert_eq!(nodes, 0, "nothing may be written to the graph");
}

#[tokio::test]
async fn a_graph_schema_with_temporal_cognify_fails_before_the_pipeline_runs() {
    let (result, llm_calls, nodes) = run(CognifyConfig::default()
        .with_graph_schema(json!({
            "title": "ResearchGraph",
            "properties": {"name": {"type": "string"}}
        }))
        .with_temporal_cognify(true))
    .await;
    assert_config_error(result, "temporal_cognify");
    assert_eq!(llm_calls, 0, "no chunk may reach the LLM");
    assert_eq!(nodes, 0, "nothing may be written to the graph");
}
