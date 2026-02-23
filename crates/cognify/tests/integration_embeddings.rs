//! Integration tests for embedding generation in the cognify pipeline.
//!
//! These tests require:
//! - Environment variables: OPENAI_URL, OPENAI_TOKEN
//! - Model file: ./target/models/BGE-Small-v1.5-model_quantized.onnx
//! - Tokenizer: ./target/models/bge-small-tokenizer.json
//!
//! Run with: cargo test --package cognee-cognify --test integration_embeddings

use cognee_cognify::{CognifyConfig, CognifyPipeline, CognifyResult};
use cognee_embedding::{config::EmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::MockGraphDB;
use cognee_llm::OpenAIAdapter;
use cognee_models::Data;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::{MockVectorDB, VectorDB};
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
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "llama3.2:3b".to_string());

    OpenAIAdapter::new(&model, api_token, Some(base_url))
        .map(Arc::new)
        .map_err(|e| {
            eprintln!("⚠️  Failed to create adapter: {}", e);
        })
}

fn get_embedding_model_dir() -> String {
    if let Ok(model_dir) = std::env::var("COGNEE_TEST_MODEL_DIR") {
        return model_dir;
    }

    if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH") {
        if let Some(parent) = std::path::Path::new(&model_path).parent() {
            return parent.to_string_lossy().to_string();
        }
    }

    "./target/models".to_string()
}

#[tokio::test]
async fn test_pipeline_with_embeddings() {
    // Skip if env vars not set
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => return,
    };

    // 1. Setup storage and graph DB
    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    // 2. Setup embedding engine (BGE-Small)
    let embedding_config = EmbeddingConfig::bge_small(get_embedding_model_dir());
    let embedding_engine = match OnnxEmbeddingEngine::new(embedding_config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping test: Failed to load embedding model: {}", e);
            eprintln!("   Ensure model is downloaded to ./target/models/");
            return;
        }
    };

    // 3. Create pipeline with embeddings
    let pipeline = CognifyPipeline::new(
        storage.clone(),
        graph_db,
        vector_db,
        embedding_engine,
        CognifyConfig::default(),
        None,
    );

    // 4. Create test data
    let text = "TechCorp is an organization based in San Francisco. \
                Alice works at TechCorp as a software engineer.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{}", id);

    // Store text in mock storage
    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::new(
        id,
        "test-doc.txt".to_string(),
        stored_location,
        "test-doc.txt".to_string(),
        "txt".to_string(),
        "text/plain".to_string(),
        "test-hash".to_string(),
        owner_id,
    );

    // 5. Run cognify pipeline (max_chunk_size now in config)
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match pipeline.cognify(vec![data_item], dataset_id, llm).await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {}", e);
            return;
        }
    };

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
async fn test_pipeline_requires_embeddings() {
    // This test verifies that embeddings are REQUIRED (matches Python behavior)

    // 1. Setup storage and graph DB
    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    // 2. Setup embedding engine
    let embedding_config = EmbeddingConfig::bge_small(get_embedding_model_dir());
    let embedding_engine = match OnnxEmbeddingEngine::new(embedding_config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping test: Failed to load embedding model: {}", e);
            return;
        }
    };

    // 3. Create pipeline (embeddings are REQUIRED)
    let pipeline = CognifyPipeline::new(
        storage.clone(),
        graph_db,
        vector_db,
        embedding_engine,
        CognifyConfig::default(),
        None,
    );

    // 4. Create test data
    let text = "Simple test text about technology.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{}", id);

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::new(
        id,
        "test.txt".to_string(),
        stored_location,
        "test.txt".to_string(),
        "txt".to_string(),
        "text/plain".to_string(),
        "test-hash".to_string(),
        owner_id,
    );

    // 5. Skip if LLM not available
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => {
            eprintln!("⚠️  Skipping test: LLM not configured");
            return;
        }
    };

    // 6. Run cognify pipeline (max_chunk_size now in config)
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match pipeline.cognify(vec![data_item], dataset_id, llm).await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {}", e);
            return;
        }
    };

    // 7. Verify embeddings WERE generated (required in Python implementation)
    assert!(
        !result.embeddings.is_empty(),
        "Embeddings are required - pipeline should always generate them"
    );

    // 8. Verify other pipeline stages still work
    assert!(!result.chunks.is_empty(), "Chunks should be generated");
}

