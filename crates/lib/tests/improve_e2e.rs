#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! End-to-end tests for the `improve()` orchestrator.
//!
//! Each run reports one `StageResult` per registry stage, in Python's order;
//! these tests assert the per-stage status/reason lists for the gate paths
//! (sessions, opt-ins, backend capabilities, a missing LLM) and the failure
//! rules (fatal abort, budget exhaustion), using mock
//! storage/graph/vector/embedding/LLM backends.

use std::sync::Arc;

use async_trait::async_trait;
use cognee::api::ApiError;
use cognee::api::improve::{
    DEFAULT_STAGES, ImproveConfig, ImproveParams, ImproveResult, ImproveStatus,
    REASON_ABORTED_BY_FATAL_STAGE, REASON_AUTO_FEEDBACK_DISABLED, REASON_BACKEND_UNSUPPORTED,
    REASON_BUDGET_EXHAUSTED, REASON_DISABLED_BY_CONFIG, REASON_NO_CHANGES,
    REASON_NO_LLM_CONFIGURED, REASON_NO_NEW_TRACE_STEPS, REASON_NO_SESSION_IDS,
    REASON_NO_WRITES_SINCE_LAST_IMPROVE, REASON_NOT_PORTED, REASON_OPT_IN_DISABLED,
    REASON_PERSONALIZATION_DISABLED, StageKind, StageResult, StageStatus, TRUTH_SUBSPACE_STAGE,
    improve, stage_watermark_key,
};
use cognee::api::improve::{IMPROVE_PIPELINE_NAME, IMPROVE_WRITE_PIPELINE_NAME};
use cognee::models::Dataset;
use cognee_cognify::CognifyConfig;
use cognee_core::DatasetLocks;
use cognee_database::CheckpointStore;
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
    /// The curator answers HTTP 402 (an exhausted LLM budget) instead.
    curator_out_of_budget: bool,
    /// The agent-context extraction call fails instead of proposing a lesson.
    agent_lessons_fail: bool,
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
        if schema.contains("environment_facts") {
            // Agent-context extraction (`AgentContextExtraction`).
            if self.agent_lessons_fail {
                return Err(LlmError::ApiError("extraction down".to_string()));
            }
            Ok(serde_json::json!({
                "lessons": [{"section": "tool_rules", "content": "Run the indexer before searching.", "confidence": 0.9}]
            }))
        } else if schema.contains("why_learned") {
            // Writer/rejecter (`WrittenLesson`) → accept + write.
            Ok(serde_json::json!({
                "accept": true,
                "statement": self.accepted_statement,
                "entities": ["TerraScout"],
                "why_learned": "learned while asking how indexing works",
            }))
        } else if schema.contains("working_statement") && self.curator_out_of_budget {
            Err(LlmError::PaymentRequired(
                "credit balance too low".to_string(),
            ))
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

// A run claims its sessions in a process-wide registry keyed by owner and
// session id; owners are random, so runs of different tests never queue.
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

/// The parameters of a run against the harness: every backend present, no
/// opt-ins. Tests override what they exercise.
fn params<'a>(
    h: &'a Harness,
    config: &'a CognifyConfig,
    dataset_name: &str,
    session_ids: Option<Vec<String>>,
    llm: Arc<dyn cognee_llm::Llm>,
) -> ImproveParams<'a> {
    ImproveParams {
        dataset_name: dataset_name.to_string(),
        dataset_id: None,
        session_ids,
        node_name: None,
        owner_id: Uuid::new_v4(),
        tenant_id: None,
        feedback_alpha: None,
        config: cognee::api::ImproveConfig::default(),
        personalization: cognee::search::PreferenceConfig::default(),
        auto_feedback: false,
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
        dataset_locks: None,
        cognify_config: config,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
        run_in_background: false,
    }
}

fn mock_llm() -> Arc<dyn cognee_llm::Llm> {
    Arc::new(MockLlm::empty())
}

fn schema_llm(curator_out_of_budget: bool) -> Arc<dyn cognee_llm::Llm> {
    Arc::new(SchemaDispatchLlm {
        accepted_statement: "TerraScout indexes data nightly.".to_string(),
        curator_out_of_budget,
        agent_lessons_fail: false,
    })
}

/// `(stage, status, reason)` per recorded stage.
fn statuses(r: &ImproveResult) -> Vec<(&str, StageStatus, Option<&str>)> {
    r.stages
        .iter()
        .map(|s| (s.stage.as_str(), s.status, s.reason.as_deref()))
        .collect()
}

fn stage<'r>(r: &'r ImproveResult, name: &str) -> &'r StageResult {
    r.stage(name)
        .unwrap_or_else(|| panic!("no {name} stage in {:?}", r.stages))
}

async fn seed_qa(h: &Harness, owner: Uuid, session_id: &str) {
    h.session_store
        .create_qa_entry(
            session_id,
            Some(&owner.to_string()),
            "How does TerraScout index data?",
            "TerraScout indexes nightly.",
            None,
        )
        .await
        .unwrap();
}

async fn create_dataset(h: &Harness, name: &str, owner: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    IngestDb::create_dataset(&*h.db, Dataset::new(name.to_string(), owner, None, id))
        .await
        .unwrap();
    id
}

/// Without session ids every session-fed stage is skipped with
/// `no_session_ids` and, with nothing opted in, the two graph stages with
/// `opt_in_disabled`: the run is `skipped` (Python `test_improve_router`
/// all-skipped case).
#[tokio::test]
async fn improve_without_sessions_skips_every_stage() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let r = improve(params(&h, &config, "ds_none", None, mock_llm()))
        .await
        .unwrap();

    let expected: Vec<(&str, StageStatus, Option<&str>)> = DEFAULT_STAGES
        .iter()
        .map(|s| {
            let reason = if s.needs_sessions() {
                REASON_NO_SESSION_IDS
            } else {
                REASON_OPT_IN_DISABLED
            };
            (s.name(), StageStatus::Skipped, Some(reason))
        })
        .collect();
    assert_eq!(statuses(&r), expected);
    assert!(
        r.stages
            .iter()
            .all(|s| s.duration_ms == 0 && s.run.is_none())
    );
    assert_eq!(r.status(), ImproveStatus::Skipped);
    assert!(r.finished && !r.background);
    assert_eq!(r.dataset_name, "ds_none");
    assert!(r.session_ids.is_empty());
    assert_eq!(r.dataset_id, None, "no stage created the dataset");
}

