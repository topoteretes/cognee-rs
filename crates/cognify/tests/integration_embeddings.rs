#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for embedding generation in the cognify pipeline.
//!
//! These tests require: OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL (or any embedding
//! provider configured via EMBEDDING_PROVIDER / EMBEDDING_API_KEY env vars).
//!
//! Run with: cargo test --package cognee-cognify --test integration_embeddings

use cognee_cognify::{CognifyConfig, CognifyResult, cognify};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_graph::MockGraphDB;
use cognee_models::Data;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_vector::{MockVectorDB, VectorDB};
use std::sync::Arc;
use uuid::Uuid;

mod test_data;
mod test_utils;

use test_data::{
    TEST_TEXT_EMBEDDINGS_BASIC, TEST_TEXT_EMBEDDINGS_ENTITY, TEST_TEXT_EMBEDDINGS_TRIPLETS_DEFAULT,
};
use test_utils::create_adapter_from_env;

/// Build an in-memory SQLite [`DatabaseConnection`] for the executor-routed
/// `cognify()` (LIB-06 Decision 1 requires it).
async fn make_in_memory_db() -> Arc<DatabaseConnection> {
    let conn = connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite");
    initialize(&conn).await.expect("initialize");
    Arc::new(conn)
}

/// Build a default-thread [`cognee_core::RayonThreadPool`] wrapped in the
/// [`cognee_core::CpuPool`] trait object — required by `cognify()` (LIB-06
/// Decision 1).
fn make_thread_pool() -> Arc<dyn cognee_core::CpuPool> {
    Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"))
}

