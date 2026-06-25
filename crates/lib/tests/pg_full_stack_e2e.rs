#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Full PostgreSQL-stack end-to-end test: relational + PgGraphAdapter + PgVectorAdapter.
//!
//! Validates the ComponentManager wiring added in COG-4457 (Items 1 + 2):
//! - `GRAPH_DATABASE_PROVIDER=postgres` is dispatched to `PgGraphAdapter`.
//! - Graph credentials fall back to `db_*` fields when `GRAPH_DATABASE_*` are
//!   not set (Item 2 credential-fallback parity).
//!
//! Required environment variables (all skipped cleanly when absent):
//!   TEST_POSTGRES_URL  — single Postgres instance for all three stores
//!   OPENAI_URL         — OpenAI-compatible LLM endpoint
//!   OPENAI_TOKEN       — API key (or alias LLM_API_KEY)
//!   COGNEE_E2E_EMBED_MODEL_PATH — path to the BGE-Small-v1.5 ONNX model
//!   COGNEE_E2E_TOKENIZER_PATH  — path to the BGE-Small tokenizer.json
//!
//! Run with:
//!   TEST_POSTGRES_URL="postgres://..." cargo test -p cognee-lib \
//!       --features pggraph,pgvector,postgres \
//!       --test pg_full_stack_e2e -- --nocapture
//!
//! Tests run serially to avoid concurrent Postgres contention.

// Gate the entire file on the postgres+pgvector+pggraph features being enabled.
#![cfg(all(feature = "pggraph", feature = "pgvector", feature = "postgres"))]

use std::sync::Arc;

use cognee_lib::PipelineContext;
use cognee_lib::add::AddPipeline;
use cognee_lib::cognify::{CognifyConfig, cognify};
use cognee_lib::component_manager::ComponentManager;
use cognee_lib::config::{ConfigManager, Settings};
use cognee_lib::core::{CpuPool, RayonThreadPool};
use cognee_lib::database::{IngestDb, PipelineRunRepository, SeaOrmPipelineRunRepository, ops};
use cognee_lib::models::DataInput;
use cognee_lib::ontology::{NoOpOntologyResolver, OntologyResolver};
use serial_test::serial;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load `.env` and return the `TEST_POSTGRES_URL`, or `None` to skip.
fn postgres_url() -> Option<String> {
    let _ = dotenv::dotenv();
    std::env::var("TEST_POSTGRES_URL")
        .ok()
        .filter(|v| !v.is_empty())
}

/// Return `true` when the LLM + embedding env vars are all set.
fn llm_and_embedding_available() -> bool {
    let _ = dotenv::dotenv();
    let has_llm = std::env::var("OPENAI_URL")
        .or_else(|_| std::env::var("LLM_ENDPOINT"))
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        && std::env::var("OPENAI_TOKEN")
            .or_else(|_| std::env::var("LLM_API_KEY"))
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    let has_embed = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        && std::env::var("COGNEE_E2E_TOKENIZER_PATH")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    has_llm && has_embed
}

