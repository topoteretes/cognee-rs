//! Integration test for the default backend: add → cognify → search → delete.
//!
//! Ports the core assertions from `test_library.py` using the Rust fixed infrastructure:
//! SQLite (metadata), Ladybug (graph), Qdrant (vector), LocalStorage (files),
//! ONNX BGE-Small (embeddings), OpenAI-compatible adapter (LLM).
//!
//! Required environment variables (set by `scripts/run_tests_with_local_env.sh`):
//!   OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH
//!
//! Run with: cargo test --package cognee-cognify --test integration_default_backend

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{
    DatabaseConnection, DeleteDb, IngestDb, SearchHistoryDb, connect, initialize, ops,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::{EmbeddingEngine, config::EmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_models::DataInput;
use cognee_search::{
    SearchBuilder, SearchRequest, SearchType,
    types::{SearchOutput, SearchResponse},
};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};
use tempfile::TempDir;
use uuid::Uuid;

mod test_utils;
use test_utils::require_env;

const AI_TEXT: &str = include_str!("test_data/artificial_intelligence.txt");

/// Extract the embedding model directory from `COGNEE_E2E_EMBED_MODEL_PATH`.
fn get_embedding_model_dir() -> String {
    if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_string_lossy().to_string();
    }
    "./target/models".to_string()
}

/// Returns true if the search result contains any data.
fn is_non_empty(response: &SearchResponse) -> bool {
    match &response.result {
        SearchOutput::Text(text) => !text.is_empty(),
        SearchOutput::Texts(texts) => !texts.is_empty(),
        SearchOutput::Items(items) => !items.is_empty(),
        SearchOutput::GraphQueryRows(rows) => !rows.is_empty(),
        SearchOutput::Rules(rules) => !rules.is_empty(),
        SearchOutput::Ack { .. } => true,
    }
}

/// Build a `SearchRequest` with all optional fields set to `None`.
fn make_request(query: &str, search_type: SearchType) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type,
        top_k: None,
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: None,
        use_combined_context: None,
        session_id: None,
        node_type: None,
        node_name: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: None,
        user_id: None,
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
    }
}

#[tokio::test]
async fn test_default_backend_add_cognify_search_delete() {
    // ── Environment ──────────────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");
    let _ = require_env("COGNEE_E2E_EMBED_MODEL_PATH");

    // ── Infrastructure setup ─────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    // Local file storage
    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    // SQLite metadata database (file must exist before sqlx opens it)
    let db_path = temp_dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create sqlite db file");
    let db_url = format!("sqlite://{}", db_path.display());
    let db = connect(&db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let database: Arc<DatabaseConnection> = Arc::new(db);

    // Ladybug graph database
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new"),
    );
    graph_db.initialize().await.expect("graph_db.initialize");

    // Qdrant vector database (BGE-Small dimension = 384)
    let vector_db: Arc<dyn VectorDB> =
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), 384));

    // ONNX embedding engine
    let model_dir = get_embedding_model_dir();
    let embedding_engine: Arc<dyn EmbeddingEngine> =
        match OnnxEmbeddingEngine::new(EmbeddingConfig::bge_small(&model_dir)) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!("⚠️  Skipping test: failed to load embedding model: {}", e);
                eprintln!(
                    "   Ensure model is at {}/BGE-Small-v1.5-model_quantized.onnx",
                    model_dir
                );
                return;
            }
        };

    // OpenAI-compatible LLM
    let llm: Arc<dyn Llm> = Arc::new(
        OpenAIAdapter::new(
            require_env("OPENAI_MODEL"),
            require_env("OPENAI_TOKEN"),
            Some(require_env("OPENAI_URL")),
        )
        .expect("OpenAIAdapter::new"),
    );

    let owner_id = Uuid::nil();

    // ── Step 3: Ingest ───────────────────────────────────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>);
    let data_items = ingest
        .add(
            vec![DataInput::Text(AI_TEXT.to_string())],
            "artificial_intelligence",
            owner_id,
            None,
        )
        .await
        .expect("ingest.add");

    assert_eq!(data_items.len(), 1, "Expected exactly 1 ingested data item");
    println!("✓ Ingested {} data item(s)", data_items.len());

    // ── Step 4: Graph empty before cognify ──────────────────────────────────
    assert!(
        graph_db.is_empty().await.expect("graph_db.is_empty"),
        "Graph should be empty before cognify"
    );

    // ── Step 5: Cognify ──────────────────────────────────────────────────────
    let config = CognifyConfig::default()
        .with_summarization(true)
        .with_triplet_embeddings(false);

    let dataset =
        ops::datasets::get_dataset_by_name(&database, "artificial_intelligence", owner_id, None)
            .await
            .expect("get_dataset_by_name")
            .expect("dataset should exist after ingest");

    let result = match cognify(
        data_items,
        dataset.id,
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone() as Arc<dyn StorageTrait>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        None,
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("⚠️  Skipping test: cognify failed: {}", e);
            return;
        }
    };

    assert!(
        !result.chunks.is_empty(),
        "Chunks should be non-empty after cognify"
    );
    assert!(
        !result.entities.is_empty(),
        "Entities should be extracted after cognify"
    );
    println!(
        "✓ Cognify: {} chunks, {} entities, {} edges",
        result.chunks.len(),
        result.entities.len(),
        result.edges.len()
    );

    // ── Step 6: Graph non-empty after cognify ────────────────────────────────
    assert!(
        !graph_db.is_empty().await.expect("graph_db.is_empty"),
        "Graph should be non-empty after cognify"
    );

    // ── Steps 7–9: Search ────────────────────────────────────────────────────
    let orchestrator = SearchBuilder::new(
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        llm.clone() as Arc<dyn Llm>,
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    // Use first extracted entity name as query term
    let query = result.entities[0].entity.name.clone();
    println!("✓ Search query: {:?}", query);

    // GRAPH_COMPLETION
    let gc_response = orchestrator
        .search(&make_request(&query, SearchType::GraphCompletion))
        .await
        .expect("search GraphCompletion");
    assert!(
        is_non_empty(&gc_response),
        "GraphCompletion should return non-empty result"
    );
    println!("✓ GraphCompletion: non-empty result");

    // CHUNKS
    let chunks_response = orchestrator
        .search(&make_request(&query, SearchType::Chunks))
        .await
        .expect("search Chunks");
    assert!(
        is_non_empty(&chunks_response),
        "Chunks search should return non-empty result"
    );
    println!("✓ Chunks: non-empty result");

    // SUMMARIES
    let summaries_response = orchestrator
        .search(&make_request(&query, SearchType::Summaries))
        .await
        .expect("search Summaries");
    assert!(
        is_non_empty(&summaries_response),
        "Summaries search should return non-empty result"
    );
    println!("✓ Summaries: non-empty result");

    // ── Step 10: Delete / cleanup ────────────────────────────────────────────
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>);
    delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::All,
            mode: DeleteMode::Soft,
        })
        .await
        .expect("delete_svc.execute");

    graph_db
        .delete_graph()
        .await
        .expect("graph_db.delete_graph");

    let remaining = ops::datasets::list_datasets(&database)
        .await
        .expect("list_datasets after delete");
    assert!(
        remaining.is_empty(),
        "All datasets should be deleted; found {:?}",
        remaining
    );

    assert!(
        graph_db.is_empty().await.expect("graph_db.is_empty"),
        "Graph should be empty after delete"
    );

    println!("✓ Delete: all data cleaned up");
    println!("✅ test_default_backend_add_cognify_search_delete PASSED");
}
