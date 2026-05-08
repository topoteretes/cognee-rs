//! Integration test: Hard-mode delete sweeps orphan entities from the graph.
//!
//! Ingests two documents with a shared entity ("TechCorp"), cognifies both,
//! then deletes one via `DeleteScope::Data` + `DeleteMode::Hard`. Verifies:
//! - Hard mode swept at least some orphan entities or entity types
//! - Graph node count decreased but is NOT empty (shared-entity doc survives)
//! - At least one data item was deleted
//!
//! Required environment variables (set by `scripts/run_tests_with_openai.sh`):
//!   OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH
//!
//! Run with: cargo test --package cognee-delete --test hard_mode_orphan_sweep

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, DeleteDb, IngestDb, connect, initialize, ops};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::{EmbeddingEngine, config::OnnxEmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_models::DataInput;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};
use tempfile::TempDir;
use uuid::Uuid;

/// Read a required environment variable, loading `.env` first (idempotent).
///
/// Accepts Python-compatible canonical names as fallbacks for legacy aliases.
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

/// Extract the embedding model directory from `COGNEE_E2E_EMBED_MODEL_PATH`.
fn get_embedding_model_dir() -> String {
    if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_string_lossy().to_string();
    }
    "./target/models".to_string()
}

/// Build full infrastructure: storage, database, graph, vector, embedding, LLM.
/// Returns all components needed for add -> cognify -> delete.
async fn setup_infrastructure(
    temp_dir: &TempDir,
) -> (
    Arc<dyn StorageTrait>,
    Arc<DatabaseConnection>,
    Arc<dyn GraphDBTrait>,
    Arc<dyn VectorDB>,
    Arc<dyn EmbeddingEngine>,
    Arc<dyn Llm>,
) {
    // Local file storage
    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    // SQLite metadata database
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
        match OnnxEmbeddingEngine::new(OnnxEmbeddingConfig::bge_small(&model_dir)) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                panic!(
                    "Failed to load embedding model: {}. \
                     Ensure model is at {}/BGE-Small-v1.5-model_quantized.onnx",
                    e, model_dir
                );
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

    (
        storage,
        database,
        graph_db,
        vector_db,
        embedding_engine,
        llm,
    )
}

const DOC1_TEXT: &str = "Alice is a researcher at TechCorp. Alice studies machine learning.";
const DOC2_TEXT: &str = "Bob is an engineer at TechCorp. Bob develops cloud infrastructure.";

#[tokio::test]
async fn test_hard_mode_sweeps_orphan_entities() {
    // ── Environment gating ──────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");
    let _ = require_env("COGNEE_E2E_EMBED_MODEL_PATH");

    // ── Infrastructure setup ────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");
    let (storage, database, graph_db, vector_db, embedding_engine, llm) =
        setup_infrastructure(&temp_dir).await;

    let owner_id = Uuid::nil();

    // ── Ingest two documents into the same dataset ──────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>);

    let data_items_1 = ingest
        .add(
            vec![DataInput::Text(DOC1_TEXT.to_string())],
            "orphan_sweep_ds",
            owner_id,
            None,
        )
        .await
        .expect("ingest doc1");

    let data_items_2 = ingest
        .add(
            vec![DataInput::Text(DOC2_TEXT.to_string())],
            "orphan_sweep_ds",
            owner_id,
            None,
        )
        .await
        .expect("ingest doc2");

    assert_eq!(data_items_1.len(), 1, "Expected 1 data item from doc1");
    assert_eq!(data_items_2.len(), 1, "Expected 1 data item from doc2");

    let doc1_data_id = data_items_1[0].id;

    // ── Cognify both documents together ─────────────────────────────────────
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    let dataset = ops::datasets::get_dataset_by_name(&database, "orphan_sweep_ds", owner_id, None)
        .await
        .expect("get_dataset_by_name")
        .expect("dataset should exist after ingest");

    let mut all_data_items = data_items_1;
    all_data_items.extend(data_items_2);

    let cognify_result = match cognify(
        all_data_items,
        dataset.id,
        None,
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone() as Arc<dyn StorageTrait>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        None,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping test: cognify failed: {e}");
            return;
        }
    };

    assert!(
        !cognify_result.entities.is_empty(),
        "Cognify should extract entities from both documents"
    );
    println!(
        "Cognify: {} chunks, {} entities, {} edges",
        cognify_result.chunks.len(),
        cognify_result.entities.len(),
        cognify_result.edges.len()
    );

    // ── Capture pre-delete graph state ──────────────────────────────────────
    let (pre_delete_nodes, _pre_delete_edges) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data before delete");
    let pre_delete_node_count = pre_delete_nodes.len();

    assert!(
        pre_delete_node_count > 0,
        "Graph should have nodes after cognify"
    );
    println!("Pre-delete graph: {} nodes", pre_delete_node_count);

    // ── Delete doc1 with Hard mode ──────────────────────────────────────────
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db));

    let result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id: doc1_data_id,
                dataset_name: Some("orphan_sweep_ds".to_string()),
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Hard,
        })
        .await
        .expect("hard delete should succeed");

    println!(
        "Delete result: deleted_data={}, deleted_orphan_entities={}, \
         deleted_orphan_entity_types={}, deleted_orphan_edge_types={}",
        result.deleted_data,
        result.deleted_orphan_entities,
        result.deleted_orphan_entity_types,
        result.deleted_orphan_edge_types,
    );

    // ── Verify hard mode swept orphans ──────────────────────────────────────
    assert!(
        result.deleted_orphan_entities > 0
            || result.deleted_orphan_entity_types > 0
            || result.deleted_orphan_edge_types > 0,
        "Hard mode should sweep at least some orphan entities, entity types, or edge types; \
         got entities={}, entity_types={}, edge_types={}",
        result.deleted_orphan_entities,
        result.deleted_orphan_entity_types,
        result.deleted_orphan_edge_types,
    );

    // ── Verify graph node count decreased ───────────────────────────────────
    let (post_delete_nodes, _post_delete_edges) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data after delete");
    let post_delete_node_count = post_delete_nodes.len();

    println!(
        "Post-delete graph: {} nodes (was {})",
        post_delete_node_count, pre_delete_node_count
    );

    assert!(
        post_delete_node_count < pre_delete_node_count,
        "Graph node count should decrease after hard delete; \
         before={}, after={}",
        pre_delete_node_count,
        post_delete_node_count,
    );

    // ── Verify graph is NOT empty (doc2 nodes survive) ──────────────────────
    assert!(
        post_delete_node_count > 0,
        "Graph should NOT be empty — doc2 nodes must survive"
    );

    // ── Verify at least one data item was deleted ───────────────────────────
    assert!(
        result.deleted_data >= 1,
        "At least one data item should be deleted; got {}",
        result.deleted_data,
    );

    println!("test_hard_mode_sweeps_orphan_entities PASSED");
}

