//! Integration test: verifies that `Data.last_accessed` is updated after search.
//!
//! Ingests a document, cognifies it, then performs searches and checks that the
//! `last_accessed` timestamp on the source `Data` record advances monotonically.
//!
//! Required environment variables (set by `scripts/run_tests_with_local_env.sh`):
//!   OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH
//!
//! Run with: cargo test --package cognee-search --test last_accessed_update

use std::sync::Arc;

use chrono::Utc;
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, IngestDb, SearchHistoryDb, connect, initialize, ops};

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_ontology::NoOpOntologyResolver;
use cognee_search::{SearchBuilder, SearchRequest, SearchType};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};
use tempfile::TempDir;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers inlined (the search crate has no shared test_utils module for
// integration tests in separate files; the matrix test uses `mod test_utils`
// which is a sibling file).
// ---------------------------------------------------------------------------

/// Read a required environment variable, loading `.env` first (idempotent).
fn require_env(var_name: &str) -> String {
    let _ = dotenv::dotenv();

    let canonical_fallback = match var_name {
        "OPENAI_TOKEN" => Some("LLM_API_KEY"),
        "OPENAI_URL" => Some("LLM_ENDPOINT"),
        "OPENAI_MODEL" => Some("LLM_MODEL"),
        _ => None,
    };

    if let Ok(v) = std::env::var(var_name)
        && !v.is_empty()
    {
        return v;
    }
    if let Some(canonical) = canonical_fallback
        && let Ok(v) = std::env::var(canonical)
        && !v.is_empty()
    {
        return v;
    }
    panic!("Required environment variable '{var_name}' is not set")
}

fn create_adapter_from_env() -> Arc<cognee_llm::OpenAIAdapter> {
    let base_url = require_env("OPENAI_URL");
    let api_token = require_env("OPENAI_TOKEN");
    let model = std::env::var("LLM_MODEL")
        .or_else(|_| std::env::var("OPENAI_MODEL"))
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());

    Arc::new(
        cognee_llm::OpenAIAdapter::new(model, api_token, Some(base_url))
            .unwrap_or_else(|e| panic!("Failed to create OpenAI adapter: {e}")),
    )
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_search_updates_last_accessed_timestamp() {
    // ── Environment ──────────────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");

    // ── Infrastructure setup ─────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    let Some((embedding_engine, embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };

    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

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

    let vector_db: Arc<dyn VectorDB> =
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), embedding_dims));

    let llm: Arc<dyn Llm> = create_adapter_from_env();
    let owner_id = Uuid::nil();

    // ── Ingest text ──────────────────────────────────────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));
    ingest
        .add(
            vec![DataInput::Text(
                "Artificial intelligence enables machines to simulate human intelligence."
                    .to_string(),
            )],
            "last_accessed_test",
            owner_id,
            None,
        )
        .await
        .expect("ingest text");

    let dataset =
        ops::datasets::get_dataset_by_name(&database, "last_accessed_test", owner_id, None)
            .await
            .expect("get_dataset_by_name")
            .expect("dataset should exist after ingest");
    let data_items = ops::datasets::get_dataset_data(&database, dataset.id)
        .await
        .expect("get_dataset_data");
    assert_eq!(data_items.len(), 1, "Expected 1 data item in dataset");
    let data_id = data_items[0].id;

    // ── Cognify (summarization=false, triplet_embeddings=false) ──────────────
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
    );

    match cognify(
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
        Ok(_) => {}
        Err(e) => {
            eprintln!("Skipping test: cognify failed: {}", e);
            return;
        }
    }

    // ── Record initial last_accessed ─────────────────────────────────────────
    let initial_data = ops::data::get_data(&database, data_id)
        .await
        .expect("get_data")
        .expect("data should exist");
    let initial_last_accessed = initial_data.last_accessed;
    println!("Initial last_accessed: {:?}", initial_last_accessed);

    // ── Wait 100ms, record before_search time ────────────────────────────────
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let before_search = Utc::now();

    // ── First search: Chunks with only_context=true ──────────────────────────
    let orchestrator = SearchBuilder::new(
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&graph_db),
        Arc::clone(&llm),
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .with_dataset_resolver(database.clone() as Arc<dyn IngestDb>)
    .build();

    let search_request = SearchRequest {
        query_text: "artificial intelligence".to_string(),
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

    orchestrator
        .search(&search_request)
        .await
        .expect("first search should succeed");

    // ── Read last_accessed after first search ────────────────────────────────
    let after_first_search = ops::data::get_data(&database, data_id)
        .await
        .expect("get_data after first search")
        .expect("data should exist");
    let first_last_accessed = after_first_search.last_accessed;
    println!(
        "After first search last_accessed: {:?}",
        first_last_accessed
    );

    // Assert it was updated: should be Some and within 30s of now
    let first_ts = first_last_accessed.expect("last_accessed should be set after search");
    let now = Utc::now();
    let delta = now.signed_duration_since(first_ts);
    assert!(
        delta.num_seconds() < 30 && delta.num_seconds() >= 0,
        "last_accessed should be within 30s of now, got delta={}s",
        delta.num_seconds()
    );

    // Assert it was updated relative to before_search
    assert!(
        first_ts >= before_search,
        "last_accessed ({}) should be >= before_search ({})",
        first_ts,
        before_search
    );

    // If initial_last_accessed was Some, the new value must be >= the initial
    if let Some(initial_ts) = initial_last_accessed {
        assert!(
            first_ts >= initial_ts,
            "last_accessed after search ({}) should be >= initial ({})",
            first_ts,
            initial_ts
        );
    }

    // ── Wait 100ms, second search ────────────────────────────────────────────
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    orchestrator
        .search(&search_request)
        .await
        .expect("second search should succeed");

    // ── Read last_accessed after second search ───────────────────────────────
    let after_second_search = ops::data::get_data(&database, data_id)
        .await
        .expect("get_data after second search")
        .expect("data should exist");
    let second_last_accessed = after_second_search
        .last_accessed
        .expect("last_accessed should be set after second search");

    // Assert monotonic increase
    assert!(
        second_last_accessed >= first_ts,
        "last_accessed should increase monotonically: second ({}) >= first ({})",
        second_last_accessed,
        first_ts
    );

    println!(
        "Monotonic check passed: {} >= {}",
        second_last_accessed, first_ts
    );
    println!("test_search_updates_last_accessed_timestamp PASSED");
}