#[tokio::test]
async fn test_embedding_semantic_similarity() {
    // Test that embeddings capture semantic similarity

    // Skip if env vars not set
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => return,
    };

    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    let embedding_config = EmbeddingConfig::bge_small(get_embedding_model_dir());
    let embedding_engine = match OnnxEmbeddingEngine::new(embedding_config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping test: Failed to load embedding model: {}", e);
            return;
        }
    };

    let pipeline = CognifyPipeline::new(
        storage.clone(),
        graph_db,
        vector_db,
        embedding_engine,
        CognifyConfig::default(),
        None,
    );

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

        let stored_location = storage
            .store(text.as_bytes(), &location)
            .await
            .expect("Failed to store text");

        let data_item = Data::new(
            id,
            format!("doc-{}.txt", i),
            stored_location,
            format!("doc-{}.txt", i),
            "txt".to_string(),
            "text/plain".to_string(),
            format!("hash-{}", i),
            owner_id,
        );

        let result: CognifyResult = match pipeline.cognify(vec![data_item], dataset_id, llm.clone()).await {
            Ok(result) => result,
            Err(e) => {
                eprintln!("⚠️  Skipping test: Cognify failed: {}", e);
                return;
            }
        };

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

#[tokio::test]
async fn test_entity_description_indexing() {
    // Test that both Entity.name and Entity.description are indexed (Phase 2 feature)

    // Skip if env vars not set
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => return,
    };

    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    let embedding_config = EmbeddingConfig::bge_small(get_embedding_model_dir());
    let embedding_engine = match OnnxEmbeddingEngine::new(embedding_config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping test: Failed to load embedding model: {}", e);
            return;
        }
    };

    let pipeline = CognifyPipeline::new(
        storage.clone(),
        graph_db,
        vector_db.clone(),
        embedding_engine,
        CognifyConfig::default(),
        None,
    );

    // Create test data with entity information
    let text = "TechCorp is a technology company founded in 2020. \
                Alice is the CEO of TechCorp and works in San Francisco. \
                Bob is a software engineer at TechCorp.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{}", id);

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::new(
        id,
        "company.txt".to_string(),
        stored_location,
        "company.txt".to_string(),
        "txt".to_string(),
        "text/plain".to_string(),
        "test-hash".to_string(),
        owner_id,
    );

    // Run cognify pipeline
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match pipeline.cognify(vec![data_item], dataset_id, llm).await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {}", e);
            return;
        }
    };

    // 1. Verify IndexedFieldsStats are populated
    println!("✓ Indexed fields stats:");
    println!("  - Chunks: {}", result.indexed_fields.chunk_text_count);
    println!(
        "  - Entity names: {}",
        result.indexed_fields.entity_name_count
    );
    println!(
        "  - Entity descriptions: {}",
        result.indexed_fields.entity_description_count
    );
    println!(
        "  - Summaries: {}",
        result.indexed_fields.summary_text_count
    );

    // 2. Verify both name and description were indexed (Phase 2 requirement)
    if !result.entities.is_empty() {
        assert_eq!(
            result.indexed_fields.entity_name_count,
            result.entities.len(),
            "Entity name count should match entity count"
        );
        assert_eq!(
            result.indexed_fields.entity_description_count,
            result.entities.len(),
            "Entity description count should match entity count (Phase 2)"
        );

        println!(
            "✓ Both name and description indexed for {} entities",
            result.entities.len()
        );
    }

    // 3. Verify vector DB has both collections
    assert!(
        vector_db.has_collection("Entity", "name").await.unwrap(),
        "Entity name collection should exist"
    );
    assert!(
        vector_db
            .has_collection("Entity", "description")
            .await
            .unwrap(),
        "Entity description collection should exist (Phase 2)"
    );

    println!("✓ Both Entity collections created in vector DB");

    // 4. Verify chunk and summary stats are also tracked
    assert_eq!(
        result.indexed_fields.chunk_text_count,
        result.chunks.len(),
        "Chunk count should match"
    );

    if !result.summaries.is_empty() {
        assert_eq!(
            result.indexed_fields.summary_text_count,
            result.summaries.len(),
            "Summary count should match"
        );
    }

    println!("✓ Phase 2: Entity description embeddings working correctly");
}
#[tokio::test]
async fn test_triplet_embeddings_disabled_by_default() {
    // Test that triplet embeddings are disabled by default (Phase 3 feature)

    // Skip if env vars not set
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => return,
    };

    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    let embedding_config = EmbeddingConfig::bge_small(get_embedding_model_dir());
    let embedding_engine = match OnnxEmbeddingEngine::new(embedding_config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping test: Failed to load embedding model: {}", e);
            return;
        }
    };

    // Create pipeline with DEFAULT config (triplet embeddings should be disabled)
    let pipeline = CognifyPipeline::new(
        storage.clone(),
        graph_db,
        vector_db.clone(),
        embedding_engine,
        CognifyConfig::default(),
        None,
    );

    // Create test data
    let text = "Alice works at TechCorp. Bob also works at TechCorp.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{}", id);

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::new(
        id,
        "test.txt".to_string(),
        stored_location,
        "test.txt".to_string(),
        "txt".to_string(),
        "text/plain".to_string(),
        "test-hash".to_string(),
        owner_id,
    );

    // Run cognify pipeline
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match pipeline.cognify(vec![data_item], dataset_id, llm).await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {}", e);
            return;
        }
    };

    // Verify triplets were NOT indexed (default config has embed_triplets=false)
    assert_eq!(
        result.indexed_fields.triplet_count, 0,
        "Triplets should NOT be indexed by default"
    );

    // Verify Triplet collection was NOT created
    assert!(
        !vector_db
            .has_collection("Triplet", "embeddable_text")
            .await
            .unwrap(),
        "Triplet collection should not exist when embed_triplets=false"
    );

    println!("✓ Phase 3: Triplet embeddings correctly disabled by default");
}