/// With a session that has no Q&A, every session stage reports what it did:
/// nothing to persist, no trace steps, no lessons. The not-yet-ported stages
/// are skipped with their reasons.
#[tokio::test]
async fn improve_with_an_empty_session_reports_every_stage() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let r = improve(params(
        &h,
        &config,
        "ds_empty_session",
        Some(vec!["empty_session".to_string()]),
        mock_llm(),
    ))
    .await
    .unwrap();

    use StageStatus::*;
    assert_eq!(
        statuses(&r),
        vec![
            ("feedback_weights", Completed, None),
            ("persist_session_qa", Completed, None),
            (
                "persist_agent_traces",
                AlreadyCompleted,
                Some(REASON_NO_NEW_TRACE_STEPS)
            ),
            (
                "extract_agent_context",
                Skipped,
                Some(REASON_AUTO_FEEDBACK_DISABLED)
            ),
            ("distill_sessions", Completed, None),
            (
                "update_user_preferences",
                Skipped,
                Some(REASON_PERSONALIZATION_DISABLED)
            ),
            (
                "build_truth_subspace",
                Skipped,
                Some(REASON_OPT_IN_DISABLED)
            ),
            ("review_conflicts", Skipped, Some(REASON_OPT_IN_DISABLED)),
            (
                "global_context_index",
                Skipped,
                Some(REASON_OPT_IN_DISABLED)
            ),
        ]
    );
    assert_eq!(r.status(), ImproveStatus::Completed);
    assert_eq!(r.session_ids, ["empty_session"]);

    let counts = |name: &str| stage(&r, name).counts.clone();
    assert_eq!(counts("feedback_weights").get("sessions"), Some(&1));
    assert_eq!(
        counts("feedback_weights").get("entries_processed"),
        Some(&0)
    );
    assert_eq!(counts("persist_session_qa").get("sessions"), Some(&1));
    assert_eq!(
        counts("persist_session_qa").get("sessions_persisted"),
        Some(&0)
    );
    assert_eq!(counts("distill_sessions").get("documents"), Some(&0));
    assert_eq!(counts("distill_sessions").get("sessions_failed"), Some(&0));
}

/// Empty session ids count as none (Python filters them out).
#[tokio::test]
async fn improve_drops_empty_session_ids() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let r = improve(params(
        &h,
        &config,
        "ds_blank_ids",
        Some(vec![String::new()]),
        mock_llm(),
    ))
    .await
    .unwrap();
    assert!(r.session_ids.is_empty());
    assert_eq!(
        stage(&r, "feedback_weights").reason.as_deref(),
        Some(REASON_NO_SESSION_IDS)
    );
}

/// A backend that does not store feedback weights skips the stage with
/// `backend_unsupported` (Python `FeedbackWeightsStage.gate`).
#[tokio::test]
async fn improve_skips_feedback_weights_on_an_unsupported_backend() {
    let h = make_harness().await;
    h.graph_db.set_supports_feedback_weights(false);
    let config = CognifyConfig::default();
    let r = improve(params(
        &h,
        &config,
        "ds_fw_unsupported",
        Some(vec!["fw_unsupported_session".to_string()]),
        mock_llm(),
    ))
    .await
    .unwrap();
    let fw = stage(&r, "feedback_weights");
    assert_eq!(fw.status, StageStatus::Skipped);
    assert_eq!(fw.reason.as_deref(), Some(REASON_BACKEND_UNSUPPORTED));
    assert_eq!(fw.duration_ms, 0);
}

/// Without a usable LLM the stages that draft text with one are skipped with
/// `no_llm_configured`; the session bridging itself still runs.
#[tokio::test]
async fn improve_without_an_llm_skips_the_llm_stages() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(cognee_llm::UnconfiguredLlm::new("openai"));
    let mut p = params(
        &h,
        &config,
        "ds_keyless",
        Some(vec!["keyless_session".to_string()]),
        llm,
    );
    p.auto_feedback = true;
    let r = improve(p).await.unwrap();
    for name in ["extract_agent_context", "distill_sessions"] {
        let s = stage(&r, name);
        assert_eq!(s.status, StageStatus::Skipped, "{name}");
        assert_eq!(
            s.reason.as_deref(),
            Some(REASON_NO_LLM_CONFIGURED),
            "{name}"
        );
    }
    assert_eq!(
        stage(&r, "persist_session_qa").status,
        StageStatus::Completed
    );
}

/// The fatal stage stops the run: missing session backends error
/// `feedback_weights` (non-fatal, recorded) and then `persist_session_qa`,
/// which aborts with the partial result and the rest skipped
/// `aborted_by_fatal_stage` (Python `test_fatal_stage_stops_run_and_raises_with_partial_result`).
#[tokio::test]
async fn improve_aborts_when_the_fatal_stage_errors() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let mut p = params(
        &h,
        &config,
        "ds_nosess",
        Some(vec!["aborted_session".to_string()]),
        mock_llm(),
    );
    p.session_store = None;
    p.session_manager = None;
    p.add_pipeline = None;

    let error = improve(p).await.unwrap_err();
    let ApiError::ImproveAborted {
        result,
        budget_exhausted,
    } = &error
    else {
        panic!("expected ImproveAborted, got {error:?}");
    };
    assert!(!budget_exhausted);
    assert!(
        error
            .to_string()
            .starts_with("improve: fatal stage 'persist_session_qa' errored: "),
        "{error}"
    );
    let recorded = statuses(result);
    assert_eq!(recorded.len(), DEFAULT_STAGES.len());
    assert_eq!(recorded[0].1, StageStatus::Errored);
    assert_eq!(recorded[1].1, StageStatus::Errored);
    for (_, status, reason) in &recorded[2..] {
        assert_eq!(*status, StageStatus::Skipped);
        assert_eq!(*reason, Some(REASON_ABORTED_BY_FATAL_STAGE));
    }
    assert_eq!(result.error, result.stages[1].error);
    assert_eq!(result.status(), ImproveStatus::Errored);
}

