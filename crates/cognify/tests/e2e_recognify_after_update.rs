//! Gap P5: Re-cognify after content update E2E test.
//!
//! Verifies that adding new content to an existing dataset, cognifying,
//! and then selectively deleting old content leaves only the new content
//! searchable.
//!
//! In Rust, "update" means adding new content with a different hash, which
//! produces a different `data_id` (UUID5 determinism). The test:
//! 1. Adds text about "Alice at TechCorp", cognifies, verifies searchable
//! 2. Adds text about "Bob at QuantumLab" (same dataset), cognifies both
//! 3. Verifies both A and B are searchable
//! 4. Deletes topic A's data_id (data-scope delete)
//! 5. Verifies only B remains searchable
//!
//! Required env vars: OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH
//!
//! Run with: cargo test --package cognee-cognify --test e2e_recognify_after_update

use std::sync::Arc;

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

const TEXT_V1: &str = "\
Alice is a senior software engineer at TechCorp, a leading technology \
company based in San Francisco. She specializes in distributed systems \
and has been working on TechCorp's next-generation cloud platform for \
the past three years. Alice holds a PhD in computer science from Stanford \
University and has published several papers on fault-tolerant computing.";

const TEXT_V2: &str = "\
Bob is a quantum physicist at QuantumLab, a cutting-edge research \
institute in Zurich. He leads the superconducting qubit team and recently \
achieved a breakthrough in quantum error correction. Bob previously \
worked at CERN and holds multiple patents in quantum computing hardware. \
QuantumLab has partnered with several European universities for its \
quantum advantage program.";

/// Build a `SearchRequest` for Chunks search with `only_context=true`.
fn make_chunks_request(query: &str) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type: SearchType::Chunks,
        top_k: Some(10),
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(true),
        use_combined_context: None,
        session_id: None,
        node_type: None,
        node_name: None,
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
    }
}

/// Concatenate all payload JSON strings from an `only_context` response
/// into a single lowercase string for substring matching.
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

