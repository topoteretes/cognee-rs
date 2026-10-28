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
    REASON_ABORTED_BY_FATAL_STAGE, REASON_BACKEND_UNSUPPORTED, REASON_BUDGET_EXHAUSTED,
    REASON_DISABLED_BY_CONFIG, REASON_NO_LLM_CONFIGURED, REASON_NO_NEW_TRACE_STEPS,
    REASON_NO_SESSION_IDS, REASON_NOT_PORTED, REASON_OPT_IN_DISABLED,
    REASON_PERSONALIZATION_DISABLED, StageResult, StageStatus, TRUTH_SUBSPACE_STAGE, improve,
};
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
    /// The curator answers HTTP 402 (an exhausted LLM budget) instead.
    curator_out_of_budget: bool,
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

// A single-session run takes a process-wide improve lock keyed by the session
// id, so every test uses its own session ids.
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
        personalization_enabled: false,
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
            ("extract_agent_context", Skipped, Some(REASON_NOT_PORTED)),
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
    let r = improve(params(
        &h,
        &config,
        "ds_keyless",
        Some(vec!["keyless_session".to_string()]),
        llm,
    ))
    .await
    .unwrap();
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

/// Stage 9 (`global_context_index`) is opt-in and needs no sessions. With
/// >= 1 graph edge, the stage formats the edges into the global session
/// graph-context.
#[tokio::test]
async fn improve_records_global_context_index_when_opted_in() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    h.graph_db
        .add_edge("node_a", "node_b", "related_to", None)
        .await
        .unwrap();
    let mut p = params(&h, &config, "ds_gcidx", None, mock_llm());
    let owner = p.owner_id;
    p.build_global_context_index = true;
    let r = improve(p).await.unwrap();

    let gci = stage(&r, "global_context_index");
    assert_eq!(gci.status, StageStatus::Completed);
    assert_eq!(r.status(), ImproveStatus::Completed);

    let stored = h
        .session_manager
        .get_graph_context(Some("_global_context_index"), Some(&owner.to_string()))
        .await
        .unwrap();
    assert_eq!(stored, Some("node_a → related_to → node_b".to_string()));
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

/// `personalization_enabled` opens the `update_user_preferences` gate; the
/// stage is not ported yet, so it reports `not_ported` instead of
/// `personalization_disabled`.
#[tokio::test]
async fn personalization_enabled_opens_the_user_preferences_gate() {
    let h = make_harness().await;
    let config = CognifyConfig::default();
    let mut p = params(
        &h,
        &config,
        "ds_personal",
        Some(vec!["personal_session".to_string()]),
        mock_llm(),
    );
    p.personalization_enabled = true;
    let r = improve(p).await.unwrap();
    assert_eq!(
        stage(&r, "update_user_preferences").reason.as_deref(),
        Some(REASON_NOT_PORTED)
    );
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
