#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `topological_rank` parity coverage for the cognify pipeline — **offline**.
//!
//! `DataPoint.topological_rank` is the 1-based index of the pipeline stage that
//! created a node; the visualization's Story / Flow layouts use it as the
//! column number, so a wrong value silently mis-renders the graph. Python
//! derives it from a **deduplicated** task-name sequence,
//! `task_sequence.index(task_name) + 1`
//! (`cognee/modules/pipelines/operations/run_tasks_base.py:181-190`), over the
//! default task list in `cognee/api/v1/cognify/cognify.py:350-375`:
//!
//! | Python task                    | index | Rust task(s)                                |
//! |--------------------------------|-------|---------------------------------------------|
//! | `classify_documents`           | 1     | `classify_documents`                        |
//! | `extract_chunks_from_documents`| 2     | `extract_chunks_from_documents`             |
//! | `extract_graph_and_summarize`  | 3     | `extract_graph_from_data` + `summarize_text` |
//! | `add_data_points`              | 4     | `add_data_points`                           |
//! | `extract_dlt_fk_edges`         | 5     | post-pipeline teardown (emits no DataPoints) |
//!
//! Rust splits Python's fused stage 3 in two, so **both halves carry rank 3**
//! and `add_data_points` keeps Python's 4 — otherwise the same node type would
//! land in a different column in each SDK.
//!
//! The expected values below are written as literals derived from that Python
//! table, never from the Rust constants or from `source_task`, so the
//! assertions cannot be satisfied by the stamping code agreeing with itself.
//!
//! Everything here runs against `MockLlm` / `MockStorage` / `MockGraphDB` /
//! `MockVectorDB` / `MockEmbeddingEngine`: no network, no LLM key, no skip
//! path. Rank coverage must never depend on a live LLM (the E2E companion in
//! `provenance_e2e.rs` does, and therefore cannot carry this).

use std::sync::Arc;

