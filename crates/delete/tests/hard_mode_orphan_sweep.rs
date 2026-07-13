#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::mock::{MissPolicy, RecordingLlm, ReplayLlm};
use cognee_llm::{Llm, build_openai_compatible_adapter};
use cognee_models::DataInput;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockVectorDB;
use cognee_vector::VectorDB;
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

/// In cassette-replay mode a pipeline error means a stale/missing cassette
/// entry; the `Err => eprintln + return` skip blocks below would otherwise
/// swallow it and pass with zero assertions. Call this in those blocks so a
/// replay miss fails loudly (re-record cassettes); no-op outside replay mode.
fn fail_loudly_on_replay_miss(what: &str, err: &impl std::fmt::Display) {
    if std::env::var("COGNEE_TEST_REPLAY").is_ok_and(|v| !v.is_empty()) {
        panic!(
            "{what} failed in replay mode — likely a stale/missing cassette entry; re-record cassettes. Error: {err}"
        );
    }
}

/// LLM for this test: offline replay when `COGNEE_TEST_REPLAY=1` (MissPolicy::Error
/// so a stale cassette fails loudly), recording when `COGNEE_RECORD_LLM=1`, else
/// the real adapter. Mirrors crates/cognify/tests/test_utils.rs (Approach E); the
/// delete crate has no shared test_utils module, so it is inlined here.
fn create_llm_from_env(cassette_name: &str) -> Arc<dyn Llm> {
    let cassette = format!(
        "{}/tests/fixtures/cassettes/{cassette_name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var("COGNEE_TEST_REPLAY").is_ok_and(|v| !v.is_empty()) {
        return Arc::new(
            ReplayLlm::from_path(&cassette)
                .unwrap_or_else(|e| panic!("❌ Failed to load cassette {cassette}: {e}"))
                .with_miss_policy(MissPolicy::Error),
        );
    }
    // Route through the production factory (provider from env, default `openai`)
    // so litellm-style model prefixes are stripped exactly as in a real run.
    let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".to_string());
    let adapter: Arc<dyn Llm> = Arc::new(
        build_openai_compatible_adapter(
            &provider,
            &require_env("OPENAI_MODEL"),
            &require_env("OPENAI_TOKEN"),
            &require_env("OPENAI_URL"),
            3,
        )
        .expect("build_openai_compatible_adapter"),
    );
    if std::env::var("COGNEE_RECORD_LLM").is_ok_and(|v| !v.is_empty()) {
        return Arc::new(RecordingLlm::new(adapter, cassette));
    }
    adapter
}

/// Build full infrastructure: storage, database, graph, vector, embedding, LLM.
/// Returns `Some` with all components needed for add -> cognify -> delete,
/// or `None` if the embedding engine could not be initialised (test will skip).
async fn setup_infrastructure(
    temp_dir: &TempDir,
) -> Option<(
    Arc<dyn StorageTrait>,
    Arc<DatabaseConnection>,
    Arc<dyn GraphDBTrait>,
    Arc<dyn VectorDB>,
    Arc<dyn EmbeddingEngine>,
    Arc<dyn Llm>,
)> {
    // Deterministic in-process embeddings (no model/API needed); the delete
    // assertions are structural (node counts), not semantic.
    let embedding_engine: Arc<dyn EmbeddingEngine> =
        Arc::new(MockEmbeddingEngine::deterministic(384));

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

    // Qdrant vector database
    // In-memory mock vector DB (qdrant extracted to closed cognee-vector-qdrant).
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());

    // LLM via cassette (replay/record/real) — see create_llm_from_env above.
    let llm: Arc<dyn Llm> = create_llm_from_env("hard_mode_orphan_sweep");

    Some((
        storage,
        database,
        graph_db,
        vector_db,
        embedding_engine,
        llm,
    ))
}

const DOC1_TEXT: &str = "Alice is a researcher at TechCorp. Alice studies machine learning.";
const DOC2_TEXT: &str = "Bob is an engineer at TechCorp. Bob develops cloud infrastructure.";

#[tokio::test]
async fn test_hard_mode_sweeps_orphan_entities() {
    // ── Infrastructure setup ────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");
    let Some((storage, database, graph_db, vector_db, embedding_engine, llm)) =
        setup_infrastructure(&temp_dir).await
    else {
        return;
    };

    let owner_id = Uuid::nil();

    // ── Ingest two documents into the same dataset ──────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

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
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
        ) as Arc<dyn cognee_core::CpuPool>,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            fail_loudly_on_replay_miss("cognify", &e);
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
    println!("Pre-delete graph: {pre_delete_node_count} nodes");

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
            memory_only: false,
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

    println!("Post-delete graph: {post_delete_node_count} nodes (was {pre_delete_node_count})");

    assert!(
        post_delete_node_count < pre_delete_node_count,
        "Graph node count should decrease after hard delete; \
         before={pre_delete_node_count}, after={post_delete_node_count}",
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
    // ── Infrastructure setup ────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");
    let Some((storage, database, graph_db, vector_db, embedding_engine, llm)) =
        setup_infrastructure(&temp_dir).await
    else {
        return;
    };

    let owner_id = Uuid::nil();

    // ── Ingest two documents into the same dataset ──────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

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
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
        ) as Arc<dyn cognee_core::CpuPool>,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        fail_loudly_on_replay_miss("cognify (re-add)", &e);
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
            memory_only: false,
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
        "Soft delete: graph went from {pre_delete_node_count} to {post_delete_node_count} nodes (orphan sweep skipped)"
    );

    println!("test_soft_mode_preserves_orphan_entities PASSED");
}
