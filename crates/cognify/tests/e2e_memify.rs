//! Optional real-backend end-to-end test for the memify pipeline.
//!
//! Seeds a small graph directly in Ladybug (no cognify / no LLM), runs
//! `memify(...)` with a real `OnnxEmbeddingEngine` and an embedded
//! `QdrantAdapter`, then issues a `TripletCompletion` search via
//! `SearchOrchestrator` using `only_context=true` (to avoid the LLM
//! completion step in the retriever).
//!
//! Gated on `COGNEE_E2E_EMBED_MODEL_PATH` / `COGNEE_E2E_TOKENIZER_PATH` —
//! prints a skip message and returns green when absent. Also graceful-skips
//! if `OnnxEmbeddingEngine::new` fails for any reason.
//!
//! Completes Phase 9 Step 8 of the memify test coverage plan.

#![cfg(feature = "testing")]

use std::sync::Arc;

use cognee_cognify::memify::{MemifyConfig, memify};
use cognee_core::{CpuPool, RayonThreadPool};
use cognee_database::{DatabaseConnection, SearchHistoryDb, connect, initialize};
use cognee_embedding::{EmbeddingEngine, config::OnnxEmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_llm::Llm;
use cognee_search::{
    SearchBuilder, SearchRequest, SearchType,
    types::{SearchOutput, SearchResponse},
};
use cognee_test_utils::MockLlm;
use cognee_vector::{QdrantAdapter, VectorDB};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

/// Resolve the directory that holds BGE-Small-v1.5 model artifacts.
///
/// Mirrors the helper used in `integration_default_backend.rs` /
/// `integration_search_matrix.rs`: prefers the parent of
/// `COGNEE_E2E_EMBED_MODEL_PATH`, falls back to `./target/models`.
fn get_embedding_model_dir() -> String {
    if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_string_lossy().to_string();
    }
    "./target/models".to_string()
}

/// Seed a single node on the Ladybug adapter with `name` + `description`
/// (and the `type` default of `"Unknown"` — memify's default path uses
/// `get_graph_data()` which does not filter on type).
async fn add_node(graph_db: &dyn GraphDBTrait, id: Uuid, name: &str, description: &str) {
    let mut node_json = serde_json::Map::new();
    node_json.insert("id".to_string(), json!(id.to_string()));
    node_json.insert("name".to_string(), json!(name));
    node_json.insert("description".to_string(), json!(description));
    graph_db
        .add_node_raw(serde_json::Value::Object(node_json))
        .await
        .expect("add_node_raw must succeed on a fresh LadybugAdapter");
}

/// Seed an edge between two existing nodes with a relationship name.
async fn add_edge(graph_db: &dyn GraphDBTrait, source: Uuid, target: Uuid, relationship: &str) {
    graph_db
        .add_edge(&source.to_string(), &target.to_string(), relationship, None)
        .await
        .expect("add_edge must succeed on a fresh LadybugAdapter");
}

/// Build a `SearchRequest` with all optional fields set to `None`, plus the
/// specific knobs we need for a no-LLM `TripletCompletion` run.
fn make_request(query: &str, search_type: SearchType) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type,
        top_k: Some(10),
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        // `only_context = true` short-circuits the orchestrator before any
        // LLM call and returns the retrieved items directly, which is what
        // we want in a test that has no real LLM wired up.
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
    }
}

/// Concatenate all payload JSON strings from an `only_context` response so
/// we can do a simple case-insensitive substring match against seeded data.
fn response_payload_text(response: &SearchResponse) -> String {
    match &response.result {
        SearchOutput::Items(items) => items
            .iter()
            .map(|item| item.payload.to_string())
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase(),
        SearchOutput::Texts(texts) => texts.join(" ").to_lowercase(),
        SearchOutput::Text(text) => text.to_lowercase(),
        _ => String::new(),
    }
}

