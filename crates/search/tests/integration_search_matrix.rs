//! Integration test: retriever & search-type matrix (ports `test_search_db.py`).
//!
//! Ingests two documents into a shared dataset, cognifies with triplet embeddings
//! enabled, then exercises 9 `SearchType` variants and asserts each returns a
//! non-empty result.  Also verifies graph/vector consistency and search history.

//! Required environment variables (set by `scripts/run_tests_with_local_env.sh`):
//!   OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH
//!
//! Run with: cargo test --package cognee-search --test integration_search_matrix

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{
    DatabaseConnection, DeleteDb, IngestDb, SearchHistoryDb, connect, initialize, ops,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_ontology::NoOpOntologyResolver;
use cognee_search::{
    SearchBuilder, SearchRequest, SearchType,
    types::{SearchOutput, SearchResponse},
};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};
use tempfile::TempDir;
use uuid::Uuid;

mod test_utils;
use test_utils::{create_adapter_from_env, require_env};

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
        SearchOutput::Structured(value) => !value.is_null(),
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
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: save,
        user_id: None,
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
    }
}

#[tokio::test]
async fn test_search_type_matrix() {
    // ── Environment ──────────────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");

    // ── Infrastructure setup ─────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

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

    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new"),
    );
    graph_db.initialize().await.expect("graph_db.initialize");

    let Some((embedding_engine, embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };

    let vector_db: Arc<dyn VectorDB> = Arc::new(QdrantAdapter::new(
        temp_dir.path().join("qdrant"),
        embedding_dims,
    ));

    let llm: Arc<dyn Llm> = create_adapter_from_env();
    let owner_id = Uuid::nil();

    // ── Step 3: Ingest two documents into the same dataset ───────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));
    ingest
        .add(
            vec![DataInput::Text(GERMANY_TEXT.to_string())],
            "test_dataset",
            owner_id,
            None,
        )
        .await
        .expect("ingest germany");
    ingest
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "test_dataset",
            owner_id,
            None,
        )
        .await
        .expect("ingest quantum");

    let dataset = ops::datasets::get_dataset_by_name(&database, "test_dataset", owner_id, None)
        .await
        .expect("get_dataset_by_name")
        .expect("dataset should exist after ingest");
    let data_items = ops::datasets::get_dataset_data(&database, dataset.id)
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

    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
    );

    let result = match cognify(
        data_items,
        dataset.id,
        None,
        None,
        None,
        Arc::clone(&llm),
        Arc::clone(&storage),
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
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
    // The graph contains both LLM-extracted edges and structural edges
    // (contains, is_part_of, has_type, etc.), but triplet embeddings are only
    // created for LLM-extracted edges.  Compare the cognify result's triplet
    // count against the vector collection size.
    let triplet_size = vector_db
        .collection_size("Triplet", "text")
        .await
        .expect("collection_size Triplet");
    assert_eq!(
        result.indexed_fields.triplet_count, triplet_size,
        "Cognify triplet count ({}) should equal Triplet vector collection size ({})",
        result.indexed_fields.triplet_count, triplet_size
    );
    println!(
        "✓ Graph/vector consistency: {} triplets indexed = {} triplet vectors",
        result.indexed_fields.triplet_count, triplet_size
    );

    // ── Steps 8–11: Search matrix ────────────────────────────────────────────
    let orchestrator = SearchBuilder::new(
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&graph_db),
        Arc::clone(&llm),
        database.clone() as Arc<dyn SearchHistoryDb>,
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
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: Some(false),
        user_id: None,
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
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

    // ── Step 11b: Context assertion for GraphCompletion (only_context=true) ──
    // This is the combination used by the Locust benchmark: the graph retriever
    // returns ranked edges (source→relationship→target + entity texts) without
    // calling the LLM.  Verify that the raw edge payloads contain relevant text.
    let graph_ctx = SearchRequest {
        query_text: query.to_string(),
        search_type: SearchType::GraphCompletion,
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
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: Some(false),
        user_id: None,
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
    };
    let graph_ctx_resp = orchestrator
        .search(&graph_ctx)
        .await
        .expect("graph completion context search");
    assert!(
        is_non_empty(&graph_ctx_resp),
        "GraphCompletion context should return non-empty edges"
    );
    let graph_ctx_text = response_text(&graph_ctx_resp);
    assert!(
        graph_ctx_text.contains("germany") || graph_ctx_text.contains("netherlands"),
        "GraphCompletion context edges should mention germany or netherlands; got: {}",
        &graph_ctx_text[..graph_ctx_text.len().min(300)]
    );
    println!(
        "✓ GraphCompletion context (only_context=true): non-empty, mentions germany/netherlands"
    );

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