use cognee_cognify::tasks::{
    ADD_DATA_POINTS_TASK_RANK, CLASSIFY_DOCUMENTS_TASK_RANK, EXTRACT_CHUNKS_TASK_RANK,
    EXTRACT_GRAPH_TASK_RANK, SUMMARIZE_TEXT_TASK_RANK, make_extract_graph_task_with_rank,
};
use cognee_cognify::{CognifyConfig, ExtractedChunks, ExtractedGraphData, cognify};
use cognee_core::Task;
use cognee_core::task::Value;
use cognee_database::ops::datasets::create_dataset;
use cognee_database::{DatabaseConnection, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_models::{Data, Dataset, DocumentChunk};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_vector::VectorDB;
use serde_json::json;
use uuid::Uuid;

// ── Python-observed ranks (see the module table) ───────────────────────────

/// `classify_documents` — Python `task_sequence` slot 1.
const PY_DOCUMENT_RANK: i32 = 1;
/// `extract_chunks_from_documents` — Python `task_sequence` slot 2.
const PY_CHUNK_RANK: i32 = 2;
/// `extract_graph_and_summarize` — Python `task_sequence` slot 3.
const PY_ENTITY_RANK: i32 = 3;
/// Same fused Python task as [`PY_ENTITY_RANK`], hence the same number.
const PY_SUMMARY_RANK: i32 = 3;
/// `add_data_points` — Python `task_sequence` slot 4 (not 5: the fused
/// graph+summarize task occupies only one slot).
const PY_ADD_DATA_POINTS_RANK: i32 = 4;
/// Python's `create_edge_type_datapoints` (`index_graph_edges.py:50`) builds
/// `EdgeType` objects that never reach a provenance stamper, so they keep the
/// `DataPoint` default — the `0` sentinel.
const PY_EDGE_TYPE_RANK: i32 = 0;

/// The exported constants must equal the Python-observed values above.
///
/// A pure drift guard: it does not exercise the stamping code (the pipeline
/// test below does), it pins the numbers so a "renumber the pipeline" change
/// cannot pass silently.
#[test]
fn rank_constants_match_python_task_sequence() {
    assert_eq!(CLASSIFY_DOCUMENTS_TASK_RANK, PY_DOCUMENT_RANK);
    assert_eq!(EXTRACT_CHUNKS_TASK_RANK, PY_CHUNK_RANK);
    assert_eq!(EXTRACT_GRAPH_TASK_RANK, PY_ENTITY_RANK);
    assert_eq!(SUMMARIZE_TEXT_TASK_RANK, PY_SUMMARY_RANK);
    assert_eq!(ADD_DATA_POINTS_TASK_RANK, PY_ADD_DATA_POINTS_RANK);
    assert_eq!(
        EXTRACT_GRAPH_TASK_RANK, SUMMARIZE_TEXT_TASK_RANK,
        "Python fuses graph extraction and summarization into one task, so both \
         Rust halves must share its slot"
    );
}

/// One chunk of prose, small enough to produce exactly one chunk (and hence
/// exactly one graph-extraction + one summarization LLM call).
const FIXTURE_TEXT: &str = "Alice works at Acme in Berlin.";

/// A canned knowledge graph with two entities and one relationship, so the run
/// produces `Entity`, `EntityType` and `EdgeType` DataPoints to assert on.
fn canned_graph_response() -> String {
    json!({
        "nodes": [
            {"id": "alice", "name": "Alice", "type": "PERSON", "description": "A person."},
            {"id": "acme", "name": "Acme", "type": "ORGANIZATION", "description": "A company."}
        ],
        "edges": [{
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "works_at",
            "description": "Alice works at Acme."
        }]
    })
    .to_string()
}

#[tokio::test]
async fn cognify_stamps_the_python_topological_ranks() {
    // FIFO: graph extraction first, then summarization.
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::new(vec![
        canned_graph_response(),
        json!({"summary": "Alice and Acme.", "description": "A short description."}).to_string(),
    ]));

    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
    let embedding_engine: Arc<dyn cognee_embedding::EmbeddingEngine> =
        Arc::new(MockEmbeddingEngine::new(8));

    let data_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let location = storage
        .store(FIXTURE_TEXT.as_bytes(), &format!("rank-{data_id}"))
        .await
        .expect("MockStorage::store");
    let data_item = Data::builder(
        data_id,
        "rank.txt",
        location,
        "rank.txt",
        "txt",
        "text/plain",
        "test-hash-rank",
        owner_id,
    )
    .build();

    let dataset_id = Uuid::new_v4();
    let db: Arc<DatabaseConnection> = {
        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        // The ledger is written on every run now, and its rows carry an FK to
        // `datasets` — so the dataset must be registered even for a run with no
        // user. (`data_id` has no FK, so no `data` row is needed.)
        create_dataset(
            &conn,
            Dataset::new("rank-parity".into(), owner_id, None, dataset_id),
        )
        .await
        .expect("seed dataset");
        Arc::new(conn)
    };
    let thread_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool"));

    let result = cognify(
        vec![data_item],
        dataset_id,
        // No user / tenant: the ownership rows resolve to the default ledger
        // user. `source_user` is covered by the LLM-gated E2E test instead; the
        // ranks under test here are user-independent.
        None,
        None,
        None,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        db,
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &CognifyConfig::default(),
    )
    .await
    .expect("cognify must succeed against the mock backends");

    // ── Documents (classify_documents) ────────────────────────────────────
    assert!(
        !result.documents_for_dlt.is_empty(),
        "the run must surface at least one Document to assert on"
    );
    for doc in &result.documents_for_dlt {
        assert_eq!(
            doc.base.topological_rank,
            Some(PY_DOCUMENT_RANK),
            "Document '{}' is created by classify_documents (Python slot 1)",
            doc.name
        );
        assert_eq!(
            doc.base.source_pipeline.as_deref(),
            Some("cognify_pipeline"),
            "source_pipeline must match Python's pipeline_name=\"cognify_pipeline\""
        );
    }

    // ── DocumentChunks (extract_chunks_from_documents) ────────────────────
    assert!(!result.chunks.is_empty(), "expected at least one chunk");
    for chunk in &result.chunks {
        assert_eq!(
            chunk.base.topological_rank,
            Some(PY_CHUNK_RANK),
            "DocumentChunk is created by extract_chunks_from_documents (Python slot 2)"
        );
    }

    // ── Entities / EntityTypes (extract_graph_from_data) ──────────────────
    assert!(
        !result.entities.is_empty(),
        "the canned knowledge graph must yield entities"
    );
    for pair in &result.entities {
        assert_eq!(
            pair.entity.base.topological_rank,
            Some(PY_ENTITY_RANK),
            "Entity '{}' comes from Python's fused extract_graph_and_summarize (slot 3)",
            pair.entity.name
        );
        assert_eq!(
            pair.entity_type.base.topological_rank,
            Some(PY_ENTITY_RANK),
            "EntityType '{}' comes from the same fused Python task (slot 3)",
            pair.entity_type.name
        );
    }

    // ── TextSummaries (summarize_text) ────────────────────────────────────
    assert!(
        !result.summaries.is_empty(),
        "summarization is enabled by default; expected at least one summary"
    );
    for summary in &result.summaries {
        assert_eq!(
            summary.base.topological_rank,
            Some(PY_SUMMARY_RANK),
            "TextSummary shares Python's fused stage 3 with graph extraction — it \
             must NOT be numbered 4"
        );
    }

    // ── EdgeTypes (never stamped by Python) ───────────────────────────────
    assert!(
        !result.edge_types.is_empty(),
        "the canned edge must yield one EdgeType"
    );
    for edge_type in &result.edge_types {
        assert_eq!(
            edge_type.base.topological_rank,
            Some(PY_EDGE_TYPE_RANK),
            "EdgeType '{}' is never handed to a provenance stamper in Python \
             (index_graph_edges.py:50), so its rank stays at the 0 sentinel",
            edge_type.relationship_name
        );
    }
}

