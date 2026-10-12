#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! End-to-end tests for `improve()` orchestrator.
//!
//! These tests exercise the stage-gate logic (which stages run based on the
//! presence of `session_ids` and available backends) using mock
//! storage/graph/vector/embedding/LLM backends. They do NOT exercise the full
//! LLM-driven cognify pipeline — that is covered by the per-stage integration
//! tests in `cognee-cognify`.

use std::sync::Arc;

use async_trait::async_trait;
use cognee::api::improve::{ImproveParams, improve};
use cognee::models::Dataset;
use cognee_cognify::CognifyConfig;
use cognee_database::{DatabaseConnection, IngestDb, SeaOrmCheckpointStore, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_ingestion::AddPipeline;
use cognee_llm::{GenerationOptions, GenerationResponse, LlmError, Message};
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockLlm;
use cognee_vector::MockVectorDB;
use serde_json::Value;
use tempfile::TempDir;
use uuid::Uuid;

/// In-test LLM double that dispatches on the request's JSON schema instead of a
/// FIFO queue, so the full distill → publish → **cognify** path completes
/// deterministically under the mock backends. Cloned verbatim from the pattern
/// in `crates/cognify/tests/memify_distill_sessions.rs`: the curator proposes
/// one lesson, the writer accepts it, the summarizer returns a summary, and
/// graph extraction returns an empty graph. The queue-based [`MockLlm`] cannot
/// satisfy cognify's summary call in a fixed position, so it can only be used
/// where cognify is *expected* to fail open — the distill opt-in test needs the
/// end-to-end publish to succeed, hence this schema dispatcher.
#[derive(Clone)]
struct SchemaDispatchLlm {
    /// Statement the writer emits when it accepts (must be non-empty to publish).
    accepted_statement: String,
}

#[async_trait]
impl cognee_llm::Llm for SchemaDispatchLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> Result<GenerationResponse, LlmError> {
        Ok(GenerationResponse {
            content: "ok".to_string(),
            model: "schema-dispatch".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> Result<Value, LlmError> {
        let schema = json_schema.to_string();
        if schema.contains("why_learned") {
            // Writer/rejecter (`WrittenLesson`) → accept + write.
            Ok(serde_json::json!({
                "accept": true,
                "statement": self.accepted_statement,
                "entities": ["TerraScout"],
                "why_learned": "learned while asking how indexing works",
            }))
        } else if schema.contains("working_statement") {
            // Curator (`CuratorBatchOutput`) → one proposed lesson.
            Ok(serde_json::json!({
                "lessons": [{"working_statement": "TerraScout indexes nightly.", "member_entry_ids": []}]
            }))
        } else if schema.contains("summary") {
            // Cognify summarization (`SummarizedContent`).
            Ok(
                serde_json::json!({"summary": "A test summary.", "description": "A test description."}),
            )
        } else {
            // Cognify graph extraction → empty graph.
            Ok(serde_json::json!({"nodes": [], "edges": []}))
        }
    }

    fn model(&self) -> &str {
        "schema-dispatch"
    }
}

struct Harness {
    _temp: TempDir,
    _sess_dir: TempDir,
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn StorageTrait>,
    add_pipeline: AddPipeline,
    graph_db: Arc<MockGraphDB>,
    vector_db: Arc<MockVectorDB>,
    embedding_engine: Arc<MockEmbeddingEngine>,
    ontology: Arc<dyn OntologyResolver>,
    session_store: Arc<dyn SessionStore>,
    session_manager: Arc<SessionManager>,
    checkpoint_store: Arc<SeaOrmCheckpointStore>,
}

async fn make_harness() -> Harness {
    let temp = TempDir::new().unwrap();
    let sess_dir = TempDir::new().unwrap();
    let db_path = temp.path().join("cognee.db");
    std::fs::File::create(&db_path).unwrap();
    let url = format!("sqlite://{}", db_path.display());
    let db = connect(&url).await.unwrap();
    initialize(&db).await.unwrap();
    let db = Arc::new(db);
    let storage: Arc<dyn StorageTrait> = Arc::new(LocalStorage::new(temp.path().join("storage")));
    storage.initialize().await.unwrap();

    let ingest_db: Arc<dyn IngestDb> = db.clone();
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());
    let add_pipeline = AddPipeline::new(Arc::clone(&storage), ingest_db)
        .with_thread_pool(Arc::new(
            cognee::core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(graph_db.clone() as Arc<dyn cognee_graph::GraphDBTrait>)
        .with_vector_db(vector_db.clone() as Arc<dyn cognee_vector::VectorDB>)
        .with_database(db.clone());
    let embedding_engine = Arc::new(MockEmbeddingEngine::new(16));
    let ontology: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());

    let session_store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(sess_dir.path()));
    let session_manager = Arc::new(SessionManager::new(Arc::clone(&session_store)));

    let checkpoint_store = Arc::new(SeaOrmCheckpointStore::new(Arc::clone(&db)));

    Harness {
        _temp: temp,
        _sess_dir: sess_dir,
        db,
        storage,
        add_pipeline,
        graph_db,
        vector_db,
        embedding_engine,
        ontology,
        session_store,
        session_manager,
        checkpoint_store,
    }
}

#[tokio::test]
async fn improve_without_sessions_runs_only_memify() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let r = improve(ImproveParams {
        dataset_name: "ds_memify".to_string(),
        session_ids: None,
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert_eq!(r.stages_run, vec!["memify".to_string()]);
    assert!(r.memify_result.is_some());
}

#[tokio::test]
async fn improve_skips_stage1_when_session_backends_missing() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    // Provide session_ids but no session_store/manager — stages 1, 2, 4
    // should all be skipped (with warnings), Stage 3 still runs.
    // Stage 2b (persist_trace_steps) is gated on `has_sessions` so its name IS
    // pushed to stages_run even when it skips/no-ops due to missing backends,
    // matching Python's convention of recording every attempted stage.
    let r = improve(ImproveParams {
        dataset_name: "ds_nosess".to_string(),
        session_ids: Some(vec!["s1".to_string()]),
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: None,
        session_manager: None,
        add_pipeline: None,
        checkpoint_store: None,
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert_eq!(
        r.stages_run,
        vec!["persist_trace_steps".to_string(), "memify".to_string()],
        "with sessions, persist_trace_steps is always recorded even when backends are missing"
    );
}

/// Stage 2c is default-off in the "no lesson" sense: a session with no Q&A
/// gates at `NoQaEntries`, so `distill_sessions` is NOT pushed onto
/// `stages_run` (Python parity: pushed only when a lesson is published). This
/// proves the zero-change guarantee — providing a session that yields nothing
/// does not add the stage.
#[tokio::test]
async fn improve_omits_distill_sessions_when_no_qa() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    // Session id with no Q&A entries → persist skips, distill gates NoQaEntries.
    let r = improve(ImproveParams {
        dataset_name: "ds_noqa".to_string(),
        session_ids: Some(vec!["empty_session".to_string()]),
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert!(
        !r.stages_run.contains(&"distill_sessions".to_string()),
        "distill_sessions must be absent when the session has no Q&A; got {:?}",
        r.stages_run
    );
    assert_eq!(r.sessions_distilled, 0);
    assert_eq!(r.lessons_published, 0);
    // The always-run stages are still present (proving the pipeline itself ran).
    assert!(r.stages_run.contains(&"persist_trace_steps".to_string()));
    assert!(r.stages_run.contains(&"memify".to_string()));
}

/// Stage 2d (`build_truth_subspace`) is opt-in. With sessions present, the flag
/// set, and the dataset resolvable, the stage runs and records its name — the
/// true branch that no other test in this file exercises. `build_truth_subspace`
/// is infallible and runs even against an empty graph, so an empty session +
/// `MockLlm::empty` is sufficient to drive it. Mirrors Python's
/// `build_truth_subspace` opt-in gate (improve.py:207-221).
#[tokio::test]
async fn improve_records_build_truth_subspace_when_opted_in() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    // Stage 2d resolves the dataset by name (Ok(Some(ds)) is required to run);
    // create it directly so the only variable under test is the opt-in flag.
    cognee_database::ops::datasets::create_dataset(
        h.db.as_ref(),
        Dataset::new("ds_truth".to_string(), owner, None, Uuid::new_v4()),
    )
    .await
    .unwrap();

    let r = improve(ImproveParams {
        dataset_name: "ds_truth".to_string(),
        session_ids: Some(vec!["truth_optin_session".to_string()]),
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: true,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert!(
        r.stages_run.contains(&"build_truth_subspace".to_string()),
        "build_truth_subspace must be recorded when opted in with a resolvable dataset; got {:?}",
        r.stages_run
    );
}

/// Companion to [`improve_records_build_truth_subspace_when_opted_in`]: with the
/// flag `false` the stage is inert and its name is never pushed, even though the
/// dataset exists and sessions are present. Proves the opt-in gate is the only
/// thing that enables Stage 2d.
#[tokio::test]
async fn improve_omits_build_truth_subspace_when_disabled() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    cognee_database::ops::datasets::create_dataset(
        h.db.as_ref(),
        Dataset::new("ds_truth_off".to_string(), owner, None, Uuid::new_v4()),
    )
    .await
    .unwrap();

    let r = improve(ImproveParams {
        dataset_name: "ds_truth_off".to_string(),
        session_ids: Some(vec!["truth_optout_session".to_string()]),
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert!(
        !r.stages_run.contains(&"build_truth_subspace".to_string()),
        "build_truth_subspace must be absent when the flag is false; got {:?}",
        r.stages_run
    );
}

/// Stage 3b (`global_context_index`) is opt-in. With the flag set, not running
/// in background, a `session_manager` present, and >= 1 graph edge, the stage
/// formats the edges into the session graph-context and records its name.
/// Mirrors Python `test_improve_global_context_index_opt_in`
/// (test_improve_global_context_index.py:56-92).
#[tokio::test]
async fn improve_records_global_context_index_when_opted_in() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    // Seed exactly one edge so `get_graph_data()` returns a non-empty edge set
    // and the stage takes its store-and-record branch.
    h.graph_db
        .add_edge("node_a", "node_b", "related_to", None)
        .await
        .unwrap();

    let r = improve(ImproveParams {
        dataset_name: "ds_gcidx".to_string(),
        session_ids: None,
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: true,
        build_truth_subspace: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert!(
        r.stages_run.contains(&"global_context_index".to_string()),
        "global_context_index must be recorded when opted in with >= 1 edge; got {:?}",
        r.stages_run
    );

    // The formatted edge line is stored under the synthetic global-context key so
    // any session can read it; assert the exact "src → rel → tgt" rendering.
    let stored = h
        .session_manager
        .get_graph_context(Some("_global_context_index"), Some(&owner.to_string()))
        .await
        .unwrap();
    assert_eq!(
        stored,
        Some("node_a → related_to → node_b".to_string()),
        "the seeded edge must be formatted into the stored global context"
    );
}

/// Companion: in background mode Stage 3b is skipped even with the flag set and
/// edges present, because ordered background pipeline chaining is unsupported.
/// The stage name is NOT recorded and no global context is stored. Mirrors
/// Python `test_improve_skips_global_context_index_in_background`
/// (test_improve_global_context_index.py:95-121).
#[tokio::test]
async fn improve_skips_global_context_index_in_background() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    h.graph_db
        .add_edge("node_a", "node_b", "related_to", None)
        .await
        .unwrap();

    let r = improve(ImproveParams {
        dataset_name: "ds_gcidx_bg".to_string(),
        session_ids: None,
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: true,
        build_truth_subspace: false,
        run_in_background: true,
    })
    .await
    .unwrap();

    assert!(
        !r.stages_run.contains(&"global_context_index".to_string()),
        "global_context_index must be skipped in background mode; got {:?}",
        r.stages_run
    );
    let stored = h
        .session_manager
        .get_graph_context(Some("_global_context_index"), Some(&owner.to_string()))
        .await
        .unwrap();
    assert!(
        stored.is_none(),
        "no global context should be stored in background mode; got {stored:?}"
    );
}

/// Stage 2c (`distill_sessions`) is recorded by the orchestrator ONLY when a
/// session actually publishes >= 1 lesson (improve.py:200-201 / distill.rs
/// conditional push). Seed a session with Q&A and drive the full curate →
/// accept → publish → cognify path via the schema-dispatch LLM so a lesson is
/// published; then run `improve()` (not the standalone distill fn) and assert
/// the orchestrator records the stage AND aggregates the counts.
#[tokio::test]
async fn improve_records_distill_sessions_when_lessons_published() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "distill_optin_session";
    let config = CognifyConfig::default();

    // Seed one Q&A turn — Gate 1 (`NoQaEntries`) needs non-empty content.
    h.session_store
        .create_qa_entry(
            session_id,
            Some(&user_id),
            "How does TerraScout index data?",
            "TerraScout indexes nightly.",
            None,
        )
        .await
        .unwrap();

    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(SchemaDispatchLlm {
        accepted_statement: "TerraScout indexes data nightly.".to_string(),
    });

    let r = improve(ImproveParams {
        dataset_name: "ds_distill".to_string(),
        session_ids: Some(vec![session_id.to_string()]),
        node_name: None,
        owner_id: owner,
        tenant_id: None,
        feedback_alpha: 0.1,
        llm,
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        add_pipeline: Some(&h.add_pipeline),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        cognify_config: &config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    })
    .await
    .unwrap();

    assert!(
        r.stages_run.contains(&"distill_sessions".to_string()),
        "distill_sessions must be recorded when a lesson is published; got {:?}",
        r.stages_run
    );
    assert!(
        r.sessions_distilled > 0,
        "at least one session must be distilled; got {}",
        r.sessions_distilled
    );
    assert!(
        r.lessons_published > 0,
        "at least one lesson must be published; got {}",
        r.lessons_published
    );
}