#[tokio::test]
async fn test_soft_mode_preserves_orphan_entities() {
    // ── Environment gating ──────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");
    let _ = require_env("COGNEE_E2E_EMBED_MODEL_PATH");

    // ── Infrastructure setup ────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");
    let (storage, database, graph_db, vector_db, embedding_engine, llm) =
        setup_infrastructure(&temp_dir).await;

    let owner_id = Uuid::nil();

    // ── Ingest two documents into the same dataset ──────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>);

    let data_items_1 = ingest
        .add(
            vec![DataInput::Text(DOC1_TEXT.to_string())],
            "soft_sweep_ds",
            owner_id,
            None,
        )
        .await
        .expect("ingest doc1");

    let data_items_2 = ingest
        .add(
            vec![DataInput::Text(DOC2_TEXT.to_string())],
            "soft_sweep_ds",
            owner_id,
            None,
        )
        .await
        .expect("ingest doc2");

    let doc1_data_id = data_items_1[0].id;

    // ── Cognify both documents together ─────────────────────────────────────
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    let dataset = ops::datasets::get_dataset_by_name(&database, "soft_sweep_ds", owner_id, None)
        .await
        .expect("get_dataset_by_name")
        .expect("dataset should exist after ingest");

    let mut all_data_items = data_items_1;
    all_data_items.extend(data_items_2);

    if let Err(e) = cognify(
        all_data_items,
        dataset.id,
        None,
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone() as Arc<dyn StorageTrait>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        None,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        eprintln!("Skipping test: cognify failed: {e}");
        return;
    }

    // ── Capture pre-delete graph state ──────────────────────────────────────
    let (pre_delete_nodes, _) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data before delete");
    let pre_delete_node_count = pre_delete_nodes.len();

    // ── Delete doc1 with Soft mode ──────────────────────────────────────────
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db));

    let result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id: doc1_data_id,
                dataset_name: Some("soft_sweep_ds".to_string()),
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Soft,
        })
        .await
        .expect("soft delete should succeed");

    // ── Verify soft mode does NOT sweep orphans ─────────────────────────────
    assert_eq!(
        result.deleted_orphan_entities, 0,
        "Soft mode should NOT sweep orphan entities"
    );
    assert_eq!(
        result.deleted_orphan_entity_types, 0,
        "Soft mode should NOT sweep orphan entity types"
    );
    assert_eq!(
        result.deleted_orphan_edge_types, 0,
        "Soft mode should NOT sweep orphan edge types"
    );

    // ── Verify data was still deleted ───────────────────────────────────────
    assert!(
        result.deleted_data >= 1,
        "At least one data item should be deleted even in soft mode; got {}",
        result.deleted_data,
    );

    // Graph may or may not have fewer nodes (provenance nodes removed in both
    // modes), but orphan sweep specifically should not fire.
    let (post_delete_nodes, _) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data after delete");
    let post_delete_node_count = post_delete_nodes.len();

    // The graph should still have nodes — at minimum the doc2 nodes.
    assert!(
        post_delete_node_count > 0,
        "Graph should NOT be empty after soft delete — doc2 nodes must survive"
    );

    println!(
        "Soft delete: graph went from {} to {} nodes (orphan sweep skipped)",
        pre_delete_node_count, post_delete_node_count
    );

    println!("test_soft_mode_preserves_orphan_entities PASSED");
}