/// A custom pipeline can move `extract_graph_from_data` to another position
/// and get the right rank on Entity / EntityType.
///
/// This is the hazard the fixed factories close: the rank is stamped in-body
/// (and, for entities, deep inside `expand_with_nodes_and_edges` *before* the
/// nodes are persisted), so without an override an embedder placing the stage
/// second would silently emit the default pipeline's rank 3.
#[tokio::test]
async fn extract_graph_rank_is_overridable_for_custom_pipelines() {
    /// The position a custom pipeline puts graph extraction at — deliberately
    /// different from [`EXTRACT_GRAPH_TASK_RANK`].
    const CUSTOM_RANK: i32 = 7;

    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::new(vec![canned_graph_response()]));
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());

    let doc_id = Uuid::new_v4();
    let input = ExtractedChunks {
        chunks: vec![DocumentChunk::new(
            Uuid::new_v4(),
            FIXTURE_TEXT.to_string(),
            FIXTURE_TEXT.split_whitespace().count(),
            0,
            "paragraph_end".to_string(),
            doc_id,
        )],
        documents: vec![],
        dataset_id: Uuid::new_v4(),
        user_id: None,
        tenant_id: None,
        failures: Default::default(),
    };

    let (_handle, ctx, db) = cognee_test_utils::test_task_context().await;
    // The stage records ownership of the entities it is about to write, and
    // those rows carry an FK to `datasets`. `test_task_context()` supplies no
    // `pipeline_ctx`, so this also covers the out-of-executor case: the rows
    // land with a NULL run id rather than erroring.
    create_dataset(
        &db,
        Dataset::new("rank-task".into(), Uuid::new_v4(), None, input.dataset_id),
    )
    .await
    .expect("seed dataset");

    let task = Task::from(make_extract_graph_task_with_rank(
        llm,
        graph_db,
        Arc::new(NoOpOntologyResolver::new()),
        Arc::clone(&db),
        // Web-page node creation needs Documents, which this input omits.
        CognifyConfig::default().with_web_page_nodes(false),
        CUSTOM_RANK,
    ));

    let output = match task.call(Arc::new(input) as Arc<dyn Value>, ctx) {
        cognee_core::task::TaskCall::Async(fut) => fut.await.expect("extract_graph task"),
        _ => panic!("make_extract_graph_task must produce an async task"),
    };
    let graph_data = (*output)
        .as_any()
        .downcast_ref::<ExtractedGraphData>()
        .expect("output is ExtractedGraphData");

    assert!(
        !graph_data.entities.is_empty(),
        "the canned knowledge graph must yield entities"
    );
    for pair in &graph_data.entities {
        assert_eq!(
            pair.entity.base.topological_rank,
            Some(CUSTOM_RANK),
            "Entity '{}' must carry the caller-supplied rank, not the default \
             pipeline's {EXTRACT_GRAPH_TASK_RANK}",
            pair.entity.name
        );
        assert_eq!(
            pair.entity_type.base.topological_rank,
            Some(CUSTOM_RANK),
            "EntityType '{}' must carry the caller-supplied rank",
            pair.entity_type.name
        );
    }
}
