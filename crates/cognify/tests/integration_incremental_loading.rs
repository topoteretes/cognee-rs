#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for incremental loading configuration behavior.
//!
//! The data-processing history layer is not wired into the current Rust `cognify()` API.
//! These tests validate that:
//! 1. The incremental_loading config flag is configurable.
//! 2. The pipeline executes successfully with incremental loading enabled.
//! 3. The pipeline executes successfully with incremental loading disabled.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::{EmbeddingEngine, error::EmbeddingError};
use cognee_graph::MockGraphDB;
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmError, Message};
use cognee_models::Data;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone)]
struct TestMockLlm;

#[async_trait]
impl Llm for TestMockLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> Result<GenerationResponse, LlmError> {
        Ok(GenerationResponse {
            content: "ok".to_string(),
            model: "test-mock".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> Result<Value, LlmError> {
        // Inspect the schema to return the right shape for each pipeline stage.
        let schema_str = json_schema.to_string();
        if schema_str.contains("summary") {
            // Summarization step expects SummarizedContent
            Ok(
                serde_json::json!({"summary": "A test summary.", "description": "A test description."}),
            )
        } else {
            // Graph extraction step expects nodes/edges
            Ok(serde_json::json!({"nodes": [], "edges": []}))
        }
    }

    fn model(&self) -> &str {
        "test-mock"
    }
}

#[derive(Clone)]
struct TestMockEmbedding {
    dimension: usize,
}

impl TestMockEmbedding {
    fn new(dimension: usize) -> Self {
        Self { dimension }
    }
}

#[async_trait]
impl EmbeddingEngine for TestMockEmbedding {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        Ok(texts
            .iter()
            .map(|_| vec![0.0; self.dimension])
            .collect::<Vec<_>>())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn batch_size(&self) -> usize {
        16
    }

    fn max_sequence_length(&self) -> usize {
        512
    }
}

fn create_test_data(name: &str, owner_id: Uuid) -> Data {
    Data::builder(
        Uuid::new_v4(),
        name,
        format!("storage/{name}"),
        format!("file://{name}"),
        "txt",
        "text/plain",
        format!("hash_{name}"),
        owner_id,
    )
    .build()
}

async fn run_pipeline_with_incremental_flag(
    incremental_loading: bool,
) -> Result<usize, Box<dyn std::error::Error>> {
    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());
    let embedding_engine = Arc::new(TestMockEmbedding::new(64));
    let llm = Arc::new(TestMockLlm);

    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    let mut data = create_test_data("doc.txt", owner_id);

    let stored_location = storage
        .store(b"First document", &data.raw_data_location)
        .await?;
    data.raw_data_location = stored_location;

    let config = CognifyConfig::default().with_incremental_loading(incremental_loading);

    let db: Arc<DatabaseConnection> = {
        let conn = connect("sqlite::memory:").await?;
        initialize(&conn).await?;
        Arc::new(conn)
    };
    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads().map_err(|e| {
            Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error>
        })?,
    );

    let result = cognify(
        vec![data],
        dataset_id,
        None,
        None,
        None,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        db,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    Ok(result.chunks.len())
}

#[test]
fn test_incremental_loading_is_configurable() {
    let default_config = CognifyConfig::default();
    assert!(default_config.incremental_loading);

    let disabled = CognifyConfig::default().with_incremental_loading(false);
    assert!(!disabled.incremental_loading);
}

#[tokio::test]
async fn test_pipeline_runs_with_incremental_loading_enabled() {
    let chunks = run_pipeline_with_incremental_flag(true)
        .await
        .expect("Pipeline should run with incremental_loading=true");
    assert_eq!(chunks, 1);
}

#[tokio::test]
async fn test_pipeline_runs_with_incremental_loading_disabled() {
    let chunks = run_pipeline_with_incremental_flag(false)
        .await
        .expect("Pipeline should run with incremental_loading=false");
    assert_eq!(chunks, 1);
}