#[tokio::test]
async fn test_memify_e2e_real_embedding_real_qdrant() {
    // ── Gate: graceful skip when model artifacts are absent ─────────────────
    if std::env::var("COGNEE_E2E_EMBED_MODEL_PATH").is_err() {
        eprintln!(
            "⚠️  skipping test_memify_e2e_real_embedding_real_qdrant: \
             COGNEE_E2E_EMBED_MODEL_PATH is not set"
        );
        return;
    }
    if std::env::var("COGNEE_E2E_TOKENIZER_PATH").is_err() {
        eprintln!(
            "⚠️  skipping test_memify_e2e_real_embedding_real_qdrant: \
             COGNEE_E2E_TOKENIZER_PATH is not set"
        );
        return;
    }

    // ── Infrastructure setup (all ephemeral, in a TempDir) ──────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    let model_dir = get_embedding_model_dir();
    let embedding_engine: Arc<dyn EmbeddingEngine> =
        match OnnxEmbeddingEngine::new(OnnxEmbeddingConfig::bge_small(&model_dir)) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!(
                    "⚠️  skipping test_memify_e2e_real_embedding_real_qdrant: \
                     failed to load embedding model: {e}"
                );
                eprintln!(
                    "   Ensure model is at {}/BGE-Small-v1.5-model_quantized.onnx",
                    model_dir
                );
                return;
            }
        };

    // Embedded Qdrant (BGE-Small dimension = 384).
    let vector_db: Arc<dyn VectorDB> =
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), 384));

    // Embedded Ladybug.
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new"),
    );
    graph_db.initialize().await.expect("graph_db.initialize");

    // In-memory SQLite for search history (required by SearchBuilder).
    let db_path = temp_dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create sqlite db file");
    let db_url = format!("sqlite://{}", db_path.display());
    let db = connect(&db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let database: Arc<DatabaseConnection> = Arc::new(db);

    // MockLlm — TripletCompletion with `only_context=true` returns the
    // context items directly and never calls `llm.generate()`, but the
    // builder still requires an `Arc<dyn Llm>`.
    let llm: Arc<dyn Llm> = Arc::new(MockLlm::empty());

    // ── Seed the graph directly (no cognify, no LLM) ────────────────────────
    //
    // 4 nodes, 3 edges:
    //   Alice  --works_at--> TechCorp
    //   Alice  --knows    --> Bob
    //   Bob    --works_at--> TechCorp
    let alice = Uuid::new_v4();
    let techcorp = Uuid::new_v4();
    let bob = Uuid::new_v4();
    let carol = Uuid::new_v4();

    add_node(graph_db.as_ref(), alice, "Alice", "Software engineer").await;
    add_node(
        graph_db.as_ref(),
        techcorp,
        "TechCorp",
        "Technology company",
    )
    .await;
    add_node(graph_db.as_ref(), bob, "Bob", "Product manager").await;
    add_node(graph_db.as_ref(), carol, "Carol", "Designer").await;

    add_edge(graph_db.as_ref(), alice, techcorp, "works_at").await;
    add_edge(graph_db.as_ref(), alice, bob, "knows").await;
    add_edge(graph_db.as_ref(), bob, techcorp, "works_at").await;

    // ── Run memify on the seeded graph ──────────────────────────────────────
    let memify_config = MemifyConfig::default();
    let pool: Arc<dyn CpuPool> =
        Arc::new(RayonThreadPool::with_default_threads().expect("rayon pool"));
    let result = memify(
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        pool,
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Some(Uuid::new_v4()), // dataset_id
        Some(Uuid::new_v4()), // user_id
        None,                 // tenant_id
        &memify_config,
    )
    .await
    .expect("memify should succeed on the seeded graph");

    assert!(
        result.triplet_count > 0,
        "memify should produce at least one triplet for 3 seeded edges"
    );
    assert_eq!(
        result.index_result.indexed_count, result.triplet_count,
        "all extracted triplets must be indexed when no embedding errors occur \
         (indexed={}, triplet_count={})",
        result.index_result.indexed_count, result.triplet_count,
    );
    assert!(
        vector_db
            .has_collection("Triplet", "text")
            .await
            .expect("has_collection"),
        "the Triplet:text collection must exist after memify"
    );

    // ── Build the SearchOrchestrator and issue a TripletCompletion query ────
    let orchestrator = SearchBuilder::new(
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&graph_db),
        Arc::clone(&llm),
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    // A query that should be semantically close to at least one seeded triplet.
    let query = "Who works at TechCorp?";
    let response = orchestrator
        .search(&make_request(query, SearchType::TripletCompletion))
        .await
        .expect("TripletCompletion search should succeed");

    // With `only_context = true` the orchestrator returns the retrieved
    // context as `SearchOutput::Items(..)`. That must be non-empty.
    match &response.result {
        SearchOutput::Items(items) => {
            assert!(
                !items.is_empty(),
                "TripletCompletion should return at least one context item \
                 after memifying 3 seeded edges"
            );
        }
        other => panic!("only_context=true should yield SearchOutput::Items, got {other:?}"),
    }

    // The memify-indexed metadata includes `relationship`, `source_id`,
    // `target_id`, etc. Assert that at least one returned payload mentions
    // a seeded relationship name or entity name (case-insensitive).
    let haystack = response_payload_text(&response);
    let seeds = ["works_at", "knows", "alice", "bob", "techcorp", "carol"];
    let hit = seeds.iter().any(|needle| haystack.contains(needle));
    assert!(
        hit,
        "at least one returned payload must reference a seeded relationship \
         or entity name (seeds={seeds:?}); payload text: {haystack}"
    );
}