/// Stage 7 (`build_truth_subspace`) is opt-in. With sessions present, the flag
/// set, and the dataset resolvable, the stage runs: an empty session distils
/// no learnings, so the build completes with nothing built.
#[tokio::test]
async fn improve_records_build_truth_subspace_when_opted_in() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let mut p = params(
        &h,
        &config,
        "ds_truth",
        Some(vec!["truth_optin_session".to_string()]),
        mock_llm(),
    );
    create_dataset(&h, "ds_truth", p.owner_id).await;
    p.build_truth_subspace = true;
    let r = improve(p).await.unwrap();

    let stage = truth_stage(&r);
    assert_eq!(stage.status, StageStatus::Completed, "{stage:?}");
    assert_eq!(stage.reason, None);
    assert_eq!(stage.error, None);
    assert_eq!(stage.counts.get("anchors"), Some(&0));
    assert_eq!(stage.counts.get("nodes_skipped"), Some(&0));
}

/// The one truth-stage record an `improve()` run reports.
fn truth_stage(r: &ImproveResult) -> &StageResult {
    let mut stages = r.stages.iter().filter(|s| s.stage == TRUTH_SUBSPACE_STAGE);
    let stage = stages.next().expect("a build_truth_subspace stage record");
    assert!(stages.next().is_none(), "exactly one truth stage record");
    stage
}

/// With the flag set but a graph backend that cannot store truth state, the
/// stage is skipped with `backend_unsupported` by its gate, before the build
/// runs — Python's `BuildTruthSubspaceStage.gate`.
#[tokio::test]
async fn improve_skips_build_truth_subspace_on_an_unsupported_backend() {
    let h = make_harness().await;
    h.graph_db.set_supports_truth_state(false);
    let config = CognifyConfig::default();
    let mut p = params(
        &h,
        &config,
        "ds_truth_unsupported",
        Some(vec!["truth_unsupported_session".to_string()]),
        mock_llm(),
    );
    create_dataset(&h, "ds_truth_unsupported", p.owner_id).await;
    p.build_truth_subspace = true;
    let r = improve(p).await.unwrap();

    let stage = truth_stage(&r);
    assert_eq!(stage.status, StageStatus::Skipped);
    assert_eq!(stage.reason.as_deref(), Some(REASON_BACKEND_UNSUPPORTED));
    assert_eq!(stage.duration_ms, 0, "a gated stage never ran");
}

/// One document of `dataset_id` with one chunk naming `entity`, so the
/// global context index has a bucket and a root to write.
async fn seed_document(h: &Harness, dataset_id: Uuid, owner: Uuid, entity: &str) -> Uuid {
    use cognee_database::ops::{data as data_ops, datasets as ds_ops};
    use cognee_graph::GraphDBTraitExt;
    let data_id = Uuid::new_v4();
    let data = cognee::models::Data::builder(
        data_id,
        "notes.md",
        "file:///tmp/notes.md",
        "notes.md",
        "md",
        "text/markdown",
        data_id.simple().to_string(),
        owner,
    )
    .build();
    data_ops::create_data(&h.db, data).await.unwrap();
    ds_ops::attach_data_to_dataset(&h.db, dataset_id, data_id)
        .await
        .unwrap();
    let chunk = format!("chunk-{entity}");
    h.graph_db
        .add_node(&serde_json::json!({"id": chunk, "type": "DocumentChunk", "document_id": data_id.to_string()}))
        .await
        .unwrap();
    h.graph_db
        .add_node(&serde_json::json!({"id": entity, "type": "Entity", "name": entity}))
        .await
        .unwrap();
    h.graph_db
        .add_edge(&chunk, entity, "contains", None)
        .await
        .unwrap();
    data_id
}

async fn index_node_texts(h: &Harness) -> Vec<String> {
    let (nodes, _) = h.graph_db.get_graph_data().await.unwrap();
    let mut texts: Vec<String> = nodes
        .into_iter()
        .filter(|(_, p)| p.get("type") == Some(&serde_json::json!("GlobalContextSummary")))
        .filter_map(|(_, p)| p.get("text").and_then(Value::as_str).map(str::to_string))
        .collect();
    texts.sort();
    texts
}

/// Stage 9 (`global_context_index`) is opt-in and needs no sessions. It
/// builds Python's default `from_entities` index: one bucket per document
/// and a root, with no LLM call (Python `test_improve_global_context_index`).
#[tokio::test]
async fn improve_builds_the_global_context_index_when_opted_in() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let mut p = params(&h, &config, "ds_gcidx", None, mock_llm());
    let owner = p.owner_id;
    let dataset_id = create_dataset(&h, "ds_gcidx", owner).await;
    seed_document(&h, dataset_id, owner, "Alice").await;
    p.build_global_context_index = true;
    let r = improve(p).await.unwrap();

    let gci = stage(&r, "global_context_index");
    assert_eq!(gci.status, StageStatus::Completed);
    assert_eq!(gci.counts.get("nodes_written"), Some(&2));
    assert_eq!(r.status(), ImproveStatus::Completed);
    assert_eq!(
        index_node_texts(&h).await,
        [
            "Topics: Entity (1)\nEntities: Alice\nDocuments: notes.md",
            "notes.md\nEntity: Alice",
        ]
    );
}

/// A background run (dispatched by its host) runs the same stages, the
/// global context index included, and says it was a background run.
#[tokio::test]
async fn improve_runs_global_context_index_in_background() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    h.graph_db
        .add_edge("node_a", "node_b", "related_to", None)
        .await
        .unwrap();
    let mut p = params(&h, &config, "ds_gcidx_bg", None, mock_llm());
    p.build_global_context_index = true;
    p.run_in_background = true;
    let r = improve(p).await.unwrap();

    assert!(r.background && r.finished);
    assert_eq!(
        stage(&r, "global_context_index").status,
        StageStatus::Completed
    );
}

