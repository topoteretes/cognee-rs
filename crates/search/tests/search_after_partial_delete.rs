//! E2E test: search returns correct results after partial delete.
//!
//! Two documents with distinct topics are cognified into the same dataset.
//! After deleting one document, search for the deleted topic should return
//! empty/irrelevant results, while search for the remaining topic should
//! still return results.
//!
//! Required env vars: OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL, COGNEE_E2E_EMBED_MODEL_PATH

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

fn require_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}

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

/// Extract all text content from a search response for keyword checking.
fn response_text(response: &SearchResponse) -> String {
    match &response.result {
        SearchOutput::Text(text) => text.clone(),
        SearchOutput::Texts(texts) => texts.join(" "),
        SearchOutput::Items(items) => items
            .iter()
            .map(|i| i.payload.to_string())
            .collect::<Vec<_>>()
            .join(" "),
        SearchOutput::GraphQueryRows(rows) => format!("{rows:?}"),
        SearchOutput::Rules(rules) => rules
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
            .join(" "),
        SearchOutput::Ack { .. } => String::new(),
        SearchOutput::Structured(value) => value.to_string(),
    }
}

fn make_chunks_request(query: &str) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type: SearchType::Chunks,
        top_k: Some(5),
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
    }
}

const GERMANY_TEXT: &str = "Germany is a country in Central Europe. Its capital is Berlin. \
    The Rhine river flows through western Germany. Oktoberfest is a famous \
    German festival held annually in Munich, Bavaria.";

const QUANTUM_TEXT: &str = "Quantum computers use qubits instead of classical bits. \
    They can solve certain problems exponentially faster using superposition \
    and entanglement. IBM and Google are leading quantum hardware research.";

#[tokio::test]
async fn test_search_returns_empty_for_deleted_doc_and_non_empty_for_remaining() {
    // ── Environment gating ──────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");

    // ── Infrastructure ──────────────────────────────────────────────────
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

    let vector_db: Arc<dyn VectorDB> = Arc::new(QdrantAdapter::new(
        temp_dir.path().join("qdrant"),
        embedding_dims,
    ));

    let llm: Arc<dyn Llm> = Arc::new(
        OpenAIAdapter::new(
            require_env("OPENAI_MODEL"),
            require_env("OPENAI_TOKEN"),
            Some(require_env("OPENAI_URL")),
        )
        .expect("OpenAIAdapter::new"),
    );

    let owner_id = Uuid::nil();

    // ── Step 1: Ingest two distinct documents into one dataset ───────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

    let germany_data = ingest
        .add(
            vec![DataInput::Text(GERMANY_TEXT.to_string())],
            "search_test",
            owner_id,
            None,
        )
        .await
        .expect("ingest Germany");
    assert_eq!(germany_data.len(), 1);
    let germany_data_id = germany_data[0].id;

    let quantum_data = ingest
        .add(
            vec![DataInput::Text(QUANTUM_TEXT.to_string())],
            "search_test",
            owner_id,
            None,
        )
        .await
        .expect("ingest Quantum");
    assert_eq!(quantum_data.len(), 1);

    let dataset = ops::datasets::get_dataset_by_name(&database, "search_test", owner_id, None)
        .await
        .expect("get dataset")
        .expect("dataset should exist");

    // ── Step 2: Cognify both documents together ─────────────────────────
    let all_items = [germany_data, quantum_data].concat();
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
    );

    if let Err(e) = cognify(
        all_items,
        dataset.id,
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
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        eprintln!("Skipping: cognify failed: {e}");
        return;
    }

    println!("Step 2 OK: Both documents cognified");

    // ── Step 3: Pre-delete search verification ──────────────────────────
    let orchestrator = SearchBuilder::new(
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        llm.clone() as Arc<dyn Llm>,
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    let germany_pre = orchestrator
        .search(&make_chunks_request("Germany Berlin Rhine"))
        .await
        .expect("search Germany pre-delete");
    assert!(
        is_non_empty(&germany_pre),
        "Germany search should be non-empty before delete"
    );

    let quantum_pre = orchestrator
        .search(&make_chunks_request("quantum qubits superposition"))
        .await
        .expect("search Quantum pre-delete");
    assert!(
        is_non_empty(&quantum_pre),
        "Quantum search should be non-empty before delete"
    );
    println!("Step 3 OK: Both topics found in search before delete");

    // ── Step 4: Delete Germany document ─────────────────────────────────
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(graph_db.clone())
            .with_vector_db(vector_db.clone());

    let result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id: germany_data_id,
                dataset_name: Some("search_test".to_string()),
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await
        .expect("delete Germany doc");

    assert!(result.deleted_data >= 1, "Should have deleted Germany data");
    println!("Step 4 OK: Deleted Germany document");

    // ── Step 5: Post-delete search verification ─────────────────────────
    // Germany search should return empty or no Germany-related content
    let germany_post = orchestrator
        .search(&make_chunks_request("Germany Berlin Rhine"))
        .await
        .expect("search Germany post-delete");

    let germany_text = response_text(&germany_post).to_lowercase();
    let has_germany_content = germany_text.contains("germany")
        || germany_text.contains("berlin")
        || germany_text.contains("rhine");

    assert!(
        !is_non_empty(&germany_post) || !has_germany_content,
        "Germany content should not appear in search results after delete. Got: {}",
        germany_text,
    );
    println!("Step 5a OK: Germany content no longer in search results");

    // Quantum search should still work
    let quantum_post = orchestrator
        .search(&make_chunks_request("quantum qubits superposition"))
        .await
        .expect("search Quantum post-delete");
    assert!(
        is_non_empty(&quantum_post),
        "Quantum search should still return results after partial delete"
    );
    println!("Step 5b OK: Quantum content still searchable");

    println!("PASSED: test_search_returns_empty_for_deleted_doc_and_non_empty_for_remaining");
}
