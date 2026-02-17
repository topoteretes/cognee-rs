//! Integration tests for embedding generation in the cognify pipeline.
//!
//! These tests require:
//! - Environment variables: OPENAI_URL, OPENAI_TOKEN
//! - Model file: examples/target/models/BGE-Small-v1.5-model_quantized.onnx
//! - Tokenizer: examples/target/models/bge-small-tokenizer.json
//!
//! Run with: cargo test --package cognee-cognify --test integration_embeddings -- --ignored

use cognee_cognify::{CognifyPipeline, CognifyResult};
use cognee_embedding::{config::EmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::MockGraphDB;
use cognee_llm::OpenAIAdapter;
use cognee_models::Data;
use cognee_storage::{MockStorage, StorageTrait};
use std::sync::Arc;
use uuid::Uuid;

/// Helper to get environment variables or skip test
fn get_env_or_skip(var_name: &str) -> Result<String, ()> {
    std::env::var(var_name).map_err(|_| {
        eprintln!("⚠️  Skipping test: {} not set", var_name);
    })
}

/// Helper to create OpenAI adapter from environment variables
fn create_adapter_from_env() -> Result<Arc<OpenAIAdapter>, ()> {
    let base_url = get_env_or_skip("OPENAI_URL")?;
    let api_token = get_env_or_skip("OPENAI_TOKEN")?;

    OpenAIAdapter::new("llama3.2:3b", api_token, Some(base_url))
        .map(Arc::new)
        .map_err(|e| {
            eprintln!("⚠️  Failed to create adapter: {}", e);
        })
}

#[tokio::test]
#[ignore] // Requires model file and LLM - run with --ignored flag
async fn test_pipeline_with_embeddings() {
    // Skip if env vars not set
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => return,
    };

    // 1. Setup storage and graph DB
    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());

    // 2. Setup embedding engine (BGE-Small)
    let embedding_config = EmbeddingConfig::bge_small("examples/target/models");
    let embedding_engine = match OnnxEmbeddingEngine::new(embedding_config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping test: Failed to load embedding model: {}", e);
            eprintln!("   Ensure model is downloaded to examples/target/models/");
            return;
        }
    };

    // 3. Create pipeline with embeddings
    let pipeline = CognifyPipeline::with_embeddings(storage.clone(), graph_db, embedding_engine);

    // 4. Create test data
    let text = "TechCorp is an organization based in San Francisco. \
                Alice works at TechCorp as a software engineer.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{}", id);

    // Store text in mock storage
    storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::new(
        id,
        "test-doc.txt".to_string(),
        location.clone(),
        "test-doc.txt".to_string(),
        "txt".to_string(),
        "text/plain".to_string(),
        "test-hash".to_string(),
        owner_id,
    );

    // 5. Run cognify pipeline
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = pipeline
        .cognify(vec![data_item], dataset_id, 512, llm)
        .await
        .expect("Cognify pipeline failed");

    // 6. Verify embeddings were generated
    assert!(!result.embeddings.is_empty(), "No embeddings generated");

    // 7. Verify embeddings for chunks
    let chunk_embeddings: Vec<_> = result
        .embeddings
        .iter()
        .filter(|e| e.data_type == "DocumentChunk")
        .collect();
    assert!(
        !chunk_embeddings.is_empty(),
        "No chunk embeddings generated"
    );

    // 8. Verify embedding dimensions (BGE-Small = 384)
    for embedding in &result.embeddings {
        assert_eq!(
            embedding.dimensions(),
            384,
            "Expected 384 dimensions for BGE-Small"
        );

        // Verify L2 normalization
        let norm = embedding.norm();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "Embedding not normalized: norm = {}",
            norm
        );
    }

    // 9. Verify embeddings for different data types
    let chunk_count = result
        .embeddings
        .iter()
        .filter(|e| e.data_type == "DocumentChunk")
        .count();
    let entity_count = result
        .embeddings
        .iter()
        .filter(|e| e.data_type == "Entity")
        .count();
    let summary_count = result
        .embeddings
        .iter()
        .filter(|e| e.data_type == "TextSummary")
        .count();

    println!(
        "✓ Embeddings generated: {} chunks, {} entities, {} summaries",
        chunk_count, entity_count, summary_count
    );

    assert!(chunk_count > 0, "No chunk embeddings");
}

#[tokio::test]
async fn test_pipeline_without_embeddings() {
    // This test doesn't require LLM or embedding model
    // It just verifies that the pipeline works without embeddings

    // 1. Setup storage and graph DB
    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());

    // 2. Create pipeline WITHOUT embeddings
    let pipeline: CognifyPipeline<MockStorage, MockGraphDB, OnnxEmbeddingEngine> =
        CognifyPipeline::new(storage.clone(), graph_db);

    // 3. Create test data
    let text = "Simple test text.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{}", id);

    storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::new(
        id,
        "test.txt".to_string(),
        location.clone(),
        "test.txt".to_string(),
        "txt".to_string(),
        "text/plain".to_string(),
        "test-hash".to_string(),
        owner_id,
    );

    // 4. Skip if LLM not available
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => {
            eprintln!("⚠️  Skipping test: LLM not configured");
            return;
        }
    };

    // 5. Run cognify pipeline
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = pipeline
        .cognify(vec![data_item], dataset_id, 512, llm)
        .await
        .expect("Cognify pipeline failed");

    // 6. Verify NO embeddings generated (engine not provided)
    assert_eq!(
        result.embeddings.len(),
        0,
        "Embeddings should be empty when engine not provided"
    );

    // 7. Verify other pipeline stages still work
    assert!(!result.chunks.is_empty(), "Chunks should be generated");
}

#[tokio::test]
#[ignore] // Requires model and LLM
async fn test_embedding_semantic_similarity() {
    // Test that embeddings capture semantic similarity

    // Skip if env vars not set
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => return,
    };

    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());

    let embedding_config = EmbeddingConfig::bge_small("examples/target/models");
    let embedding_engine = match OnnxEmbeddingEngine::new(embedding_config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping test: Failed to load embedding model: {}", e);
            return;
        }
    };

    let pipeline = CognifyPipeline::with_embeddings(storage.clone(), graph_db, embedding_engine);

    // Create two semantically similar documents
    let texts = [
        "Machine learning is a subset of artificial intelligence.",
        "Deep learning is a type of machine learning algorithm.",
    ];

    let dataset_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();

    let mut all_embeddings = Vec::new();

    for (i, text) in texts.iter().enumerate() {
        let id = Uuid::new_v4();
        let location = format!("test-data-{}", id);

        storage
            .store(text.as_bytes(), &location)
            .await
            .expect("Failed to store text");

        let data_item = Data::new(
            id,
            format!("doc-{}.txt", i),
            location.clone(),
            format!("doc-{}.txt", i),
            "txt".to_string(),
            "text/plain".to_string(),
            format!("hash-{}", i),
            owner_id,
        );

        let result: CognifyResult = pipeline
            .cognify(vec![data_item], dataset_id, 512, llm.clone())
            .await
            .expect("Cognify failed");

        all_embeddings.push(result.embeddings);
    }

    // Compare embeddings from both documents
    if !all_embeddings[0].is_empty() && !all_embeddings[1].is_empty() {
        let emb1 = &all_embeddings[0][0];
        let emb2 = &all_embeddings[1][0];

        let similarity = emb1.cosine_similarity(emb2).expect("Dimension mismatch");

        println!("✓ Semantic similarity: {:.4}", similarity);

        // Semantically similar texts should have high cosine similarity
        assert!(
            similarity > 0.5,
            "Expected high similarity for related ML texts, got {}",
            similarity
        );
    }
}