/// `distill_sessions` publishes a lesson through the full curate → accept →
/// publish → cognify path, and reports it in its counts.
#[tokio::test]
async fn improve_reports_distilled_lessons() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let session_id = "distill_optin_session";
    let p = params(
        &h,
        &config,
        "ds_distill",
        Some(vec![session_id.to_string()]),
        schema_llm(false),
    );
    seed_qa(&h, p.owner_id, session_id).await;
    let r = improve(p).await.unwrap();

    let persist = stage(&r, "persist_session_qa");
    assert_eq!(persist.status, StageStatus::Completed, "{persist:?}");
    assert_eq!(persist.counts.get("sessions_persisted"), Some(&1));
    let distill = stage(&r, "distill_sessions");
    assert_eq!(distill.status, StageStatus::Completed, "{distill:?}");
    assert!(distill.counts["documents"] > 0, "{distill:?}");
    assert_eq!(distill.counts.get("sessions_completed"), Some(&1));
    assert!(
        r.dataset_id.is_some(),
        "persist_session_qa created the dataset"
    );
}

/// An exhausted LLM budget in a non-fatal stage ends the run there: the
/// stage is errored and every later stage is skipped `budget_exhausted`,
/// without an error (Python `test_improve_budget_exhaustion.py`).
#[tokio::test]
async fn improve_stops_on_an_exhausted_llm_budget() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let session_id = "budget_session";
    let p = params(
        &h,
        &config,
        "ds_budget",
        Some(vec![session_id.to_string()]),
        schema_llm(true),
    );
    seed_qa(&h, p.owner_id, session_id).await;
    let r = improve(p).await.unwrap();

    let distill = stage(&r, "distill_sessions");
    assert_eq!(distill.status, StageStatus::Errored, "{distill:?}");
    assert_eq!(distill.counts.get("sessions_failed"), Some(&1));
    let after: Vec<_> = statuses(&r).into_iter().skip(5).collect();
    assert_eq!(after.len(), 4);
    for (name, status, reason) in after {
        assert_eq!(status, StageStatus::Skipped, "{name}");
        assert_eq!(reason, Some(REASON_BUDGET_EXHAUSTED), "{name}");
    }
    assert_eq!(r.error, None);
    assert_eq!(r.status(), ImproveStatus::Errored);
}

/// Under per-dataset storage (`ENABLE_BACKEND_ACCESS_CONTROL`) the graph
/// stages read the dataset's own graph: with routing engines that fail
/// outside a dataset scope, the global context index still runs.
#[tokio::test]
async fn improve_runs_the_graph_stages_in_the_dataset_under_per_dataset_storage() {
    let h = make_harness().await;
    let stores = cognee_test_utils::PerDatasetMocks::new();
    let config = CognifyConfig::default();
    let mut p = params(&h, &config, "ds_per_dataset", None, mock_llm());
    let dataset_id = create_dataset(&h, "ds_per_dataset", p.owner_id).await;
    p.graph_db = stores.graph_db();
    p.vector_db = stores.vector_db();
    p.build_global_context_index = true;
    let r = improve(p).await.unwrap();

    assert_eq!(
        stage(&r, "global_context_index").status,
        StageStatus::Completed
    );
    assert_eq!(r.dataset_id, Some(dataset_id));
    assert_eq!(
        stores.graph_datasets(),
        vec![dataset_id],
        "the stages read the dataset's graph"
    );
}

/// `improve.stages_disabled` skips the named stages with
/// `disabled_by_config`, ahead of every other gate (Python
/// `test_improve_orchestration` / `test_stage_gates`
/// `test_disabled_by_config_wins_over_everything`).
#[tokio::test]
async fn stages_disabled_by_config_are_skipped_first() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let mut p = params(
        &h,
        &config,
        "ds_disabled",
        Some(vec!["disabled_session".to_string()]),
        mock_llm(),
    );
    p.config = ImproveConfig {
        stages_disabled: vec![
            "feedback_weights".to_string(),
            "distill_sessions".to_string(),
            // A graph-only stage the caller opted into: the config still wins.
            "global_context_index".to_string(),
        ],
        ..ImproveConfig::default()
    };
    p.build_global_context_index = true;
    let r = improve(p).await.unwrap();
    for name in [
        "feedback_weights",
        "distill_sessions",
        "global_context_index",
    ] {
        let s = stage(&r, name);
        assert_eq!(s.status, StageStatus::Skipped, "{name}");
        assert_eq!(
            s.reason.as_deref(),
            Some(REASON_DISABLED_BY_CONFIG),
            "{name}"
        );
        assert_eq!(s.duration_ms, 0, "{name}");
    }
    assert_eq!(
        stage(&r, "persist_session_qa").status,
        StageStatus::Completed
    );
    assert_eq!(r.stages.len(), DEFAULT_STAGES.len());
}

/// An invalid configuration fails the run before any stage, loudly: a
/// disabled fatal stage, an unknown stage name, an out-of-range alpha.
#[tokio::test]
async fn improve_rejects_an_invalid_configuration_before_any_stage() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let bad_configs = [
        ImproveConfig {
            stages_disabled: vec!["persist_session_qa".to_string()],
            ..ImproveConfig::default()
        },
        ImproveConfig {
            stages_disabled: vec!["persist_sesion_qa".to_string()],
            ..ImproveConfig::default()
        },
        ImproveConfig {
            feedback_alpha: 0.0,
            ..ImproveConfig::default()
        },
    ];
    for bad in bad_configs {
        let mut p = params(&h, &config, "ds_bad_config", None, mock_llm());
        p.config = bad.clone();
        let err = improve(p).await.unwrap_err();
        assert!(
            matches!(err, ApiError::InvalidArgument(_)),
            "{bad:?}: {err}"
        );
    }
    let mut p = params(&h, &config, "ds_bad_alpha", None, mock_llm());
    p.feedback_alpha = Some(1.5);
    let err = improve(p).await.unwrap_err();
    assert!(err.to_string().contains("feedback_alpha"), "{err}");
}

