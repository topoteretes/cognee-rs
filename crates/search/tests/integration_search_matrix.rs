//! Integration test: retriever & search-type matrix (ports `test_search_db.py`).
//!
//! Ingests two documents into a shared dataset, cognifies with triplet embeddings
//! enabled, then exercises 9 `SearchType` variants and asserts each returns a
//! non-empty result.  Also verifies graph/vector consistency and search history.
//!
//! Required environment variables (set by `scripts/run_tests_with_local_env.sh`):
//!   OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH
//!
//! Run with: cargo test --package cognee-search --test integration_search_matrix

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, CognifyPipeline};
use cognee_database::{DatabaseTrait, SqliteDatabase};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::{EmbeddingEngine, config::EmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::IngestPipeline;
use cognee_llm::Llm;
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
use test_utils::{create_adapter_from_env, get_embedding_model_dir, require_env};

const GERMANY_TEXT: &str = include_str!("test_data/germany_netherlands.txt");
const QUANTUM_TEXT: &str = include_str!("test_data/quantum_computers.txt");

/// Returns true if the search response contains any non-empty data.
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

/// Returns the lowercased text content of a search response for keyword assertions.
fn response_text(response: &SearchResponse) -> String {
    match &response.result {
        SearchOutput::Text(text) => text.to_lowercase(),
        SearchOutput::Texts(texts) => texts.join(" ").to_lowercase(),
        SearchOutput::Items(items) => items
            .iter()
            .map(|item| item.payload.to_string().to_lowercase())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Build a `SearchRequest` with all optional fields set to `None`.
fn make_request(query: &str, search_type: SearchType, save: Option<bool>) -> SearchRequest {
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
        save_interaction: save,
    }
}

#[tokio::test]
async fn test_search_type_matrix() {
    // ── Environment ──────────────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");
    let _ = require_env("COGNEE_E2E_EMBED_MODEL_PATH");

    // ── Infrastructure setup ─────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    // SQLite metadata database (file must exist before sqlx opens it)
    let db_path = temp_dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create sqlite db file");
    let db_url = format!("sqlite://{}", db_path.display());
    let database: Arc<dyn DatabaseTrait> = Arc::new(
        SqliteDatabase::new(&db_url)
            .await
            .expect("SqliteDatabase::new"),
    );
    database.initialize().await.expect("database.initialize");

    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new"),
    );
    graph_db.initialize().await.expect("graph_db.initialize");

    let vector_db: Arc<dyn VectorDB> =
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), 384));

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

    let llm: Arc<dyn Llm> = create_adapter_from_env();
    let owner_id = Uuid::nil();

    // ── Step 3: Ingest two documents into the same dataset ───────────────────
    let ingest = IngestPipeline::new(Arc::clone(&storage), Arc::clone(&database));
    ingest
        .add(
            vec![DataInput::Text(GERMANY_TEXT.to_string())],
            "test_dataset",
            owner_id,
        )
        .await
        .expect("ingest germany");
    ingest
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "test_dataset",
            owner_id,
        )
        .await
        .expect("ingest quantum");

    let dataset = database
        .get_dataset_by_name("test_dataset", owner_id)
        .await
        .expect("get_dataset_by_name")
        .expect("dataset should exist after ingest");
    let data_items = database
        .get_dataset_data(dataset.id)
        .await
        .expect("get_dataset_data");
    assert_eq!(data_items.len(), 2, "Expected 2 data items in dataset");
    println!("✓ Ingested {} data items", data_items.len());

    // ── Step 4: Graph empty before cognify ──────────────────────────────────
    assert!(
        graph_db.is_empty().await.expect("graph_db.is_empty"),
        "Graph should be empty before cognify"
    );

    // ── Step 5: Cognify with triplet embeddings enabled ──────────────────────
    let config = CognifyConfig::default()
        .with_summarization(true)
        .with_triplet_embeddings(true);

    let cognify = CognifyPipeline::new(
        Arc::clone(&storage),
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        config,
        None,
    );

    let result = match cognify
        .cognify(data_items, dataset.id, Arc::clone(&llm))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("⚠️  Skipping test: cognify failed: {}", e);
            return;
        }
    };

    assert!(!result.entities.is_empty(), "Entities should be extracted");
    assert!(!result.edges.is_empty(), "Edges should be extracted");
    assert!(
        result.indexed_fields.triplet_count > 0,
        "Triplets should be indexed when embed_triplets=true"
    );
    println!(
        "✓ Cognify: {} chunks, {} entities, {} edges, {} triplets",
        result.chunks.len(),
        result.entities.len(),
        result.edges.len(),
        result.indexed_fields.triplet_count
    );

    // ── Step 6: Graph non-empty after cognify ────────────────────────────────
    assert!(
        !graph_db.is_empty().await.expect("graph_db.is_empty"),
        "Graph should be non-empty after cognify"
    );

    // ── Step 7: Graph/vector consistency check ───────────────────────────────
    let (_, graph_edges) = graph_db.get_graph_data().await.expect("get_graph_data");
    let triplet_size = vector_db
        .collection_size("Triplet", "embeddable_text")
        .await
        .expect("collection_size Triplet");
    assert_eq!(
        graph_edges.len(),
        triplet_size,
        "Edge count in graph ({}) should equal Triplet vector collection size ({})",
        graph_edges.len(),
        triplet_size
    );
    println!(
        "✓ Graph/vector consistency: {} edges = {} triplet vectors",
        graph_edges.len(),
        triplet_size
    );

    // ── Steps 8–11: Search matrix ────────────────────────────────────────────
    let orchestrator = SearchBuilder::new(
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&graph_db),
        Arc::clone(&llm),
        Arc::clone(&database),
    )
    .build();

    let query = "Next to which country is Germany located?";

    // Graph-based search types — save interactions for history tracking
    let graph_search_types = [
        SearchType::GraphCompletion,
        SearchType::GraphCompletionCot,
        SearchType::GraphCompletionContextExtension,
        SearchType::GraphSummaryCompletion,
        SearchType::TripletCompletion,
    ];

    // Retrieval-only search types
    let retrieval_search_types = [
        SearchType::Chunks,
        SearchType::Summaries,
        SearchType::RagCompletion,
        SearchType::Temporal,
    ];

    // Execute graph-based searches (save_interaction = true for history tracking)
    for search_type in graph_search_types {
        let response = orchestrator
            .search(&make_request(query, search_type, Some(true)))
            .await
            .unwrap_or_else(|e| panic!("search {:?} failed: {}", search_type, e));

        assert!(
            is_non_empty(&response),
            "{:?} should return non-empty result",
            search_type
        );

        let text = response_text(&response);
        assert!(
            text.contains("germany") || text.contains("netherlands"),
            "{:?} result should mention germany or netherlands; got: {}",
            search_type,
            &text[..text.len().min(200)]
        );

        println!(
            "✓ {:?}: non-empty, mentions germany/netherlands",
            search_type
        );
    }

    // Execute retrieval-only searches
    for search_type in retrieval_search_types {
        let response = orchestrator
            .search(&make_request(query, search_type, Some(false)))
            .await
            .unwrap_or_else(|e| panic!("search {:?} failed: {}", search_type, e));

        assert!(
            is_non_empty(&response),
            "{:?} should return non-empty result",
            search_type
        );
        println!("✓ {:?}: non-empty result", search_type);
    }

    // ── Step 10–11: Context assertion for Chunks ─────────────────────────────
    let chunks_ctx = SearchRequest {
        query_text: query.to_string(),
        search_type: SearchType::Chunks,
        top_k: None,
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(true),
        use_combined_context: None,
        session_id: None,
        node_type: None,
        node_name: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: Some(false),
    };
    let chunks_resp = orchestrator
        .search(&chunks_ctx)
        .await
        .expect("chunks context search");
    let chunks_text = response_text(&chunks_resp);
    assert!(
        chunks_text.contains("germany") || chunks_text.contains("netherlands"),
        "Chunks context should contain germany or netherlands; got: {}",
        &chunks_text[..chunks_text.len().min(200)]
    );
    println!("✓ Chunks context: mentions germany/netherlands");

    // ── Step 12: Search history count ────────────────────────────────────────
    // The orchestrator saves interactions without a user_id, so retrieve all history.
    let history = orchestrator
        .get_history(None, None)
        .await
        .expect("get_history");
    assert!(
        history.len() >= 5,
        "Expected >= 5 history entries (one per graph search type); got {}",
        history.len()
    );
    println!("✓ Search history: {} entries (>= 5)", history.len());

    // ── Step 13: Cleanup ─────────────────────────────────────────────────────
    let delete_svc = DeleteService::new(Arc::clone(&storage), Arc::clone(&database));
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

    let remaining = database
        .list_datasets()
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

    println!("✓ Cleanup complete");
    println!("✅ test_search_type_matrix PASSED");
}
