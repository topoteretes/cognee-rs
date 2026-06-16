//! E2E lifecycle loop test: add → cognify → delete → re-add → re-cognify → search.
//!
//! Verifies that after a hard delete of a dataset, the same content can be
//! re-ingested and re-cognified with identical deterministic IDs (UUID5),
//! and that search works on the re-created data.
//!
//! Required environment variables:
//!   OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH
//!
//! Run with: cargo test --package cognee-cognify --test e2e_lifecycle_loop

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{
    DatabaseConnection, DeleteDb, IngestDb, SearchHistoryDb, connect, initialize, ops,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::EmbeddingEngine;
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
use test_utils::require_env;

const TEST_TEXT: &str = "Alice works at TechCorp in San Francisco as a software engineer.";

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

/// Build a `SearchRequest` with all optional fields set to `None` explicitly.
fn make_request(query: &str, search_type: SearchType) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type,
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
        summarize_context: None,
    }
}

#[tokio::test]
async fn test_readd_and_recognify_after_delete() {
    // ── Environment gates ───────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");

    // ── Infrastructure setup ────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    let Some((embedding_engine, embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        return;
    };
    let embedding_engine: Arc<dyn EmbeddingEngine> = embedding_engine;

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
    let vector_db: Arc<dyn VectorDB> = Arc::new(QdrantAdapter::new(
        temp_dir.path().join("qdrant"),
        embedding_dims,
    ));

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

    // ═════════════════════════════════════════════════════════════════════════
    // FIRST CYCLE: add -> cognify
    // ═════════════════════════════════════════════════════════════════════════

    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));
    let data_items_1 = ingest
        .add(
            vec![DataInput::Text(TEST_TEXT.to_string())],
            "lifecycle_test",
            owner_id,
            None,
        )
        .await
        .expect("first ingest.add");

    assert_eq!(
        data_items_1.len(),
        1,
        "Expected exactly 1 ingested data item in first cycle"
    );
    let original_data_id = data_items_1[0].id;

    let dataset_1 = ops::datasets::get_dataset_by_name(&database, "lifecycle_test", owner_id, None)
        .await
        .expect("get_dataset_by_name after first add")
        .expect("dataset should exist after first add");
    let original_dataset_id = dataset_1.id;

    println!(
        "First cycle: data_id={}, dataset_id={}",
        original_data_id, original_dataset_id
    );

    // Cognify (first cycle)
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    let result_1 = match cognify(
        data_items_1,
        dataset_1.id,
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
            eprintln!("Skipping test: first cognify failed: {}", e);
            return;
        }
    };

    assert!(
        !result_1.chunks.is_empty(),
        "Chunks should be non-empty after first cognify"
    );
    assert!(
        !result_1.entities.is_empty(),
        "Entities should be extracted after first cognify"
    );

    let first_chunk_count = result_1.chunks.len();
    let first_entity_count = result_1.entities.len();

    // Graph should be non-empty after cognify
    let (nodes_before_delete, _edges_before_delete) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data after first cognify");
    assert!(
        !nodes_before_delete.is_empty(),
        "Graph should have nodes after first cognify"
    );

    println!(
        "First cycle complete: {} chunks, {} entities, {} graph nodes",
        first_chunk_count,
        first_entity_count,
        nodes_before_delete.len()
    );

    // ═════════════════════════════════════════════════════════════════════════
    // DELETE
    // ═════════════════════════════════════════════════════════════════════════

    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(graph_db.clone() as Arc<dyn GraphDBTrait>)
            .with_vector_db(vector_db.clone() as Arc<dyn VectorDB>);

    let delete_result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id,
                dataset_name: "lifecycle_test".to_string(),
            },
            mode: DeleteMode::Hard,
            memory_only: false,
        })
        .await
        .expect("delete_svc.execute");

    assert!(
        delete_result.deleted_datasets >= 1,
        "Should have deleted at least 1 dataset; got {}",
        delete_result.deleted_datasets
    );
    assert!(
        delete_result.deleted_data >= 1,
        "Should have deleted at least 1 data item; got {}",
        delete_result.deleted_data
    );

    println!(
        "Delete complete: {} datasets, {} data items removed",
        delete_result.deleted_datasets, delete_result.deleted_data
    );

    // Verify graph is empty after delete
    let (nodes_after_delete, _edges_after_delete) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data after delete");
    assert!(
        nodes_after_delete.is_empty(),
        "Graph should be empty after hard delete; found {} nodes",
        nodes_after_delete.len()
    );

    // Verify dataset no longer exists in DB
    let dataset_after_delete =
        ops::datasets::get_dataset_by_name(&database, "lifecycle_test", owner_id, None)
            .await
            .expect("get_dataset_by_name after delete");
    assert!(
        dataset_after_delete.is_none(),
        "Dataset 'lifecycle_test' should not exist after delete"
    );

    println!("Post-delete assertions passed: graph empty, dataset gone");

    // ═════════════════════════════════════════════════════════════════════════
    // SECOND CYCLE: re-add -> re-cognify -> search
    // ═════════════════════════════════════════════════════════════════════════

    let ingest_2 = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));
    let data_items_2 = ingest_2
        .add(
            vec![DataInput::Text(TEST_TEXT.to_string())],
            "lifecycle_test",
            owner_id,
            None,
        )
        .await
        .expect("second ingest.add");

    assert_eq!(
        data_items_2.len(),
        1,
        "Expected exactly 1 ingested data item in second cycle"
    );
    let readded_data_id = data_items_2[0].id;

    // CRITICAL: UUID5 determinism — same content + same owner = same ID
    assert_eq!(
        original_data_id, readded_data_id,
        "Re-added data should have the same deterministic UUID5 ID: \
         original={}, readded={}",
        original_data_id, readded_data_id
    );

    let dataset_2 = ops::datasets::get_dataset_by_name(&database, "lifecycle_test", owner_id, None)
        .await
        .expect("get_dataset_by_name after second add")
        .expect("dataset should exist after second add");

    // CRITICAL: UUID5 determinism for dataset
    assert_eq!(
        original_dataset_id, dataset_2.id,
        "Re-created dataset should have the same deterministic UUID5 ID: \
         original={}, readded={}",
        original_dataset_id, dataset_2.id
    );

    println!(
        "Second cycle IDs verified: data_id={}, dataset_id={}",
        readded_data_id, dataset_2.id
    );

    // Re-cognify (second cycle)
    let result_2 = match cognify(
        data_items_2,
        dataset_2.id,
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
            panic!(
                "Second cognify should succeed after delete+re-add, but failed: {}",
                e
            );
        }
    };

    assert!(
        !result_2.chunks.is_empty(),
        "Chunks should be non-empty after second cognify (pipeline status was reset)"
    );
    assert!(
        !result_2.entities.is_empty(),
        "Entities should be extracted after second cognify"
    );

    // Deterministic chunking: same text = same chunk count
    assert_eq!(
        result_2.chunks.len(),
        first_chunk_count,
        "Chunk count should match between first and second cognify: \
         first={}, second={}",
        first_chunk_count,
        result_2.chunks.len()
    );

    println!(
        "Second cognify complete: {} chunks, {} entities",
        result_2.chunks.len(),
        result_2.entities.len()
    );

    // Search for the re-cognified data
    let orchestrator = SearchBuilder::new(
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        llm.clone() as Arc<dyn Llm>,
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    let response = orchestrator
        .search(&make_request("Alice TechCorp", SearchType::Chunks))
        .await
        .expect("Chunks search after re-cognify should succeed");

    assert!(
        is_non_empty(&response),
        "Search for 'Alice TechCorp' should return non-empty results after re-cognify"
    );

    println!("Search after re-cognify: non-empty result confirmed");
    println!("test_readd_and_recognify_after_delete PASSED");
}