/// Python's `test_improve_agent_context.py`: with automatic session context
/// on, each session's pending trace steps become agent lessons, counted as
/// `lessons`; the watermark makes a re-run find nothing pending. Without it
/// the stage is skipped with `auto_feedback_disabled`.
#[tokio::test]
async fn extract_agent_context_turns_pending_traces_into_lessons() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let owner = Uuid::new_v4();
    let user = owner.to_string();
    for session in ["agent_a", "agent_b"] {
        for step in ["index", "search"] {
            h.session_manager
                .add_agent_trace_step(
                    &user,
                    Some(session),
                    step,
                    "success",
                    "",
                    "",
                    serde_json::json!({}),
                    None,
                    "",
                    false,
                )
                .await
                .unwrap();
        }
    }
    let run = |auto_feedback: bool| {
        let mut p = params(
            &h,
            &config,
            "ds_agent_context",
            Some(vec!["agent_a".to_string(), "agent_b".to_string()]),
            schema_llm(false),
        );
        p.owner_id = owner;
        p.auto_feedback = auto_feedback;
        p
    };

    let off = improve(run(false)).await.unwrap();
    let s = stage(&off, "extract_agent_context");
    assert_eq!(
        (s.status, s.reason.as_deref()),
        (StageStatus::Skipped, Some(REASON_AUTO_FEEDBACK_DISABLED))
    );

    let r = improve(run(true)).await.unwrap();
    let s = stage(&r, "extract_agent_context");
    assert_eq!(s.status, StageStatus::Completed, "{s:?}");
    assert_eq!(s.counts.get("lessons"), Some(&2));
    assert_eq!(s.counts.get("sessions_failed"), Some(&0));
    let rows = h
        .session_manager
        .get_session_context_entries(Some("agent_a"), Some(&user))
        .await
        .unwrap();
    let lessons = cognee_session::coerce_active_context_entries(&rows).unwrap();
    assert_eq!(lessons.len(), 1);
    assert_eq!(
        (
            lessons[0].context_profile.as_str(),
            lessons[0].section.as_str()
        ),
        ("agent", "tool_rules")
    );

    // Nothing is pending any more.
    let again = improve(run(true)).await.unwrap();
    let s = stage(&again, "extract_agent_context");
    assert_eq!(s.status, StageStatus::Completed);
    assert_eq!(s.counts.get("lessons"), Some(&0));
}

/// Python's `test_one_failing_session_does_not_block_others`: an extraction
/// failure is counted, and errors the stage without aborting the run.
#[tokio::test]
async fn a_failed_agent_context_extraction_errors_only_its_stage() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let owner = Uuid::new_v4();
    h.session_manager
        .add_agent_trace_step(
            &owner.to_string(),
            Some("agent_fail"),
            "index",
            "error",
            "",
            "",
            serde_json::json!({}),
            None,
            "exit 1",
            false,
        )
        .await
        .unwrap();
    let llm = Arc::new(SchemaDispatchLlm {
        accepted_statement: "TerraScout indexes data nightly.".to_string(),
        curator_out_of_budget: false,
        agent_lessons_fail: true,
    });
    let mut p = params(
        &h,
        &config,
        "ds_agent_fail",
        Some(vec!["agent_fail".to_string()]),
        llm,
    );
    p.owner_id = owner;
    p.auto_feedback = true;
    let r = improve(p).await.unwrap();
    let s = stage(&r, "extract_agent_context");
    assert_eq!(s.status, StageStatus::Errored);
    assert!(
        s.error.as_deref().unwrap().contains("extraction down"),
        "{s:?}"
    );
    assert_eq!(s.counts.get("lessons"), Some(&0));
    assert_eq!(s.counts.get("sessions_failed"), Some(&1));
    // A non-fatal stage: the later stages still ran.
    assert_eq!(stage(&r, "distill_sessions").status, StageStatus::Completed);
    assert_eq!(r.status(), ImproveStatus::Errored);
}

/// `update_user_preferences` end to end: a turn rated 5 that used two graph
/// nodes writes two `prefers` edges from the user's preference node; a re-run
/// with nothing new is `completed` with the reason `no_changes`. Off, the
/// stage is skipped with `personalization_disabled`.
#[tokio::test]
async fn update_user_preferences_writes_prefers_edges() {
    use cognee::search::user_preferences::{load_preference_state, node_turn_counter};

    let h = make_harness().await;
    let config = CognifyConfig::default();
    let owner = Uuid::new_v4();
    let user = owner.to_string();
    for id in ["pref-node-1", "pref-node-2"] {
        h.graph_db
            .add_node_raw(serde_json::json!({"id": id, "type": "Entity", "name": id}))
            .await
            .unwrap();
    }
    let qa_id = h
        .session_manager
        .save_qa(
            Some("personal_session"),
            Some(&user),
            "How does TerraScout index data?",
            "TerraScout indexes nightly.",
            None,
            Some(cognee_session::UsedGraphElementIds {
                node_ids: vec!["pref-node-1".to_string(), "pref-node-2".to_string()],
                edge_ids: vec![],
            }),
        )
        .await
        .unwrap();
    h.session_manager
        .add_feedback(Some("personal_session"), Some(&user), &qa_id, None, Some(5))
        .await
        .unwrap();
    let run = |enabled: bool| {
        let mut p = params(
            &h,
            &config,
            "ds_personal",
            Some(vec!["personal_session".to_string()]),
            schema_llm(false),
        );
        p.owner_id = owner;
        p.personalization = cognee::search::PreferenceConfig {
            enabled,
            ..Default::default()
        };
        p
    };

    let off = improve(run(false)).await.unwrap();
    assert_eq!(
        stage(&off, "update_user_preferences").reason.as_deref(),
        Some(REASON_PERSONALIZATION_DISABLED)
    );

    let r = improve(run(true)).await.unwrap();
    let s = stage(&r, "update_user_preferences");
    assert_eq!(
        (s.status, s.reason.as_deref()),
        (StageStatus::Completed, None)
    );
    assert_eq!(s.counts.get("turns_applied"), Some(&1));
    assert_eq!(s.counts.get("edges_written"), Some(&2));
    let dataset = r.dataset_id.expect("the session stage created the dataset");
    let (node, weights) = load_preference_state(h.graph_db.as_ref(), &user, &dataset.to_string())
        .await
        .unwrap()
        .expect("the preference node");
    assert_eq!(node_turn_counter(&node), 1);
    // 0.5 + 0.3 * (1.0 - 0.5)
    assert_eq!(weights["pref-node-1"].weight, 0.65);
    assert_eq!(weights["pref-node-2"].weight, 0.65);

    let again = improve(run(true)).await.unwrap();
    let s = stage(&again, "update_user_preferences");
    assert_eq!(
        (s.status, s.reason.as_deref()),
        (StageStatus::Completed, Some(REASON_NO_CHANGES))
    );
    assert_eq!(s.counts.get("edges_written"), Some(&0));
}

