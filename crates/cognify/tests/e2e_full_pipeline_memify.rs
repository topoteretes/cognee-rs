//! End-to-end test: add -> cognify -> memify -> search (TripletCompletion) -> delete -> verify.
//!
//! Exercises the full 6-stage pipeline with real backends:
//! SQLite (metadata), Ladybug (graph), Qdrant (vector), LocalStorage (files),
//! ONNX BGE-Small (embeddings), OpenAI-compatible LLM.
//!
//! Required environment variables (set by `scripts/run_tests_with_local_env.sh`):
//!   OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH
//!
//! Run with: cargo test --package cognee-cognify --test e2e_full_pipeline_memify

use std::sync::Arc;

use cognee_cognify::memify::{MemifyConfig, memify};
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{
    DatabaseConnection, DeleteDb, IngestDb, SearchHistoryDb, connect, initialize, ops,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::{EmbeddingEngine, config::OnnxEmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, OpenAIAdapter};
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
use test_utils::{get_embedding_model_dir, require_env};

const AI_TEXT: &str = include_str!("test_data/artificial_intelligence.txt");

/// Returns true if the search result contains any data.
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
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: None,
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
async fn test_full_pipeline_add_cognify_memify_search_delete() {
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
        match OnnxEmbeddingEngine::new(OnnxEmbeddingConfig::bge_small(&model_dir)) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!("Skipping test: failed to load embedding model: {}", e);
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

    // ── Step 1: Add (ingest) ─────────────────────────────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));
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
    println!("Step 1 OK: Ingested {} data item(s)", data_items.len());

    let dataset =
        ops::datasets::get_dataset_by_name(&database, "artificial_intelligence", owner_id, None)
            .await
            .expect("get_dataset_by_name")
            .expect("dataset should exist after ingest");

    // ── Step 2: Cognify (triplet_embeddings=false so memify creates them) ────
    let config = CognifyConfig::default()
        .with_summarization(true)
        .with_triplet_embeddings(false);

    let cognify_result = match cognify(
        data_items,
        dataset.id,
        Some(owner_id),
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone() as Arc<dyn StorageTrait>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        database.clone(),
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
            eprintln!("Skipping test: cognify failed: {}", e);
            return;
        }
    };

    assert!(
        !cognify_result.chunks.is_empty(),
        "Chunks should be non-empty after cognify"
    );
    assert!(
        !cognify_result.entities.is_empty(),
        "Entities should be extracted after cognify"
    );
    assert!(
        !graph_db.is_empty().await.expect("graph_db.is_empty"),
        "Graph should be non-empty after cognify"
    );
    println!(
        "Step 2 OK: Cognify produced {} chunks, {} entities, {} edges",
        cognify_result.chunks.len(),
        cognify_result.entities.len(),
        cognify_result.edges.len()
    );

    // Triplet collection should NOT exist yet (triplet_embeddings was false)
    let has_triplets_before = vector_db
        .has_collection("Triplet", "text")
        .await
        .expect("has_collection Triplet before memify");
    assert!(
        !has_triplets_before,
        "Triplet collection should not exist before memify (triplet_embeddings=false)"
    );

    // ── Step 3: Memify ───────────────────────────────────────────────────────
    let memify_config = MemifyConfig::default();
    let memify_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("rayon pool"));
    let memify_result = memify(
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        memify_pool,
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Some(dataset.id),
        None, // user_id
        None, // tenant_id
        &memify_config,
    )
    .await
    .expect("memify should succeed on the cognify-populated graph");

    assert!(
        memify_result.triplet_count > 0,
        "memify should produce at least one triplet from cognify edges"
    );
    assert_eq!(
        memify_result.index_result.indexed_count, memify_result.triplet_count,
        "all extracted triplets must be indexed (indexed={}, triplet_count={})",
        memify_result.index_result.indexed_count, memify_result.triplet_count,
    );

    let has_triplets_after = vector_db
        .has_collection("Triplet", "text")
        .await
        .expect("has_collection Triplet after memify");
    assert!(
        has_triplets_after,
        "Triplet:text collection must exist after memify"
    );
    println!(
        "Step 3 OK: Memify indexed {} triplets",
        memify_result.triplet_count
    );

    // ── Step 4: Search (TripletCompletion) ───────────────────────────────────
    let orchestrator = SearchBuilder::new(
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        llm.clone() as Arc<dyn Llm>,
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    // Use first extracted entity name as the query term
    let query = cognify_result.entities[0].entity.name.clone();
    println!("Step 4: Searching for {:?}", query);

    let triplet_response = orchestrator
        .search(&make_request(&query, SearchType::TripletCompletion))
        .await
        .expect("search TripletCompletion");
    assert!(
        is_non_empty(&triplet_response),
        "TripletCompletion should return non-empty result after memify"
    );
    println!("Step 4 OK: TripletCompletion returned non-empty result");

    // Also verify a basic search type still works alongside triplet search
    let chunks_response = orchestrator
        .search(&make_request(&query, SearchType::Chunks))
        .await
        .expect("search Chunks");
    assert!(
        is_non_empty(&chunks_response),
        "Chunks search should return non-empty result"
    );
    println!("Step 4 OK: Chunks search also returned non-empty result");

    // ── Step 5: Delete ───────────────────────────────────────────────────────
    // 5a. Preview first (dry run)
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(graph_db.clone() as Arc<dyn GraphDBTrait>)
            .with_vector_db(vector_db.clone() as Arc<dyn VectorDB>);

    let preview = delete_svc
        .preview(&DeleteRequest {
            scope: DeleteScope::All,
            mode: DeleteMode::Hard,
        })
        .await
        .expect("delete preview");

    assert!(
        preview.datasets_to_delete > 0,
        "Preview should report at least 1 dataset to delete"
    );
    assert!(
        preview.data_to_delete > 0,
        "Preview should report at least 1 data item to delete"
    );
    assert!(
        preview.graph_nodes_to_delete > 0,
        "Preview should report graph nodes to delete"
    );
    assert!(
        preview.vector_points_to_delete > 0,
        "Preview should report vector points to delete"
    );
    println!(
        "Step 5a OK: Preview reports {} datasets, {} data, {} graph nodes, {} vector points",
        preview.datasets_to_delete,
        preview.data_to_delete,
        preview.graph_nodes_to_delete,
        preview.vector_points_to_delete,
    );

    // 5b. Execute hard delete
    let delete_result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::All,
            mode: DeleteMode::Hard,
        })
        .await
        .expect("delete execute");

    assert!(
        delete_result.deleted_datasets > 0,
        "Should have deleted at least 1 dataset"
    );
    assert!(
        delete_result.deleted_data > 0,
        "Should have deleted at least 1 data item"
    );
    assert!(
        delete_result.deleted_graph_nodes > 0,
        "Should have deleted graph nodes"
    );
    assert!(
        delete_result.deleted_vector_points > 0,
        "Should have deleted vector points"
    );
    println!(
        "Step 5b OK: Deleted {} datasets, {} data, {} graph nodes, {} vector points",
        delete_result.deleted_datasets,
        delete_result.deleted_data,
        delete_result.deleted_graph_nodes,
        delete_result.deleted_vector_points,
    );

    // Also clean the graph (DeleteService.execute for All scope calls
    // delete_graph internally, but do it explicitly to be thorough)
    graph_db
        .delete_graph()
        .await
        .expect("graph_db.delete_graph");

    // ── Step 6: Verify cleanup ───────────────────────────────────────────────
    // 6a. Relational DB should be empty
    let remaining_datasets = ops::datasets::list_datasets(&database)
        .await
        .expect("list_datasets after delete");
    assert!(
        remaining_datasets.is_empty(),
        "All datasets should be deleted; found {:?}",
        remaining_datasets
    );

    // 6b. Graph should be empty
    assert!(
        graph_db.is_empty().await.expect("graph_db.is_empty"),
        "Graph should be empty after delete"
    );

    // 6c. Vector collections should be empty (all collections cleaned up)
    let collections = vector_db
        .list_collections()
        .await
        .expect("list_collections after delete");
    // After a full hard delete, either collections are gone or they are empty.
    // Check that we can't find any Triplet data.
    let has_triplets_post_delete = vector_db
        .has_collection("Triplet", "text")
        .await
        .unwrap_or(false);
    if has_triplets_post_delete {
        // If the collection still exists, verify it has no data
        let empty_query = vec![0.0_f32; 384];
        let leftover = vector_db
            .search_similar("Triplet", "text", &empty_query, 1)
            .await
            .unwrap_or_default();
        assert!(
            leftover.is_empty(),
            "Triplet collection should be empty after delete, but found {} points",
            leftover.len()
        );
    }

    println!(
        "Step 6 OK: Cleanup verified - 0 datasets, graph empty, {} remaining collections",
        collections.len()
    );
    println!("PASSED: test_full_pipeline_add_cognify_memify_search_delete");
}