/// Build a `Settings` pointing all three stores at `pg_url`.
/// Only relational `db_*` fields are set; graph credentials are intentionally
/// left empty to exercise the Item-2 credential-fallback path.
fn make_all_postgres_settings(pg_url: &str) -> Settings {
    // Parse host/port/name/user/pass from the URL so we can fill the individual
    // db_* fields that the fallback resolver reads.
    let parsed = url::Url::parse(pg_url).expect("TEST_POSTGRES_URL must be a valid URL");
    let host = parsed.host_str().unwrap_or("localhost").to_string();
    let port = parsed.port().unwrap_or(5432);
    let name = parsed.path().trim_start_matches('/').to_string();
    let user = parsed.username().to_string();
    let pass = parsed.password().unwrap_or("").to_string();

    // Pull LLM / embedding settings from env (already validated by the caller).
    let llm_key = std::env::var("OPENAI_TOKEN")
        .or_else(|_| std::env::var("LLM_API_KEY"))
        .unwrap_or_default();
    let llm_endpoint = std::env::var("OPENAI_URL")
        .or_else(|_| std::env::var("LLM_ENDPOINT"))
        .unwrap_or_default();
    let llm_model = std::env::var("LLM_MODEL")
        .or_else(|_| std::env::var("OPENAI_MODEL"))
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let embed_model = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH").unwrap_or_default();
    let embed_tok = std::env::var("COGNEE_E2E_TOKENIZER_PATH").unwrap_or_default();

    Settings {
        // Relational DB — individual fields; resolved_relational_db_url() builds the URL.
        db_provider: "postgres".to_string(),
        db_host: host,
        db_port: port,
        db_name: name,
        db_username: user,
        db_password: pass,

        // Graph DB — postgres provider, credentials intentionally empty to
        // trigger the Item-2 fallback to db_* fields.
        graph_database_provider: "postgres".to_string(),
        // graph_database_url, graph_database_host, etc. left at defaults (empty)

        // Vector DB — use pgvector with the same Postgres instance.
        vector_db_provider: "pgvector".to_string(),
        vector_db_url: pg_url.to_string(),

        // Embedding — real ONNX model for non-trivial similarity.
        embedding_provider: "onnx".to_string(),
        embedding_model_path: embed_model,
        embedding_tokenizer_path: embed_tok,
        embedding_dimensions: 384,

        // LLM
        llm_provider: "openai".to_string(),
        llm_api_key: llm_key,
        llm_endpoint,
        llm_model,

        // Use a unique system root so on-disk data stays isolated.
        system_root_directory: format!("./.cognee_pg_e2e_{}", Uuid::new_v4()),

        ..Settings::default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
#[serial]
async fn pg_full_stack_add_and_cognify() {
    // Skip when Postgres, LLM, or embedding model are unavailable.
    let Some(pg_url) = postgres_url() else {
        eprintln!("TEST_POSTGRES_URL not set — skipping pg_full_stack_add_and_cognify");
        return;
    };
    if !llm_and_embedding_available() {
        eprintln!("LLM/embedding env vars not set — skipping pg_full_stack_add_and_cognify");
        return;
    }

    let settings = make_all_postgres_settings(&pg_url);
    let system_root = settings.system_root_directory.clone();
    let cm = Arc::new(ComponentManager::new(ConfigManager::new(settings)));

    // Initialize all three backends via ComponentManager (exercises wiring).
    let db = cm
        .database()
        .await
        .expect("relational Postgres backend must initialize");
    let graph_db = cm
        .graph_db()
        .await
        .expect("PgGraphAdapter must initialize via ComponentManager");
    let vector_db = cm
        .vector_db()
        .await
        .expect("PgVectorAdapter must initialize via ComponentManager");
    let embedding = cm
        .embedding_engine()
        .await
        .expect("embedding engine must initialize");
    let llm = cm.llm().await.expect("LLM must initialize");
    let storage = cm.storage().await.expect("storage must initialize");

    let owner_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001")
        .expect("static UUID is always valid");
    let dataset_name = format!("pg_e2e_test_{}", Uuid::new_v4().simple());

    // ---- Step 1: Ingest a small text fixture. --------------------------------
    let fixture = "Alice Johnson is a software engineer at TechCorp in San Francisco. \
                   She works on machine learning infrastructure and collaborates with Bob Smith.";

    let ingest_db: Arc<dyn IngestDb> = db.clone();
    let thread_pool: Arc<dyn CpuPool> = Arc::new(
        RayonThreadPool::with_default_threads().expect("RayonThreadPool must create successfully"),
    );

    let add_pipeline = AddPipeline::new(Arc::clone(&storage), ingest_db)
        .with_thread_pool(Arc::clone(&thread_pool))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&db));

    add_pipeline
        .add(
            vec![DataInput::Text(fixture.to_string())],
            &dataset_name,
            owner_id,
            None,
        )
        .await
        .expect("add() must succeed on Postgres stack");

    // ---- Step 2: Cognify. ---------------------------------------------------
    let dataset = ops::datasets::get_dataset_by_name(&db, &dataset_name, owner_id, None)
        .await
        .expect("get_dataset_by_name must succeed")
        .unwrap_or_else(|| panic!("dataset '{dataset_name}' must exist after add()"));

    let data_items = ops::datasets::get_dataset_data(&db, dataset.id)
        .await
        .expect("get_dataset_data must succeed");

    assert!(
        !data_items.is_empty(),
        "Dataset must have > 0 data items after add()"
    );

    let cognify_config = CognifyConfig::default()
        .with_incremental_loading(false)
        .with_summarization(false); // skip summarization to reduce LLM calls

    let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));

    let ontology_resolver: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());

    cognify(
        data_items,
        dataset.id,
        Some(owner_id),
        None,
        dataset.tenant_id,
        Arc::clone(&llm),
        Arc::clone(&storage),
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding),
        Arc::clone(&db),
        Arc::clone(&pipeline_run_repo),
        Arc::clone(&thread_pool),
        Arc::clone(&ontology_resolver),
        &cognify_config,
    )
    .await
    .expect("cognify() must succeed on Postgres stack");

    // ---- Step 3: Assert non-empty graph + vector output. --------------------
    let (nodes, _) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data must succeed");
    assert!(
        !nodes.is_empty(),
        "Graph must have > 0 nodes after cognify on Postgres stack"
    );

    let collections: Vec<(String, String)> = vector_db
        .list_collections()
        .await
        .expect("list_collections must succeed");
    assert!(
        !collections.is_empty(),
        "Vector DB must have > 0 collections after cognify on Postgres stack"
    );

    // ---- Cleanup. -----------------------------------------------------------
    // Best-effort; errors are non-fatal for the test result.
    let _ = graph_db.delete_graph().await;
    let _ = std::fs::remove_dir_all(&system_root);
}