/// `improve.review_conflicts` opts every run into `review_conflicts`; the
/// stage then applies its own gates (no LLM), and is not ported past them.
#[tokio::test]
async fn review_conflicts_config_opts_in() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let opted_in = || ImproveConfig {
        review_conflicts: true,
        ..ImproveConfig::default()
    };
    let mut p = params(&h, &config, "ds_review", None, mock_llm());
    p.config = opted_in();
    let r = improve(p).await.unwrap();
    assert_eq!(
        stage(&r, "review_conflicts").reason.as_deref(),
        Some(REASON_NOT_PORTED)
    );

    let keyless: Arc<dyn cognee_llm::Llm> = Arc::new(cognee_llm::UnconfiguredLlm::new("openai"));
    let mut p = params(&h, &config, "ds_review_keyless", None, keyless);
    p.config = opted_in();
    let r = improve(p).await.unwrap();
    assert_eq!(
        stage(&r, "review_conflicts").reason.as_deref(),
        Some(REASON_NO_LLM_CONFIGURED)
    );
}

// ---------------------------------------------------------------------------
// The stage watermark (`already_completed: no_writes_since_last_improve`).
// ---------------------------------------------------------------------------

async fn log_write(h: &Harness, dataset_id: Uuid) {
    use cognee_database::{PipelineRunStatus, RelationalDb};
    Arc::clone(&h.db)
        .pipeline_runs()
        .log_pipeline_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "cognify_pipeline",
            Some(dataset_id),
            PipelineRunStatus::Completed,
            None,
        )
        .await
        .unwrap();
}

/// A completed `global_context_index` stamps its start; the next run with no
/// write since reports `already_completed`; a write makes it run again
/// (Python `GlobalContextIndexStage`, `graph_changes.py`).
#[tokio::test]
async fn global_context_index_is_already_completed_without_writes_since_last_improve() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let owner = Uuid::new_v4();
    let dataset_id = create_dataset(&h, "ds_watermark", owner).await;
    let run = || {
        let mut p = params(&h, &config, "ds_watermark", None, mock_llm());
        p.owner_id = owner;
        p.build_global_context_index = true;
        improve(p)
    };

    let first = run().await.unwrap();
    assert_eq!(
        stage(&first, "global_context_index").status,
        StageStatus::Completed
    );
    let stamp = h
        .checkpoint_store
        .load(&stage_watermark_key(
            StageKind::GlobalContextIndex,
            dataset_id,
        ))
        .await
        .unwrap();
    assert!(stamp.is_some(), "a completed run stamps the stage");

    let second = run().await.unwrap();
    let gci = stage(&second, "global_context_index");
    assert_eq!(gci.status, StageStatus::AlreadyCompleted);
    assert_eq!(
        gci.reason.as_deref(),
        Some(REASON_NO_WRITES_SINCE_LAST_IMPROVE)
    );
    assert_eq!(second.status(), ImproveStatus::Completed);

    log_write(&h, dataset_id).await;
    let third = run().await.unwrap();
    assert_eq!(
        stage(&third, "global_context_index").status,
        StageStatus::Completed
    );
}

/// A delete runs no pipeline, but it changes the dataset: like Python's
/// `forget` operation row, it records a `forget` row the change check counts,
/// so the next run rebuilds the index (Python `test_watermarks.py`
/// "deletion").
#[tokio::test]
async fn a_data_item_delete_invalidates_the_global_context_index() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let owner = Uuid::new_v4();
    let rows = h
        .add_pipeline
        .add(
            vec![cognee::models::DataInput::Text("Alice likes tea.".into())],
            "ds_forget",
            owner,
            None,
        )
        .await
        .unwrap();
    let dataset_id = IngestDb::get_dataset_by_name_in_tenant(&*h.db, "ds_forget", owner, None)
        .await
        .unwrap()
        .unwrap()
        .id;
    let run = || {
        let mut p = params(&h, &config, "ds_forget", None, mock_llm());
        p.owner_id = owner;
        p.build_global_context_index = true;
        improve(p)
    };
    let first = run().await.unwrap();
    assert_eq!(
        stage(&first, "global_context_index").status,
        StageStatus::Completed
    );
    assert_eq!(
        stage(&run().await.unwrap(), "global_context_index").status,
        StageStatus::AlreadyCompleted
    );

    {
        use cognee_database::RelationalDb;
        let delete = cognee_delete::DeleteService::new(
            Arc::clone(&h.storage),
            h.db.clone() as Arc<dyn cognee_database::DeleteDb>,
        )
        .with_pipeline_run_repo(Arc::clone(&h.db).pipeline_runs());
        delete
            .execute(&cognee_delete::DeleteRequest {
                scope: cognee_delete::DeleteScope::DataInDataset {
                    dataset_id,
                    data_id: rows[0].id,
                    delete_dataset_if_empty: false,
                },
                mode: cognee_delete::DeleteMode::Hard,
                memory_only: false,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        stage(&run().await.unwrap(), "global_context_index").status,
        StageStatus::Completed,
        "the delete is a change"
    );
}

/// Python counts every non-noop improve as a change. A run whose stage wrote
/// to the graph records an `improve` row, so the next run rebuilds; a host's
/// own `improve_pipeline` row (the HTTP server's) does not count, so repeated
/// index-only runs stay `already_completed`.
#[tokio::test]
async fn an_improve_that_wrote_invalidates_the_index_and_a_host_row_does_not() {
    use cognee_database::{PipelineRunStatus, RelationalDb};
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let owner = Uuid::new_v4();
    let dataset_id = create_dataset(&h, "ds_improve_write", owner).await;
    let run = |sessions: Option<Vec<String>>| {
        let mut p = params(&h, &config, "ds_improve_write", sessions, mock_llm());
        p.owner_id = owner;
        p.build_global_context_index = true;
        improve(p)
    };
    run(None).await.unwrap();

    Arc::clone(&h.db)
        .pipeline_runs()
        .log_pipeline_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            IMPROVE_PIPELINE_NAME,
            Some(dataset_id),
            PipelineRunStatus::Completed,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        stage(&run(None).await.unwrap(), "global_context_index").status,
        StageStatus::AlreadyCompleted,
        "a host's improve row is not a change"
    );

    // The row a run records after its first writing stage (which stages
    // count is pinned by the orchestrator's unit tests).
    Arc::clone(&h.db)
        .pipeline_runs()
        .log_pipeline_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            IMPROVE_WRITE_PIPELINE_NAME,
            Some(dataset_id),
            PipelineRunStatus::Completed,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        stage(&run(None).await.unwrap(), "global_context_index").status,
        StageStatus::Completed,
        "an improve that wrote is a change"
    );
}

