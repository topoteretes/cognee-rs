#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression test: TextSummary vector payload must include the "text" field.
//!
//! Verifies that when the cognify pipeline indexes TextSummary data points into
//! the vector DB, each point's metadata contains the actual summary text under
//! the `"text"` key. Without this field, `SearchType::SUMMARIES` returns IDs
//! but no human-readable content, breaking parity with the Python SDK.
//!
//! This test is fully offline and deterministic: it uses MockLlm, MockStorage,
//! MockGraphDB, MockVectorDB, and MockEmbeddingEngine so it never touches the
//! network or filesystem.
//!
//! Run with:
//!   cargo test --package cognee-cognify --test integration_text_summary_payload

use std::sync::Arc;

use cognee_ontology::NoOpOntologyResolver;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_models::Data;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_vector::VectorDB;
use serde_json::json;
use uuid::Uuid;

/// The exact summary string seeded into the MockLlm.  Every TextSummary point
/// in the vector DB must carry this value in its `metadata["text"]`.
const EXPECTED_SUMMARY: &str = "Concise NLP summary about language and computers.";

#[tokio::test]
async fn text_summary_payload_contains_text_field() {
    // -- Mock LLM with two FIFO responses ----------------------------------------
    //
    // 1. Empty knowledge graph  (consumed by the graph-extraction stage)
    // 2. Summarization response (consumed by the summarize-text stage)
    let mock_llm = MockLlm::new(vec![
        // Response 1: graph extraction -> empty graph. Field is `edges`, not the
        // ignored `relationships`: since #83 dropped `#[serde(default)]` from
        // `KnowledgeGraph.edges`, it is a required field and a payload missing it
        // fails typed deserialization ("missing field `edges`").
        json!({"nodes": [], "edges": []}).to_string(),
        // Response 2: summarization -> deterministic summary
        json!({
            "summary": EXPECTED_SUMMARY,
            "description": "Detailed description of natural language processing topics."
        })
        .to_string(),
    ]);
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(mock_llm);

    // -- Other mocks --------------------------------------------------------------
    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());
    let embedding_engine: Arc<dyn cognee_embedding::EmbeddingEngine> =
        Arc::new(MockEmbeddingEngine::new(8));

    // -- Prepare a single Data item -----------------------------------------------
    let text = "Natural language processing helps computers understand human language.";
    let data_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = format!("test-summary-{data_id}");

    let stored_location = storage
        .store(text.as_bytes(), &location)
        .await
        .expect("MockStorage::store should not fail");

    let data_item = Data::builder(
        data_id,
        "nlp-doc.txt",
        stored_location,
        "nlp-doc.txt",
        "txt",
        "text/plain",
        "test-hash-nlp",
        owner_id,
    )
    .build();

    // -- Run the cognify pipeline -------------------------------------------------
    let dataset_id = Uuid::new_v4();
    let config = CognifyConfig::default(); // enable_summarization defaults to true

    let db: Arc<DatabaseConnection> = {
        let conn = connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        initialize(&conn).await.expect("initialize");
        Arc::new(conn)
    };
    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
    );

    let result = cognify(
        vec![data_item],
        dataset_id,
        None,
        None,
        None,
        llm,
        storage,
        graph_db,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine,
        db,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    .expect("cognify pipeline should succeed with mock backends");

    // -- Assertion 1: summaries are non-empty -------------------------------------
    assert!(
        !result.summaries.is_empty(),
        "cognify result should contain at least one TextSummary (enable_summarization=true)"
    );

    // -- Assertion 2: TextSummary:text collection exists and has points -----------
    assert!(
        vector_db
            .has_collection("TextSummary", "text")
            .await
            .unwrap(),
        "TextSummary:text collection must exist in the vector DB after cognify"
    );

    let collection_size = vector_db
        .collection_size("TextSummary", "text")
        .await
        .unwrap();
    assert!(
        collection_size > 0,
        "TextSummary:text collection must contain at least one point, but was empty"
    );

    // -- Assertion 3: every point carries the summary text in metadata["text"] ----
    let query = vec![0.0_f32; 8];
    let hits = vector_db
        .search_similar("TextSummary", "text", &query, 10)
        .await
        .unwrap();

    assert!(
        !hits.is_empty(),
        "search_similar on TextSummary:text must return at least one hit"
    );

    for (i, hit) in hits.iter().enumerate() {
        let text_value = hit.metadata.get("text").unwrap_or_else(|| {
            panic!(
                "TextSummary point [{}] (id={}) is missing the 'text' metadata key. \
                 Full metadata: {:?}. \
                 This means the summary text was not persisted in the vector payload.",
                i, hit.id, hit.metadata
            )
        });

        let text_str = text_value.as_str().unwrap_or_else(|| {
            panic!(
                "TextSummary point [{}] (id={}) has 'text' metadata but it is not a string: {:?}",
                i, hit.id, text_value
            )
        });

        assert!(
            !text_str.is_empty(),
            "TextSummary point [{}] (id={}) has an empty 'text' metadata value",
            i,
            hit.id
        );

        assert_eq!(
            text_str, EXPECTED_SUMMARY,
            "TextSummary point [{}] (id={}) has unexpected 'text' value.\n\
             Expected: {:?}\n\
             Actual:   {:?}\n\
             Full metadata: {:?}",
            i, hit.id, EXPECTED_SUMMARY, text_str, hit.metadata
        );
    }

    println!(
        "All {} TextSummary point(s) carry the correct 'text' payload: {:?}",
        hits.len(),
        EXPECTED_SUMMARY
    );
}