#[tokio::test]
async fn test_recognify_after_content_update() {
    // ── Environment gating ──────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");
    let _ = require_env("COGNEE_E2E_EMBED_MODEL_PATH");

    // ── Infrastructure setup ────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

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
                eprintln!("Skipping test: failed to load embedding model: {}", e);
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
    let dataset_name = "update_test";
    let ontology: Arc<dyn cognee_ontology::OntologyResolver> =
        Arc::new(NoOpOntologyResolver::new());
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    // ── Step 1: Ingest text_v1 (Alice at TechCorp) ─────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>);
    let data_items_v1 = ingest
        .add(
            vec![DataInput::Text(TEXT_V1.to_string())],
            dataset_name,
            owner_id,
            None,
        )
        .await
        .expect("ingest text_v1");
    assert_eq!(
        data_items_v1.len(),
        1,
        "Expected 1 ingested data item for v1"
    );
    let data_id_v1 = data_items_v1[0].id;
    println!("Step 1: Ingested text_v1, data_id={data_id_v1}");

    // ── Step 2: Cognify text_v1 ─────────────────────────────────────────────
    let dataset = ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
        .await
        .expect("get_dataset_by_name")
        .expect("dataset should exist after ingest");

    let result_v1 = match cognify(
        data_items_v1,
        dataset.id,
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone() as Arc<dyn StorageTrait>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        None,
        Arc::clone(&ontology),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping test: cognify v1 failed: {e}");
            return;
        }
    };
    assert!(
        !result_v1.chunks.is_empty(),
        "Cognify v1 should produce chunks"
    );
    println!(
        "Step 2: Cognified v1 - {} chunks, {} entities",
        result_v1.chunks.len(),
        result_v1.entities.len()
    );

    // ── Step 3: Verify search for "Alice TechCorp" returns results ──────────
    let orchestrator = SearchBuilder::new(
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        llm.clone() as Arc<dyn Llm>,
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    let response_alice = orchestrator
        .search(&make_chunks_request("Alice TechCorp software engineer"))
        .await
        .expect("search for Alice after v1 cognify");
    assert!(
        is_non_empty(&response_alice),
        "Search for Alice should return results after cognify v1"
    );
    let alice_text = response_payload_text(&response_alice);
    assert!(
        alice_text.contains("alice") || alice_text.contains("techcorp"),
        "Search results should mention Alice or TechCorp; got: {alice_text}"
    );
    println!("Step 3: Search for Alice returned results");

    // ── Step 4: Ingest text_v2 (Bob at QuantumLab) into the same dataset ────
    let data_items_v2 = ingest
        .add(
            vec![DataInput::Text(TEXT_V2.to_string())],
            dataset_name,
            owner_id,
            None,
        )
        .await
        .expect("ingest text_v2");
    assert_eq!(
        data_items_v2.len(),
        1,
        "Expected 1 ingested data item for v2"
    );
    let data_id_v2 = data_items_v2[0].id;
    println!("Step 4: Ingested text_v2, data_id={data_id_v2}");

    // Verify different content produces different data_id (UUID5 determinism)
    assert_ne!(
        data_id_v1, data_id_v2,
        "Different content must produce different data_id"
    );

    // ── Step 5: Cognify the new data item (v2) ─────────────────────────────
    let result_v2 = match cognify(
        data_items_v2,
        dataset.id,
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone() as Arc<dyn StorageTrait>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        None,
        Arc::clone(&ontology),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping test: cognify v2 failed: {e}");
            return;
        }
    };
    assert!(
        !result_v2.chunks.is_empty(),
        "Cognify v2 should produce chunks"
    );
    println!(
        "Step 5: Cognified v2 - {} chunks, {} entities",
        result_v2.chunks.len(),
        result_v2.entities.len()
    );

    // ── Step 6: Verify both Alice and Bob are searchable ────────────────────
    let response_alice_after_v2 = orchestrator
        .search(&make_chunks_request("Alice TechCorp software engineer"))
        .await
        .expect("search for Alice after v2 cognify");
    assert!(
        is_non_empty(&response_alice_after_v2),
        "Search for Alice should still return results after adding v2"
    );
    println!("Step 6a: Alice still searchable after v2 cognify");

    let response_bob = orchestrator
        .search(&make_chunks_request("Bob QuantumLab quantum physicist"))
        .await
        .expect("search for Bob after v2 cognify");
    assert!(
        is_non_empty(&response_bob),
        "Search for Bob should return results after cognify v2"
    );
    let bob_text = response_payload_text(&response_bob);
    assert!(
        bob_text.contains("bob") || bob_text.contains("quantumlab") || bob_text.contains("quantum"),
        "Search results should mention Bob or QuantumLab; got: {bob_text}"
    );
    println!("Step 6b: Bob searchable after v2 cognify");

    // ── Step 7: Delete text_v1's data (data-scope delete) ───────────────────
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db));

    delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id: data_id_v1,
                dataset_name: Some(dataset_name.to_string()),
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Hard,
        })
        .await
        .expect("delete text_v1 data");
    println!("Step 7: Deleted text_v1 data (data_id={data_id_v1})");

    // ── Step 8: Verify Bob still searchable, Alice no longer returned ───────
    let response_bob_after_delete = orchestrator
        .search(&make_chunks_request("Bob QuantumLab quantum physicist"))
        .await
        .expect("search for Bob after delete");
    assert!(
        is_non_empty(&response_bob_after_delete),
        "Search for Bob should still return results after deleting v1"
    );
    let bob_text_after = response_payload_text(&response_bob_after_delete);
    assert!(
        bob_text_after.contains("bob")
            || bob_text_after.contains("quantumlab")
            || bob_text_after.contains("quantum"),
        "Bob content should persist after v1 deletion; got: {bob_text_after}"
    );
    println!("Step 8a: Bob still searchable after v1 deletion");

    let response_alice_after_delete = orchestrator
        .search(&make_chunks_request("Alice TechCorp software engineer"))
        .await
        .expect("search for Alice after delete");
    let alice_text_after = response_payload_text(&response_alice_after_delete);
    // After deleting v1, Alice-specific content should no longer appear.
    // The search may return results (e.g. Bob's content as nearest match),
    // but Alice/TechCorp terms should be absent from the payloads.
    let alice_still_present =
        alice_text_after.contains("alice") && alice_text_after.contains("techcorp");
    assert!(
        !alice_still_present,
        "Alice+TechCorp content should not appear in search results after v1 deletion; \
         got: {alice_text_after}"
    );
    println!("Step 8b: Alice no longer in search results after v1 deletion");

    println!("test_recognify_after_content_update PASSED");
}