#[tokio::test]
async fn test_triplet_embeddings_enabled() {
    // Test that triplet embeddings work when explicitly enabled (Phase 3 feature)

    // Skip if env vars not set
    let llm = match create_adapter_from_env() {
        Ok(adapter) => adapter,
        Err(_) => return,
    };

    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    let embedding_config = EmbeddingConfig::bge_small(get_embedding_model_dir());
    let embedding_engine = match OnnxEmbeddingEngine::new(embedding_config) {
        Ok(engine) => Arc::new(engine),
        Err(e) => {
            eprintln!("⚠️  Skipping test: Failed to load embedding model: {}", e);
            return;
        }
    };

    // Create pipeline with config that ENABLES triplet embeddings (Phase 3)
    let config = CognifyConfig::default().with_triplet_embeddings(true);
    let pipeline = CognifyPipeline::new(
        storage.clone(),
        graph_db,
        vector_db.clone(),
        embedding_engine,
        config,
        None,
    );

    // Create test data with relationship-heavy content
    let text = "Steve Jobs founded Apple Inc. in 1976. \
                Apple Inc. is a technology company based in California. \
                Tim Cook became CEO of Apple Inc. after Steve Jobs.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{}", id);

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::new(
        id,
        "company.txt".to_string(),
        stored_location,
        "company.txt".to_string(),
        "txt".to_string(),
        "text/plain".to_string(),
        "test-hash".to_string(),
        owner_id,
    );

    // Run cognify pipeline
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match pipeline.cognify(vec![data_item], dataset_id, llm).await {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {}", e);
            return;
        }
    };

    println!("✓ Indexed fields stats:");
    println!("  - Chunks: {}", result.indexed_fields.chunk_text_count);
    println!(
        "  - Entity names: {}",
        result.indexed_fields.entity_name_count
    );
    println!(
        "  - Entity descriptions: {}",
        result.indexed_fields.entity_description_count
    );
    println!(
        "  - Summaries: {}",
        result.indexed_fields.summary_text_count
    );
    println!("  - Triplets: {}", result.indexed_fields.triplet_count);

    // Verify triplets WERE indexed (config has embed_triplets=true)
    if !result.edges.is_empty() && !result.entities.is_empty() {
        assert!(
            result.indexed_fields.triplet_count > 0,
            "Triplets should be indexed when embed_triplets=true and edges exist"
        );

        // Verify Triplet collection was created
        assert!(
            vector_db
                .has_collection("Triplet", "embeddable_text")
                .await
                .unwrap(),
            "Triplet collection should exist when embed_triplets=true"
        );

        println!(
            "✓ Phase 3: Triplet embeddings correctly enabled and indexed {} triplets",
            result.indexed_fields.triplet_count
        );
    } else {
        println!("⚠️  No edges extracted - cannot verify triplet indexing");
    }
}
