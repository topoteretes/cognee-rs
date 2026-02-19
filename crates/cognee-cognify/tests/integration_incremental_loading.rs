//! Integration tests for incremental loading feature (Phase 4).
//!
//! Tests verify that:
//! 1. When incremental_loading is enabled, already-processed data is skipped
//! 2. New data is processed normally
//! 3. Processing history can be cleared
//! 4. When disabled, all data is always processed

use std::sync::Arc;

use async_trait::async_trait;
use cognee_cognify::{CognifyConfig, CognifyPipeline};
use cognee_database::{DatabaseTrait, MockDatabase};
use cognee_embedding::engine::EmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_llm::{Llm, LlmError, Message};
use cognee_models::{Data, Embedding};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::MockVectorDB;
use serde::de::DeserializeOwned;
use uuid::Uuid;

// Simple mock LLM for testing
#[derive(Clone)]
struct TestMockLlm;

#[async_trait]
impl Llm for TestMockLlm {
    async fn chat(&self, _messages: Vec<Message>) -> Result<String, LlmError> {
        Ok(r#"{"nodes": [], "edges": []}"#.to_string())
    }

    async fn chat_structured<T: DeserializeOwned + Send>(
        &self,
        _messages: Vec<Message>,
    ) -> Result<T, LlmError> {
        // Return empty KnowledgeGraph structure
        let json = r#"{"nodes": [], "edges": []}"#;
        serde_json::from_str(json)
            .map_err(|e| LlmError::SerializationError(format!("Mock deserialization: {}", e)))
    }

    fn model(&self) -> &str {
        "test-mock"
    }
}

// Simple mock embedding engine for testing
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
    async fn embed(&self, texts: &[&str]) -> Result<Vec<Embedding>, String> {
        Ok(texts
            .iter()
            .map(|_| Embedding {
                vector: vec![0.0; self.dimension],
            })
            .collect())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Helper to create test Data item
fn create_test_data(name: &str, content: &str, owner_id: Uuid) -> Data {
    Data::new(
        Uuid::new_v4(),
        name.to_string(),
        format!("storage/{}", name),
        format!("file://{}", name),
        "txt".to_string(),
        "text/plain".to_string(),
        format!("hash_{}", name),
        owner_id,
    )
}

#[tokio::test]
async fn test_incremental_loading_skips_processed_data() {
    let storage = Arc::new(MockStorage::new());
    let database = Arc::new(MockDatabase::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());
    let embedding_engine = Arc::new(TestMockEmbedding::new(384));
    let llm = Arc::new(TestMockLlm);

    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    // Create test data
    let data1 = create_test_data("doc1.txt", "First document", owner_id);
    let data2 = create_test_data("doc2.txt", "Second document", owner_id);

    // Store the content in mock storage
    storage
        .store(b"First document", &data1.raw_data_location)
        .await
        .unwrap();
    storage
        .store(b"Second document", &data2.raw_data_location)
        .await
        .unwrap();

    // Initialize database
    database.initialize().await.unwrap();

    // Create both data items in database
    database.create_data(data1.clone()).await.unwrap();
    database.create_data(data2.clone()).await.unwrap();

    // Create dataset and attach both data items
    let dataset = cognee_models::Dataset::new("test_dataset".to_string(), owner_id);
    database.create_dataset(dataset.clone()).await.unwrap();
    database
        .attach_data_to_dataset(dataset.id, data1.id)
        .await
        .unwrap();
    database
        .attach_data_to_dataset(dataset.id, data2.id)
        .await
        .unwrap();

    // Mark data1 as already processed
    database
        .mark_data_processed(&[data1.id], "cognify_pipeline")
        .await
        .unwrap();

    // Create pipeline with incremental loading enabled
    let config = CognifyConfig::default().with_incremental_loading(true);
    let pipeline = CognifyPipeline::with_config(
        storage.clone(),
        graph_db.clone(),
        vector_db.clone(),
        embedding_engine.clone(),
        config,
    );

    // Run cognify with both data items
    let result = pipeline
        .cognify(
            vec![data1.clone(), data2.clone()],
            dataset.id,
            llm.clone(),
            database.clone(),
        )
        .await
        .unwrap();

    // Only data2 should have been processed (data1 was already marked as processed)
    // Since MockLlm returns empty KnowledgeGraph, we won't have entities/edges
    // But we can verify that data2 was marked as processed
    let is_data1_processed = database
        .is_data_processed(data1.id, "cognify_pipeline")
        .await
        .unwrap();
    let is_data2_processed = database
        .is_data_processed(data2.id, "cognify_pipeline")
        .await
        .unwrap();

    assert!(is_data1_processed, "data1 should remain processed");
    assert!(is_data2_processed, "data2 should be newly processed");

    // Result should be from data2 only (1 chunk from "Second document")
    assert_eq!(result.chunks.len(), 1, "Should process only new data");
}

#[tokio::test]
async fn test_incremental_loading_disabled_processes_all() {
    let storage = Arc::new(MockStorage::new());
    let database = Arc::new(MockDatabase::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());
    let embedding_engine = Arc::new(TestMockEmbedding::new(384));
    let llm = Arc::new(TestMockLlm);

    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    // Create test data
    let data1 = create_test_data("doc1.txt", "First document", owner_id);

    // Store the content
    storage
        .store(b"First document", &data1.raw_data_location)
        .await
        .unwrap();

    // Initialize database
    database.initialize().await.unwrap();
    database.create_data(data1.clone()).await.unwrap();

    // Mark as already processed
    database
        .mark_data_processed(&[data1.id], "cognify_pipeline")
        .await
        .unwrap();

    // Create pipeline with incremental loading DISABLED (default)
    let config = CognifyConfig::default(); // incremental_loading = false by default
    let pipeline = CognifyPipeline::with_config(
        storage.clone(),
        graph_db.clone(),
        vector_db.clone(),
        embedding_engine.clone(),
        config,
    );

    // Run cognify
    let result = pipeline
        .cognify(vec![data1.clone()], dataset_id, llm.clone(), database.clone())
        .await
        .unwrap();

    // Should process the data even though it was marked as processed
    assert_eq!(
        result.chunks.len(),
        1,
        "Should process data even when already marked (full reprocess mode)"
    );
}

#[tokio::test]
async fn test_clear_processing_history() {
    let database = Arc::new(MockDatabase::new());
    database.initialize().await.unwrap();

    let data_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();

    // Create and mark data as processed
    let data = create_test_data("doc.txt", "Test", owner_id);
    database.create_data(data.clone()).await.unwrap();
    database
        .mark_data_processed(&[data.id], "cognify_pipeline")
        .await
        .unwrap();

    // Verify it's processed
    let is_processed = database
        .is_data_processed(data.id, "cognify_pipeline")
        .await
        .unwrap();
    assert!(is_processed);

    // Clear processing history
    database
        .clear_processing_history("cognify_pipeline")
        .await
        .unwrap();

    // Verify it's no longer processed
    let is_processed_after = database
        .is_data_processed(data.id, "cognify_pipeline")
        .await
        .unwrap();
    assert!(!is_processed_after, "Should be unprocessed after clearing");
}

#[tokio::test]
async fn test_clear_specific_data_processing() {
    let database = Arc::new(MockDatabase::new());
    database.initialize().await.unwrap();

    let owner_id = Uuid::new_v4();
    let data1 = create_test_data("doc1.txt", "First", owner_id);
    let data2 = create_test_data("doc2.txt", "Second", owner_id);

    database.create_data(data1.clone()).await.unwrap();
    database.create_data(data2.clone()).await.unwrap();

    // Mark both as processed
    database
        .mark_data_processed(&[data1.id, data2.id], "cognify_pipeline")
        .await
        .unwrap();

    // Clear only data1
    database
        .clear_data_processing(&[data1.id])
        .await
        .unwrap();

    // Verify data1 is cleared but data2 is still processed
    let is_data1_processed = database
        .is_data_processed(data1.id, "cognify_pipeline")
        .await
        .unwrap();
    let is_data2_processed = database
        .is_data_processed(data2.id, "cognify_pipeline")
        .await
        .unwrap();

    assert!(!is_data1_processed, "data1 should be cleared");
    assert!(is_data2_processed, "data2 should remain processed");
}

#[tokio::test]
async fn test_get_unprocessed_data() {
    let database = Arc::new(MockDatabase::new());
    database.initialize().await.unwrap();

    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    // Create dataset
    let dataset = cognee_models::Dataset::new("test_dataset".to_string(), owner_id);
    database.create_dataset(dataset.clone()).await.unwrap();

    // Create 3 data items
    let data1 = create_test_data("doc1.txt", "First", owner_id);
    let data2 = create_test_data("doc2.txt", "Second", owner_id);
    let data3 = create_test_data("doc3.txt", "Third", owner_id);

    database.create_data(data1.clone()).await.unwrap();
    database.create_data(data2.clone()).await.unwrap();
    database.create_data(data3.clone()).await.unwrap();

    // Attach all to dataset
    database
        .attach_data_to_dataset(dataset.id, data1.id)
        .await
        .unwrap();
    database
        .attach_data_to_dataset(dataset.id, data2.id)
        .await
        .unwrap();
    database
        .attach_data_to_dataset(dataset.id, data3.id)
        .await
        .unwrap();

    // Mark data1 and data2 as processed
    database
        .mark_data_processed(&[data1.id, data2.id], "cognify_pipeline")
        .await
        .unwrap();

    // Get unprocessed data
    let unprocessed = database
        .get_unprocessed_data(dataset.id, "cognify_pipeline")
        .await
        .unwrap();

    // Should only return data3
    assert_eq!(unprocessed.len(), 1);
    assert_eq!(unprocessed[0].id, data3.id);
}