/// A run that ends errored saves no stamp, so the next run builds again.
#[tokio::test]
async fn an_errored_run_does_not_stamp_the_stage() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let owner = Uuid::new_v4();
    let dataset_id = create_dataset(&h, "ds_watermark_err", owner).await;

    // A failed vector write fails the build.
    seed_document(&h, dataset_id, owner, "Alice").await;
    h.vector_db.set_index_error("boom");
    let mut p = params(&h, &config, "ds_watermark_err", None, mock_llm());
    p.owner_id = owner;
    p.build_global_context_index = true;
    let r = improve(p).await.unwrap();
    assert_eq!(
        stage(&r, "global_context_index").status,
        StageStatus::Errored
    );
    assert_eq!(r.status(), ImproveStatus::Errored);
    let stamp = h
        .checkpoint_store
        .load(&stage_watermark_key(
            StageKind::GlobalContextIndex,
            dataset_id,
        ))
        .await
        .unwrap();
    assert!(stamp.is_none(), "an errored run saves no stamp");
}

// ---------------------------------------------------------------------------
// The waiting dataset lock and session claim (Python `test_improve_queue.py`).
// ---------------------------------------------------------------------------

const SHORT: std::time::Duration = std::time::Duration::from_millis(200);
const LONG: std::time::Duration = std::time::Duration::from_secs(60);

fn locked_add_pipeline(h: &Harness, locks: &Arc<DatasetLocks>) -> AddPipeline {
    let ingest_db: Arc<dyn IngestDb> = h.db.clone();
    AddPipeline::new(Arc::clone(&h.storage), ingest_db)
        .with_thread_pool(Arc::new(
            cognee::core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(h.graph_db.clone() as Arc<dyn cognee_graph::GraphDBTrait>)
        .with_vector_db(h.vector_db.clone() as Arc<dyn cognee_vector::VectorDB>)
        .with_database(h.db.clone())
        .with_dataset_locks(Arc::clone(locks))
}

/// An improve on a dataset another run holds waits, then runs every stage —
/// whether it targets another session, the same one, or none
/// (`test_an_overlapping_improve_waits_then_runs_every_stage`).
#[tokio::test]
async fn an_overlapping_improve_waits_then_runs_every_stage() {
    for sessions in [Some(vec!["queue_other".to_string()]), None] {
        let h = make_harness().await;
        let config = CognifyConfig::default();
        let locks = Arc::new(DatasetLocks::new());
        let owner = Uuid::new_v4();
        let dataset_id = create_dataset(&h, "ds_queue", owner).await;
        let holder = locks.lock(dataset_id).await;

        let mut p = params(&h, &config, "ds_queue", sessions.clone(), mock_llm());
        p.owner_id = owner;
        p.dataset_locks = Some(Arc::clone(&locks));
        let run = improve(p);
        tokio::pin!(run);
        assert!(
            tokio::time::timeout(SHORT, &mut run).await.is_err(),
            "{sessions:?}: the run waits for the dataset"
        );

        drop(holder);
        let r = tokio::time::timeout(LONG, run).await.unwrap().unwrap();
        assert_eq!(r.stages.len(), DEFAULT_STAGES.len());
        assert_eq!(r.dataset_id, Some(dataset_id));
    }
}

/// A dataset that does not exist yet is held by the identity an `add` of it
/// would lock.
#[tokio::test]
async fn an_improve_of_a_new_dataset_holds_its_identity() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let locks = Arc::new(DatasetLocks::new());
    let owner = Uuid::new_v4();
    let holder = locks
        .lock(cognee_ingestion::generate_dataset_id(
            "ds_new_queue",
            owner,
            None,
        ))
        .await;
    let mut p = params(&h, &config, "ds_new_queue", None, mock_llm());
    p.owner_id = owner;
    p.dataset_locks = Some(Arc::clone(&locks));
    let run = improve(p);
    tokio::pin!(run);
    assert!(tokio::time::timeout(SHORT, &mut run).await.is_err());
    drop(holder);
    tokio::time::timeout(LONG, run).await.unwrap().unwrap();
}

/// `test_improves_on_different_datasets_do_not_wait`.
#[tokio::test]
async fn improves_on_different_datasets_do_not_wait() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let locks = Arc::new(DatasetLocks::new());
    let owner = Uuid::new_v4();
    let held = create_dataset(&h, "ds_held", owner).await;
    create_dataset(&h, "ds_free", owner).await;
    let _holder = locks.lock(held).await;

    let mut p = params(&h, &config, "ds_free", None, mock_llm());
    p.owner_id = owner;
    p.dataset_locks = Some(Arc::clone(&locks));
    let r = tokio::time::timeout(LONG, improve(p))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.stages.len(), DEFAULT_STAGES.len());
}

