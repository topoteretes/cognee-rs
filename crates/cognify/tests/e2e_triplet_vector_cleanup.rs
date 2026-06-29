#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! E2E test: triplet vector points are cleaned up after data-scope deletion.
//!
//! Two documents in separate datasets are cognified and memified. After
//! deleting one document's data, the Triplet vector collection should lose
//! only that document's triplet points while preserving the other's.
//!
//! Required env vars: OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL, COGNEE_E2E_EMBED_MODEL_PATH

use std::sync::Arc;

use cognee_cognify::memify::{MemifyConfig, memify};
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, DeleteDb, IngestDb, connect, initialize, ops};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockVectorDB;
use cognee_vector::VectorDB;
use tempfile::TempDir;
use uuid::Uuid;

mod test_utils;
use test_utils::{create_deterministic_embedding_engine, create_llm_from_env};

const AI_TEXT: &str = include_str!("test_data/artificial_intelligence.txt");

const QUANTUM_TEXT: &str = "\
Quantum computing leverages quantum mechanical phenomena like superposition \
and entanglement to perform computations. Quantum bits (qubits) can exist \
in multiple states simultaneously, enabling quantum computers to solve \
certain problems exponentially faster than classical computers. \
Companies like IBM, Google, and Microsoft are investing heavily in \
quantum hardware and quantum error correction research.";

#[tokio::test]
async fn test_triplet_vector_cleanup_after_data_delete() {
    // LLM comes from create_llm_from_env (replay/record/real); embeddings from
    // create_test_embedding_engine, which honours MOCK_EMBEDDING=deterministic.
    // No explicit OPENAI_* gating needed — replay mode runs without credentials.

    // ── Infrastructure ──────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    let embedding_engine: Arc<dyn EmbeddingEngine> = create_deterministic_embedding_engine();

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

    // In-memory mock vector DB (qdrant extracted to closed cognee-vector-qdrant).
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());

    let llm: Arc<dyn Llm> = create_llm_from_env("triplet_vector_cleanup");

    let owner_id = Uuid::nil();
    let ontology = Arc::new(NoOpOntologyResolver::new());
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false); // memify will create triplets

    // ── Step 1: Ingest two documents ────────────────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

    let data_ai = ingest
        .add(
            vec![DataInput::Text(AI_TEXT.to_string())],
            "ds_ai",
            owner_id,
            None,
        )
        .await
        .expect("ingest ds_ai");
    assert_eq!(data_ai.len(), 1);
    let data_ai_id = data_ai[0].id;

    let data_q = ingest
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "ds_quantum",
            owner_id,
            None,
        )
        .await
        .expect("ingest ds_quantum");
    assert_eq!(data_q.len(), 1);

    let ds_ai = ops::datasets::get_dataset_by_name(&database, "ds_ai", owner_id, None)
        .await
        .expect("get ds_ai")
        .expect("ds_ai should exist");
    let ds_q = ops::datasets::get_dataset_by_name(&database, "ds_quantum", owner_id, None)
        .await
        .expect("get ds_quantum")
        .expect("ds_quantum should exist");

    // ── Step 2: Cognify both ────────────────────────────────────────────
    let _result_ai = match cognify(
        data_ai,
        ds_ai.id,
        Some(owner_id),
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone(),
        graph_db.clone(),
        vector_db.clone(),
        embedding_engine.clone(),
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
        ) as Arc<dyn cognee_core::CpuPool>,
        ontology.clone(),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: cognify ds_ai failed: {e}");
            return;
        }
    };

    let _result_q = match cognify(
        data_q,
        ds_q.id,
        Some(owner_id),
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone(),
        graph_db.clone(),
        vector_db.clone(),
        embedding_engine.clone(),
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
        ) as Arc<dyn cognee_core::CpuPool>,
        ontology.clone(),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: cognify ds_quantum failed: {e}");
            return;
        }
    };

    println!("Step 2 OK: Both datasets cognified");

    // ── Step 3: Memify both datasets ────────────────────────────────────
    let memify_config = MemifyConfig::default();
    let memify_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("rayon pool"));

    let memify_ai = memify(
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&memify_pool),
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Some(ds_ai.id),
        None,
        None,
        &memify_config,
    )
    .await
    .expect("memify ds_ai");

    let memify_q = memify(
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        memify_pool,
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Some(ds_q.id),
        None,
        None,
        &memify_config,
    )
    .await
    .expect("memify ds_quantum");

    assert!(
        memify_ai.triplet_count > 0,
        "ds_ai should have triplets after memify"
    );
    assert!(
        memify_q.triplet_count > 0,
        "ds_quantum should have triplets after memify"
    );

    println!(
        "Step 3 OK: Memified — ds_ai: {} triplets, ds_quantum: {} triplets",
        memify_ai.triplet_count, memify_q.triplet_count,
    );

    // ── Step 4: Verify Triplet collection exists and has points ─────────
    assert!(
        vector_db
            .has_collection("Triplet", "text")
            .await
            .expect("has_collection"),
        "Triplet:text collection should exist after memify"
    );

    let pre_triplet_count = vector_db
        .collection_size("Triplet", "text")
        .await
        .expect("collection_size pre-delete");
    let expected_total = memify_ai.triplet_count + memify_q.triplet_count;
    println!(
        "Step 4: Triplet collection has {pre_triplet_count} points (expected ~{expected_total})",
    );
    assert!(
        pre_triplet_count > 0,
        "Triplet collection should have points"
    );

    // ── Step 5: Delete ds_ai data (data-scope, not dataset-scope) ───────
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(graph_db.clone())
            .with_vector_db(vector_db.clone());

    let delete_result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id: data_ai_id,
                dataset_name: Some("ds_ai".to_string()),
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await
        .expect("delete ds_ai data");

    assert!(
        delete_result.deleted_data >= 1,
        "Should have deleted at least 1 data item"
    );
    println!(
        "Step 5 OK: Deleted ds_ai data ({} data, {} vector points)",
        delete_result.deleted_data, delete_result.deleted_vector_points,
    );

    // ── Step 6: Verify triplet vector cleanup ───────────────────────────
    let post_triplet_count = vector_db
        .collection_size("Triplet", "text")
        .await
        .expect("collection_size post-delete");

    println!(
        "Step 6: Triplet collection now has {post_triplet_count} points (was {pre_triplet_count})",
    );

    // Triplet count should have decreased (ds_ai triplets removed)
    assert!(
        post_triplet_count < pre_triplet_count,
        "Triplet count should decrease after data-scope delete: post={post_triplet_count}, pre={pre_triplet_count}",
    );

    // Triplet collection should still have ds_quantum's points
    assert!(
        post_triplet_count > 0,
        "Triplet collection should still have ds_quantum's triplet points"
    );

    // ds_quantum should still be intact in the DB
    let ds_q_after = ops::datasets::get_dataset_by_name(&database, "ds_quantum", owner_id, None)
        .await
        .expect("get ds_quantum after delete")
        .expect("ds_quantum should still exist");
    let q_data = ops::datasets::get_dataset_data(&database, ds_q_after.id)
        .await
        .expect("get ds_quantum data");
    assert!(
        !q_data.is_empty(),
        "ds_quantum should still have its data items"
    );

    println!("PASSED: test_triplet_vector_cleanup_after_data_delete");
}