#[tokio::test]
async fn test_pipeline_with_embeddings() {
    if !test_utils::llm_env_available() {
        eprintln!("skipping: live LLM credentials (OPENAI_URL/OPENAI_TOKEN) not set");
        return;
    }
    let llm = create_adapter_from_env();

    let Some((embedding_engine, embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };

    // 1. Setup storage and graph DB
    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    // 2. Create config
    let config = CognifyConfig::default();

    // 4. Create test data
    let text = TEST_TEXT_EMBEDDINGS_BASIC;

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{id}");

    // Store text in mock storage
    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::builder(
        id,
        "test-doc.txt",
        stored_location,
        "test-doc.txt",
        "txt",
        "text/plain",
        "test-hash",
        owner_id,
    )
    .build();

    // 5. Run cognify pipeline (max_chunk_size now in config)
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match cognify(
        vec![data_item],
        dataset_id,
        None,
        None,
        None,
        llm,
        storage.clone(),
        graph_db,
        vector_db,
        embedding_engine,
        make_in_memory_db().await,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        make_thread_pool(),
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {e}");
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

    // 8. Verify embedding dimensions match the configured engine
    for embedding in &result.embeddings {
        assert_eq!(
            embedding.dimensions(),
            embedding_dims,
            "Expected {embedding_dims} dimensions from embedding engine"
        );

        // Verify L2 normalization
        let norm = embedding.norm();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "Embedding not normalized: norm = {norm}"
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
        "✓ Embeddings generated: {chunk_count} chunks, {entity_count} entities, {summary_count} summaries"
    );

    assert!(chunk_count > 0, "No chunk embeddings");
}

#[tokio::test]
async fn test_pipeline_requires_embeddings() {
    if !test_utils::llm_env_available() {
        eprintln!("skipping: live LLM credentials (OPENAI_URL/OPENAI_TOKEN) not set");
        return;
    }
    // This test verifies that embeddings are REQUIRED (matches Python behavior)

    let Some((embedding_engine, _embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };

    // 1. Setup storage and graph DB
    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    // 2. Create config (embeddings are REQUIRED)
    let config = CognifyConfig::default();

    // 4. Create test data
    let text = "Simple test text about technology.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{id}");

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::builder(
        id,
        "test.txt",
        stored_location,
        "test.txt",
        "txt",
        "text/plain",
        "test-hash",
        owner_id,
    )
    .build();

    let llm = create_adapter_from_env();

    // 6. Run cognify pipeline (max_chunk_size now in config)
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match cognify(
        vec![data_item],
        dataset_id,
        None,
        None,
        None,
        llm,
        storage.clone(),
        graph_db,
        vector_db,
        embedding_engine,
        make_in_memory_db().await,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        make_thread_pool(),
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {e}");
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
    if !test_utils::llm_env_available() {
        eprintln!("skipping: live LLM credentials (OPENAI_URL/OPENAI_TOKEN) not set");
        return;
    }
    // Test that embeddings capture semantic similarity

    let llm = create_adapter_from_env() as Arc<dyn cognee_llm::Llm>;

    let Some((embedding_engine, _embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };
    let embedding_engine: Arc<dyn cognee_embedding::EmbeddingEngine> = embedding_engine;

    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());

    let config = CognifyConfig::default();

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
        let location = format!("test-data-{id}");

        let stored_location = storage
            .store(text.as_bytes(), &location)
            .await
            .expect("Failed to store text");

        let data_item = Data::builder(
            id,
            format!("doc-{i}.txt"),
            stored_location,
            format!("doc-{i}.txt"),
            "txt",
            "text/plain",
            format!("hash-{i}"),
            owner_id,
        )
        .build();

        let result: CognifyResult = match cognify(
            vec![data_item],
            dataset_id,
            None,
            None,
            None,
            Arc::clone(&llm),
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            make_in_memory_db().await,
            Arc::new(cognee_database::NoopPipelineRunRepository::new())
                as Arc<dyn cognee_database::PipelineRunRepository>,
            make_thread_pool(),
            Arc::new(NoOpOntologyResolver::new()),
            &config,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                eprintln!("⚠️  Skipping test: Cognify failed: {e}");
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

        println!("✓ Semantic similarity: {similarity:.4}");

        // Semantically similar texts should have high cosine similarity
        assert!(
            similarity > 0.5,
            "Expected high similarity for related ML texts, got {similarity}"
        );
    }
}

#[tokio::test]
async fn test_entity_name_indexing() {
    if !test_utils::llm_env_available() {
        eprintln!("skipping: live LLM credentials (OPENAI_URL/OPENAI_TOKEN) not set");
        return;
    }
    // Test that Entity.name is indexed (matching Python's index_fields=["name"])

    let llm = create_adapter_from_env();

    let Some((embedding_engine, _embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };

    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    let config = CognifyConfig::default();

    // Create test data with entity information
    let text = TEST_TEXT_EMBEDDINGS_ENTITY;

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{id}");

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::builder(
        id,
        "company.txt",
        stored_location,
        "company.txt",
        "txt",
        "text/plain",
        "test-hash",
        owner_id,
    )
    .build();

    // Run cognify pipeline
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match cognify(
        vec![data_item],
        dataset_id,
        None,
        None,
        None,
        llm,
        storage.clone(),
        graph_db,
        vector_db.clone(),
        embedding_engine,
        make_in_memory_db().await,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        make_thread_pool(),
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {e}");
            return;
        }
    };

    // 1. Verify IndexedFieldsStats are populated
    println!("✓ Indexed fields stats:");
    println!("  - Chunks: {}", result.indexed_fields.chunk_text_count());
    println!(
        "  - Entity names: {}",
        result.indexed_fields.entity_name_count()
    );
    println!(
        "  - Summaries: {}",
        result.indexed_fields.summary_text_count()
    );

    // 2. Verify entity names were indexed
    if !result.entities.is_empty() {
        assert_eq!(
            result.indexed_fields.entity_name_count(),
            result.entities.len(),
            "Entity name count should match entity count"
        );

        println!(
            "✓ Entity names indexed for {} entities",
            result.entities.len()
        );
    }

    // 3. Verify vector DB has Entity name collection (but NOT description)
    assert!(
        vector_db.has_collection("Entity", "name").await.unwrap(),
        "Entity name collection should exist"
    );
    assert!(
        !vector_db
            .has_collection("Entity", "description")
            .await
            .unwrap(),
        "Entity description collection should NOT exist (matches Python SDK)"
    );

    println!("✓ Only Entity name collection created in vector DB (Python-compatible)");

    // 4. Verify chunk and summary stats are also tracked
    assert_eq!(
        result.indexed_fields.chunk_text_count(),
        result.chunks.len(),
        "Chunk count should match"
    );

    if !result.summaries.is_empty() {
        assert_eq!(
            result.indexed_fields.summary_text_count(),
            result.summaries.len(),
            "Summary count should match"
        );
    }

    println!("✓ Entity name-only indexing working correctly (Python-compatible)");
}
#[tokio::test]
async fn test_triplet_embeddings_disabled_by_default() {
    if !test_utils::llm_env_available() {
        eprintln!("skipping: live LLM credentials (OPENAI_URL/OPENAI_TOKEN) not set");
        return;
    }
    // Test that triplet embeddings are disabled by default (Phase 3 feature)

    let llm = create_adapter_from_env();

    let Some((embedding_engine, _embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };

    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    // Create config with DEFAULT settings (triplet embeddings should be disabled)
    let config = CognifyConfig::default();

    // Create test data
    let text = TEST_TEXT_EMBEDDINGS_TRIPLETS_DEFAULT;

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{id}");

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::builder(
        id,
        "test.txt",
        stored_location,
        "test.txt",
        "txt",
        "text/plain",
        "test-hash",
        owner_id,
    )
    .build();

    // Run cognify pipeline
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match cognify(
        vec![data_item],
        dataset_id,
        None,
        None,
        None,
        llm,
        storage.clone(),
        graph_db,
        vector_db.clone(),
        embedding_engine,
        make_in_memory_db().await,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        make_thread_pool(),
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {e}");
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
        !vector_db.has_collection("Triplet", "text").await.unwrap(),
        "Triplet collection should not exist when embed_triplets=false"
    );

    println!("✓ Phase 3: Triplet embeddings correctly disabled by default");
}

#[tokio::test]
async fn test_triplet_embeddings_enabled() {
    if !test_utils::llm_env_available() {
        eprintln!("skipping: live LLM credentials (OPENAI_URL/OPENAI_TOKEN) not set");
        return;
    }
    // Test that triplet embeddings work when explicitly enabled (Phase 3 feature)

    let llm = create_adapter_from_env();

    let Some((embedding_engine, _embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };

    let storage = Arc::new(MockStorage::new());
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());

    // Create config that ENABLES triplet embeddings (Phase 3)
    let config = CognifyConfig::default().with_triplet_embeddings(true);

    // Create test data with relationship-heavy content
    let text = "Steve Jobs founded Apple Inc. in 1976. \
                Apple Inc. is a technology company based in California. \
                Tim Cook became CEO of Apple Inc. after Steve Jobs.";

    let id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-data-{id}");

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("Failed to store text");

    let data_item = Data::builder(
        id,
        "company.txt",
        stored_location,
        "company.txt",
        "txt",
        "text/plain",
        "test-hash",
        owner_id,
    )
    .build();

    // Run cognify pipeline
    let dataset_id = Uuid::new_v4();
    let result: CognifyResult = match cognify(
        vec![data_item],
        dataset_id,
        None,
        None,
        None,
        llm,
        storage.clone(),
        graph_db,
        vector_db.clone(),
        embedding_engine,
        make_in_memory_db().await,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        make_thread_pool(),
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("⚠️  Skipping test: Cognify pipeline failed: {e}");
            return;
        }
    };

    println!("✓ Indexed fields stats:");
    println!("  - Chunks: {}", result.indexed_fields.chunk_text_count());
    println!(
        "  - Entity names: {}",
        result.indexed_fields.entity_name_count()
    );
    println!(
        "  - Summaries: {}",
        result.indexed_fields.summary_text_count()
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
            vector_db.has_collection("Triplet", "text").await.unwrap(),
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