/// A run queued behind the dataset is let in when the run ahead of it is
/// cancelled while it waits for a session
/// (`test_a_cancelled_holder_lets_the_next_improve_run`).
#[tokio::test]
async fn a_cancelled_run_lets_the_next_improve_run() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let locks = Arc::new(DatasetLocks::new());
    let owner = Uuid::new_v4();
    let dataset = create_dataset(&h, "ds_cancel", owner).await;
    // The first run takes the dataset, then waits on a session someone holds.
    let _session = cognee_session::acquire_improve_lock_many(cognee_session::improve_lock_keys(
        ["cancel_held"],
        &owner.to_string(),
    ))
    .await;

    let mut first = params(
        &h,
        &config,
        "ds_cancel",
        Some(vec!["cancel_held".to_string()]),
        mock_llm(),
    );
    first.owner_id = owner;
    first.dataset_locks = Some(Arc::clone(&locks));
    // Boxed so that dropping it drops the run itself.
    let mut first = Box::pin(improve(first));
    // Polled until it holds the dataset (the registry's lock no longer comes
    // free): a future only progresses while polled, and on a loaded machine
    // one slice may not reach the hold.
    let deadline = std::time::Instant::now() + LONG;
    loop {
        assert!(tokio::time::timeout(SHORT, &mut first).await.is_err());
        if tokio::time::timeout(SHORT, locks.lock(dataset))
            .await
            .is_err()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the first run never took the dataset"
        );
    }

    let mut second = params(&h, &config, "ds_cancel", None, mock_llm());
    second.owner_id = owner;
    second.dataset_locks = Some(Arc::clone(&locks));
    let second = improve(second);
    tokio::pin!(second);
    assert!(
        tokio::time::timeout(SHORT, &mut second).await.is_err(),
        "the second run queues behind the first on the dataset"
    );

    // Cancelling the first releases the dataset it held.
    drop(first);
    let r = tokio::time::timeout(LONG, second).await.unwrap().unwrap();
    assert_eq!(r.stages.len(), DEFAULT_STAGES.len());
}

/// The stages' `add` calls re-enter the dataset lock the run holds — through
/// the registry named on the params, and through the add pipeline's own when
/// none is named — instead of waiting on it forever.
#[tokio::test]
async fn the_stage_adds_ride_on_the_run_dataset_hold() {
    for name_the_registry in [true, false] {
        let h = make_harness().await;
        let config = CognifyConfig::default();
        let locks = Arc::new(DatasetLocks::new());
        let pipeline = locked_add_pipeline(&h, &locks);
        let session_id = format!("reentrant_session_{name_the_registry}");
        let mut p = params(
            &h,
            &config,
            "ds_reentrant",
            Some(vec![session_id.clone()]),
            schema_llm(false),
        );
        p.add_pipeline = Some(&pipeline);
        if name_the_registry {
            p.dataset_locks = Some(Arc::clone(&locks));
        }
        seed_qa(&h, p.owner_id, &session_id).await;
        let r = tokio::time::timeout(LONG, improve(p))
            .await
            .expect("a stage add must not wait on the run's own hold")
            .unwrap();
        let persist = stage(&r, "persist_session_qa");
        assert_eq!(persist.status, StageStatus::Completed, "{persist:?}");
        assert_eq!(persist.counts.get("sessions_persisted"), Some(&1));
        assert!(r.dataset_id.is_some(), "the stage created the dataset");
    }
}

/// Python `test_improve_queue.py` "background improve returns at once and
/// runs after the holder": a session remember's improve is launched detached
/// and returns at once, waits for the dataset's holder in the process-wide
/// registry, then runs.
#[tokio::test]
async fn a_background_improve_returns_at_once_and_runs_after_the_holder() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let dataset_id = create_dataset(&h, "ds_bg_queue", owner).await;
    seed_qa(&h, owner, "bg_queue").await;
    let holder = DatasetLocks::global().lock(dataset_id).await;

    let launch = cognee::api::launch_session_auto_improve(cognee::api::SessionAutoImprove {
        dataset_name: "ds_bg_queue".to_string(),
        dataset_id: Some(dataset_id),
        session_id: "bg_queue".to_string(),
        owner_id: owner,
        tenant_id: None,
        self_improvement: true,
        improve_config: Some(ImproveConfig::default()),
        personalization: cognee::search::PreferenceConfig::default(),
        auto_feedback: false,
        llm: mock_llm(),
        storage: Arc::clone(&h.storage),
        graph_db: h.graph_db.clone() as Arc<_>,
        vector_db: h.vector_db.clone() as Arc<_>,
        embedding_engine: h.embedding_engine.clone() as Arc<_>,
        ontology_resolver: Arc::clone(&h.ontology),
        db: Some(h.db.clone()),
        session_store: Some(Arc::clone(&h.session_store)),
        session_manager: Some(Arc::clone(&h.session_manager)),
        checkpoint_store: Some(h.checkpoint_store.clone() as Arc<_>),
        add_pipeline: Arc::new(locked_add_pipeline(&h, &DatasetLocks::global())),
        cognify_config: Arc::new(CognifyConfig::default()),
    });
    let launched = tokio::time::timeout(SHORT, launch)
        .await
        .expect("the launch returns at once");
    assert!(launched);
    assert!(
        !cognee::wait_for_background_tasks(Some(SHORT)).await,
        "the improve waits for the holder"
    );
    let rows = |h: &Harness| {
        let db = Arc::clone(&h.db);
        async move {
            cognee_database::DeleteDb::get_dataset_data(&*db, dataset_id)
                .await
                .unwrap()
                .len()
        }
    };
    assert_eq!(rows(&h).await, 0, "nothing ran while the dataset was held");

    drop(holder);
    assert!(cognee::wait_for_background_tasks(Some(LONG)).await);
    assert!(
        rows(&h).await > 0,
        "after the holder, the improve persisted the session's Q&A"
    );
}
