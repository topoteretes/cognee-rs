//! Bidirectional session-graph bridge — `improve()`.
//!
//! Multi-stage pipeline matching Python `cognee.api.v1.improve.improve()`.
//! The result reports one [`StageResult`] per stage of Python's
//! `DEFAULT_STAGES` (`cognee/modules/improve/registry.py`), in order
//! ([`IMPROVE_STAGES`]):
//!
//! 1. `feedback_weights`: apply feedback weights from session Q&A entries to
//!    graph nodes/edges.
//! 2. `persist_session_qa`: persist session Q&A text into the permanent
//!    knowledge graph. The one fatal stage: an error stops the run and
//!    `improve()` returns it (Python raises, with the stages after it skipped
//!    as `aborted_by_fatal_stage`; Rust's error carries no partial result).
//! 3. `persist_agent_traces`: persist agent trace steps.
//! 4. `extract_agent_context`: not ported (reported as skipped).
//! 5. `distill_sessions`: distill each session's Q&A into curated,
//!    entity-anchored lesson documents tagged with the `session_learnings`
//!    node-set.
//! 6. `update_user_preferences`: not ported (reported as skipped).
//! 7. `build_truth_subspace` (opt-in).
//! 8. `review_conflicts`: opt-in in Python and not ported (reported as skipped).
//! 9. `global_context_index` (opt-in): not ported (reported as skipped).
//!
//! Python's triplet-enrichment stage is gone (SDK-1115), and so is Rust's.
//! So is graph-session sync (Python `3401da38a`): `improve()` no longer writes
//! a graph snapshot into sessions.
//!
//! Every stage other than `persist_session_qa` fails open: a failure in one
//! stage is recorded on its [`StageResult`] and does not abort the
//! subsequent stages (matches Python semantics).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use cognee_cognify::{
    CognifyConfig, apply_feedback_weights_pipeline, distill_sessions_in_knowledge_graph,
    persist_sessions_in_knowledge_graph,
};
use cognee_database::{PipelineRunRepository, RelationalDb};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_models::dataset_scope::maybe_with_dataset;
use cognee_ontology::OntologyResolver;
use cognee_session::{ImproveLockGuard, SessionManager, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use serde::Serialize;
use tracing::{info, warn};
use uuid::Uuid;

use super::error::ApiError;

/// Name of the truth-subspace stage, as Python's `BuildTruthSubspaceStage.name`.
pub const TRUTH_SUBSPACE_STAGE: &str = "build_truth_subspace";
/// Stage names, as Python's `*Stage.name` (`cognee/modules/improve/stages.py`).
pub const FEEDBACK_WEIGHTS_STAGE: &str = "feedback_weights";
pub const PERSIST_SESSION_QA_STAGE: &str = "persist_session_qa";
pub const PERSIST_AGENT_TRACES_STAGE: &str = "persist_agent_traces";
pub const EXTRACT_AGENT_CONTEXT_STAGE: &str = "extract_agent_context";
pub const DISTILL_SESSIONS_STAGE: &str = "distill_sessions";
pub const UPDATE_USER_PREFERENCES_STAGE: &str = "update_user_preferences";
pub const REVIEW_CONFLICTS_STAGE: &str = "review_conflicts";
pub const GLOBAL_CONTEXT_INDEX_STAGE: &str = "global_context_index";

/// Python's `DEFAULT_STAGES` names in execution order
/// (`cognee/modules/improve/registry.py:26-36`). Every [`ImproveResult`] lists
/// them all, in this order.
pub const IMPROVE_STAGES: [&str; 9] = [
    FEEDBACK_WEIGHTS_STAGE,
    PERSIST_SESSION_QA_STAGE,
    PERSIST_AGENT_TRACES_STAGE,
    EXTRACT_AGENT_CONTEXT_STAGE,
    DISTILL_SESSIONS_STAGE,
    UPDATE_USER_PREFERENCES_STAGE,
    TRUTH_SUBSPACE_STAGE,
    REVIEW_CONFLICTS_STAGE,
    GLOBAL_CONTEXT_INDEX_STAGE,
];

/// Skip reason of a stage whose collaborators (session store or manager, add
/// pipeline, relational database) were not supplied. Python's name for a
/// missing session manager (`REASON_SESSION_MANAGER_UNAVAILABLE`,
/// `cognee/modules/improve/stages.py`).
pub const REASON_SESSION_MANAGER_UNAVAILABLE: &str = "session_manager_unavailable";
/// Skip reason of a stage Python has and Rust does not port. Rust-only: a stage
/// that Python would run reports it instead of being left out of the result.
pub const REASON_NOT_IMPLEMENTED: &str = "not_implemented";
/// Skip reason of a run that lost the per-session improve lock to a run on the
/// same session already in flight. Rust-only: Python queues behind the lock.
pub const REASON_IMPROVE_IN_PROGRESS: &str = "improve_in_progress";

/// Skip reason of a session-fed stage run without session ids (Python
/// `REASON_NO_SESSION_IDS`, `cognee/modules/improve/result.py`).
pub const REASON_NO_SESSION_IDS: &str = "no_session_ids";
/// Skip reason of an opt-in stage the caller did not opt into (Python
/// `REASON_OPT_IN_DISABLED`, `cognee/modules/improve/stages.py`).
pub const REASON_OPT_IN_DISABLED: &str = "opt_in_disabled";
pub use cognee_cognify::REASON_BACKEND_UNSUPPORTED;

/// Status of one improve stage — Python's `StageStatus`
/// (`cognee/modules/improve/result.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Completed,
    AlreadyCompleted,
    Skipped,
    Errored,
}

impl StageStatus {
    /// The wire name Python uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::AlreadyCompleted => "already_completed",
            Self::Skipped => "skipped",
            Self::Errored => "errored",
        }
    }
}

/// What one improve stage did in one run — Python's `StageResult`
/// (`cognee/modules/improve/result.py:47-60`). Serialized with Python's
/// field names (`duration_ms`); Python's `run` (a pipeline's run info) is not
/// carried.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StageResult {
    pub stage: String,
    pub status: StageStatus,
    /// Why the stage was skipped; set whenever `status` is `Skipped`.
    pub reason: Option<String>,
    pub error: Option<String>,
    pub counts: BTreeMap<String, i64>,
    /// Wall time of the stage's run; `0` for a stage its gate skipped.
    pub duration_ms: u64,
}

impl StageResult {
    pub fn skipped(stage: &str, reason: &str) -> Self {
        Self::new(stage, StageStatus::Skipped, Some(reason.to_string()), None)
    }

    pub fn errored(stage: &str, error: impl Into<String>) -> Self {
        Self::new(stage, StageStatus::Errored, None, Some(error.into()))
    }

    pub fn completed(stage: &str) -> Self {
        Self::new(stage, StageStatus::Completed, None, None)
    }

    fn new(
        stage: &str,
        status: StageStatus,
        reason: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            stage: stage.to_string(),
            status,
            reason,
            error,
            counts: BTreeMap::new(),
            duration_ms: 0,
        }
    }
}

/// Map a truth-subspace build onto its stage result, as Python's
/// `BuildTruthSubspaceStage.run` does (`cognee/modules/improve/stages.py:491-520`).
///
/// The build fails open — a failed centroid commit or chunk write comes back
/// as `Errored` instead of an error — so the status is translated rather than
/// reported as completed, which would hide the failure. Counts are the
/// build's integer fields.
fn truth_subspace_stage_result(build: &cognee_cognify::TruthSubspaceResult) -> StageResult {
    use cognee_cognify::TruthBuildStatus;

    let counts = || {
        BTreeMap::from([
            ("anchors".to_string(), build.anchors as i64),
            ("nodes_scored".to_string(), build.nodes_scored as i64),
            ("nodes_skipped".to_string(), build.nodes_skipped as i64),
            ("truth_epoch".to_string(), build.truth_epoch),
        ])
    };
    match build.status {
        TruthBuildStatus::Errored => StageResult {
            counts: counts(),
            ..StageResult::errored(
                TRUTH_SUBSPACE_STAGE,
                build
                    .error
                    .clone()
                    .unwrap_or_else(|| "truth subspace build errored".to_string()),
            )
        },
        // Normally unreachable: the stage gate checks the same capability
        // first. Reachable by a mid-run capability change; the mapping should
        // not lie about it.
        TruthBuildStatus::Skipped => StageResult::skipped(
            TRUTH_SUBSPACE_STAGE,
            build.reason.as_deref().unwrap_or("skipped"),
        ),
        TruthBuildStatus::Completed => StageResult {
            counts: counts(),
            ..StageResult::completed(TRUTH_SUBSPACE_STAGE)
        },
    }
}

/// Overall status of an improve run — Python's `ImproveStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImproveStatus {
    Running,
    Errored,
    Skipped,
    Completed,
}

impl ImproveStatus {
    /// The wire name Python uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Errored => "errored",
            Self::Skipped => "skipped",
            Self::Completed => "completed",
        }
    }
}

/// Result of an `improve()` operation — Python's `ImproveResult`
/// (`cognee/modules/improve/result.py:212-245`): one [`StageResult`] per stage
/// in [`IMPROVE_STAGES`] order, plus the run's identity and its derived
/// [`status`](Self::status). Serialized with Python's field names, including
/// the computed `status`.
#[derive(Debug, Clone, Serialize)]
#[serde(into = "ImproveResultWire")]
pub struct ImproveResult {
    pub dataset_id: Option<Uuid>,
    pub dataset_name: Option<String>,
    pub session_ids: Vec<String>,
    /// One entry per stage, in [`IMPROVE_STAGES`] order. Empty only for a
    /// result that no run filled.
    pub stages: Vec<StageResult>,
    /// Always `false`: Rust runs improve inline.
    pub background: bool,
    /// Always `true` for a returned result: Rust runs improve inline.
    pub finished: bool,
    /// Set when a fatal stage aborted the run. A foreground run returns the
    /// error instead, so a returned result has none.
    pub error: Option<String>,
}

impl Default for ImproveResult {
    fn default() -> Self {
        Self {
            dataset_id: None,
            dataset_name: None,
            session_ids: Vec::new(),
            stages: Vec::new(),
            background: false,
            finished: true,
            error: None,
        }
    }
}

impl ImproveResult {
    /// Summary of the stages, as Python's `ImproveResult.status`.
    pub fn status(&self) -> ImproveStatus {
        if !self.finished {
            ImproveStatus::Running
        } else if self.error.is_some()
            || self.stages.iter().any(|s| s.status == StageStatus::Errored)
        {
            ImproveStatus::Errored
        } else if !self.stages.is_empty()
            && self.stages.iter().all(|s| s.status == StageStatus::Skipped)
        {
            ImproveStatus::Skipped
        } else {
            ImproveStatus::Completed
        }
    }

    /// The stage named `name`, as Python's `ImproveResult.stage`.
    pub fn stage(&self, name: &str) -> Option<&StageResult> {
        self.stages.iter().find(|s| s.stage == name)
    }

    /// Why the run went wrong, if it did, as Python's `_improve_error_text`
    /// (`cognee/api/v1/remember/remember.py:73-91`): the fatal error, else the
    /// errored stages as `"<stage>: <error>"` joined by `"; "`.
    pub fn error_text(&self) -> Option<String> {
        if let Some(error) = &self.error {
            return Some(error.clone());
        }
        let errored: Vec<String> = self
            .stages
            .iter()
            .filter(|s| s.status == StageStatus::Errored)
            .map(|s| format!("{}: {}", s.stage, s.error.as_deref().unwrap_or("errored")))
            .collect();
        (!errored.is_empty()).then(|| errored.join("; "))
    }

    /// `name=status` pairs, comma-joined, as Python's `stage_summary`.
    pub fn stage_summary(&self) -> String {
        self.stages
            .iter()
            .map(|s| format!("{}={}", s.stage, s.status.as_str()))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// The serialized shape of [`ImproveResult`]: its fields plus `status`.
#[derive(Serialize)]
struct ImproveResultWire {
    dataset_id: Option<Uuid>,
    dataset_name: Option<String>,
    session_ids: Vec<String>,
    stages: Vec<StageResult>,
    background: bool,
    finished: bool,
    error: Option<String>,
    status: ImproveStatus,
}

impl From<ImproveResult> for ImproveResultWire {
    fn from(r: ImproveResult) -> Self {
        let status = r.status();
        Self {
            dataset_id: r.dataset_id,
            dataset_name: r.dataset_name,
            session_ids: r.session_ids,
            stages: r.stages,
            background: r.background,
            finished: r.finished,
            error: r.error,
            status,
        }
    }
}

/// Parameters for [`improve`].
///
/// All fields are required at construction time — `Default` is intentionally
/// not derived because several fields (`owner_id`, the engine handles, and
/// `cognify_config`) have no sensible default value. This forces every caller
/// to think about each dependency. Callers that omit optional behavior should
/// pass `None` explicitly for the `Option<...>` fields.
///
/// LIB-04 (Decision 8) introduced this struct to replace the previous 18
/// positional parameters. Python's deprecated custom-task and scoping options
/// (`extraction_tasks`, `enrichment_tasks`, `data`, `node_name`, `node_type`,
/// `vector_db_config`, `graph_db_config`) are not carried (SDK-1115): a REST
/// client that still sends them has them ignored by the HTTP layer.
pub struct ImproveParams<'a> {
    /// Dataset name to operate on (Stage 2 persistence).
    pub dataset_name: String,
    /// Session ids that drive Stages 1, 2, and 4. `None` or empty skips them.
    pub session_ids: Option<Vec<String>>,
    /// Owner UUID under which graph/session reads and writes are scoped.
    pub owner_id: Uuid,
    /// Optional tenant UUID for multi-tenant deployments.
    pub tenant_id: Option<Uuid>,
    /// Mixing factor for feedback weight propagation (Stage 1).
    pub feedback_alpha: f64,

    /// When `true` and not running in background, build the global context
    /// index (graph summary) after the session stages.
    ///
    /// Mirrors Python's `build_global_context_index` parameter.
    /// Default `false` (opt-in) — matches Python parity.
    pub build_global_context_index: bool,

    /// When `true` and sessions are present, build/refresh the truth-subspace
    /// centroid slots and per-chunk alignment (Stage 2d, Phase-2 join point).
    ///
    /// Mirrors Python's `build_truth_subspace` parameter. Default `false`
    /// (opt-in) — the entire truth-subspace path is inert unless this is set.
    pub build_truth_subspace: bool,

    /// When `true`, treat this as a background run: skips stages that
    /// require the prior stage to have completed synchronously (e.g. the
    /// global context index and the sync-graph stage).
    ///
    /// Background dispatch is handled by the host (HTTP server or CLI);
    /// this flag is used only for stage-skipping logic parity with Python.
    pub run_in_background: bool,

    /// LLM handle (used by Stage 2 cognify-of-session-text).
    pub llm: Arc<dyn Llm>,
    /// File storage handle.
    pub storage: Arc<dyn StorageTrait>,
    /// Graph database handle.
    pub graph_db: Arc<dyn GraphDBTrait>,
    /// Vector database handle.
    pub vector_db: Arc<dyn VectorDB>,
    /// Embedding engine handle.
    pub embedding_engine: Arc<dyn EmbeddingEngine>,
    /// Ontology resolver handle.
    pub ontology_resolver: Arc<dyn OntologyResolver>,

    /// Metadata DB connection. Dataset lookup.
    pub db: Option<Arc<dyn RelationalDb>>,
    /// Session backing store. Required for Stages 1 and 2.
    pub session_store: Option<Arc<dyn SessionStore>>,
    /// Session manager. Required for Stage 1.
    pub session_manager: Option<Arc<SessionManager>>,
    /// Add pipeline (borrowed). Required for Stage 2.
    pub add_pipeline: Option<&'a AddPipeline>,

    /// Borrowed cognify configuration used by Stage 2 persistence.
    pub cognify_config: &'a CognifyConfig,
}

/// Bidirectional session-graph bridge.
///
/// Background dispatch is a host-side concern — this function is strictly
/// synchronous.
///
/// All inputs are passed via [`ImproveParams`] (see Decision 8 / LIB-04).
/// The id of the owner's dataset `name` in `tenant`, or `None` when there is
/// no relational database or no such dataset. Names resolve among the owner's
/// own datasets only, as Python's `get_dataset_ids` does; a failed lookup is
/// an error, never a silent run of the graph stages outside the dataset.
async fn dataset_id_by_name(
    db: Option<&Arc<dyn RelationalDb>>,
    name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    let Some(db) = db else {
        return Ok(None);
    };
    let dataset =
        cognee_database::IngestDb::get_dataset_by_name_in_tenant(&**db, name, owner_id, tenant_id)
            .await
            .map_err(|e| ApiError::Improve(format!("dataset lookup failed for '{name}': {e}")))?;
    Ok(dataset.map(|ds| ds.id))
}

/// Whole milliseconds since `started_at`, saturating.
fn elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The error `improve()` returns when the fatal `persist_session_qa` stage
/// errors. Python raises it from `execute_stages` and skips the remaining
/// stages with `aborted_by_fatal_stage`; a caller of Rust's `improve()` gets
/// the error and no partial result.
fn fatal_stage_error(error: String) -> ApiError {
    warn!("improve: fatal stage '{PERSIST_SESSION_QA_STAGE}' failed, run stopped: {error}");
    ApiError::Improve(format!("{PERSIST_SESSION_QA_STAGE}: {error}"))
}

pub async fn improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError> {
    let ImproveParams {
        dataset_name,
        session_ids,
        owner_id,
        tenant_id,
        feedback_alpha,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        ontology_resolver,
        db,
        session_store,
        session_manager,
        add_pipeline,
        cognify_config,
        build_global_context_index,
        build_truth_subspace,
        run_in_background,
    } = params;

    // ---- Improve lock (parity with Python session_lock.py:136-150) ----
    //
    // When exactly one session is targeted, acquire a per-session lock so
    // that concurrent `improve()` calls on the same session don't duplicate
    // work (e.g. auto-improve + idle-watcher + SessionEnd firing at once).
    // Multi-session improves skip the lock — the pattern is rare and locking
    // N sessions atomically is messy (matches Python comment verbatim).
    //
    // The guard holds a `String`, not a `MutexGuard`, so it is Send-safe
    // across `.await` points.
    let _improve_guard = if let Some(ref sids) = session_ids {
        if sids.len() == 1 {
            match ImproveLockGuard::acquire(&sids[0]) {
                Some(g) => Some(g),
                None => {
                    info!(
                        session_id = %sids[0],
                        "improve: session already being improved, skipping"
                    );
                    // Nothing ran: every stage is skipped, so the result's
                    // status is `skipped`, as Python's docs describe a lost lock.
                    return Ok(ImproveResult {
                        session_ids: sids.clone(),
                        dataset_name: Some(dataset_name),
                        stages: IMPROVE_STAGES
                            .iter()
                            .map(|name| StageResult::skipped(name, REASON_IMPROVE_IN_PROGRESS))
                            .collect(),
                        ..ImproveResult::default()
                    });
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let mut result = ImproveResult {
        dataset_name: Some(dataset_name.clone()),
        session_ids: session_ids.clone().unwrap_or_default(),
        ..ImproveResult::default()
    };
    // One slot per stage, filled as the stages run and read out in registry
    // order at the end. A slot nothing filled is a gate skip.
    let mut stage_slots: BTreeMap<&'static str, StageResult> = BTreeMap::new();
    let has_sessions = session_ids.as_ref().is_some_and(|ids| !ids.is_empty());

    // Wrap the body in a `cognee.api.improve` OTEL span for parity with
    // Python's `cognee.api.v1.improve.improve()` (gap 03 / task 03-07).
    // Attribute names mirror the analytics payload below and the Python
    // span's verbose names (`dataset`, `session_count`, `run_in_background`).
    let session_count = session_ids.as_ref().map(|v| v.len()).unwrap_or(0);
    let span = tracing::info_span!(
        "cognee.api.improve",
        dataset = %dataset_name,
        session_count = session_count,
        run_in_background = false,
    );
    let _enter = span.enter();

    // Mirrors Python `send_telemetry("cognee.improve", ...)` from
    // cognee/api/v1/improve/improve.py:91.
    #[cfg(feature = "telemetry")]
    {
        cognee_telemetry::send_telemetry(
            "cognee.improve",
            owner_id,
            Some(serde_json::json!({
                "dataset": dataset_name.clone(),
                "session_count": session_count,
                "session_ids": session_ids.clone(),
                "run_in_background": false,
                "cognee_version": env!("CARGO_PKG_VERSION"),
            })),
        );
    }

    // The dataset the graph-reading stages (1, 3) run in. Under per-dataset
    // storage (`ENABLE_BACKEND_ACCESS_CONTROL`) the graph they read and write
    // is that dataset's own, as Python's improve stages run in the dataset's database
    // context. Stages 2–2d scope themselves (add, cognify, truth subspace). A
    // dataset that does not exist yet has no graph to weight; stage 2 may
    // create it, so the later stages look it up again.
    let stage1_dataset =
        dataset_id_by_name(db.as_ref(), &dataset_name, owner_id, tenant_id).await?;

    // ---- Stage 1: Apply Feedback Weights ----
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        let started_at = Instant::now();
        let mut stage = match (session_store.as_ref(), session_manager.as_ref()) {
            (Some(store), Some(mgr)) => {
                match maybe_with_dataset(
                    stage1_dataset,
                    apply_feedback_weights_pipeline(
                        sids,
                        owner_id,
                        feedback_alpha,
                        &*graph_db,
                        Arc::clone(store),
                        Arc::clone(mgr),
                    ),
                )
                .await
                {
                    Ok(r) => {
                        info!(
                            processed = r.processed,
                            applied = r.applied,
                            skipped = r.skipped,
                            "improve stage 1 (feedback_weights) complete"
                        );
                        StageResult {
                            counts: BTreeMap::from([
                                ("sessions".to_string(), sids.len() as i64),
                                ("entries_processed".to_string(), r.processed as i64),
                                ("entries_applied".to_string(), r.applied as i64),
                            ]),
                            ..StageResult::completed(FEEDBACK_WEIGHTS_STAGE)
                        }
                    }
                    Err(e) => {
                        warn!("improve stage 1 (feedback_weights) failed (non-fatal): {e}");
                        StageResult::errored(FEEDBACK_WEIGHTS_STAGE, e.to_string())
                    }
                }
            }
            _ => {
                warn!(
                    "improve stage 1: session_store and session_manager are required; skipping feedback_weights"
                );
                StageResult::skipped(FEEDBACK_WEIGHTS_STAGE, REASON_SESSION_MANAGER_UNAVAILABLE)
            }
        };
        stage.duration_ms = elapsed_ms(started_at);
        stage_slots.insert(FEEDBACK_WEIGHTS_STAGE, stage);
    }

    // ---- Stage 2: Persist Session Q&A to Graph ----
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        // LIB-06-03: `persist_sessions_in_knowledge_graph` now requires
        // `Arc<dyn RelationalDb>` and `Arc<dyn CpuPool>`.
        let stage2_db = db.clone();
        let started_at = Instant::now();
        let mut stage = match (session_store.as_ref(), add_pipeline, stage2_db) {
            (Some(store), Some(pipeline), Some(database)) => {
                let thread_pool: Arc<dyn cognee_core::CpuPool> =
                    match cognee_core::RayonThreadPool::with_default_threads() {
                        Ok(pool) => Arc::new(pool),
                        Err(e) => {
                            warn!(
                                "improve stage 2: failed to construct thread pool: {e}; skipping persist_sessions"
                            );
                            return Err(fatal_stage_error(format!(
                                "failed to construct thread pool: {e}"
                            )));
                        }
                    };
                let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                    Arc::clone(&database).pipeline_runs();
                match persist_sessions_in_knowledge_graph(
                    sids,
                    &dataset_name,
                    owner_id,
                    tenant_id,
                    Arc::clone(store),
                    pipeline,
                    Arc::clone(&llm),
                    Arc::clone(&storage),
                    Arc::clone(&graph_db),
                    Arc::clone(&vector_db),
                    Arc::clone(&embedding_engine),
                    database,
                    pipeline_run_repo,
                    thread_pool,
                    Arc::clone(&ontology_resolver),
                    cognify_config,
                )
                .await
                {
                    Ok(r) => {
                        info!(
                            persisted = r.sessions_persisted,
                            skipped = r.sessions_skipped,
                            failed = r.sessions_failed,
                            "improve stage 2 (persist_sessions) complete"
                        );
                        let counts = BTreeMap::from([
                            ("sessions".to_string(), sids.len() as i64),
                            (
                                "sessions_persisted".to_string(),
                                r.sessions_persisted as i64,
                            ),
                            ("sessions_skipped".to_string(), r.sessions_skipped as i64),
                            ("sessions_failed".to_string(), r.sessions_failed as i64),
                        ]);
                        if r.sessions_failed > 0 {
                            StageResult {
                                counts,
                                ..StageResult::errored(
                                    PERSIST_SESSION_QA_STAGE,
                                    format!("{} session(s) failed to persist", r.sessions_failed),
                                )
                            }
                        } else {
                            StageResult {
                                counts,
                                ..StageResult::completed(PERSIST_SESSION_QA_STAGE)
                            }
                        }
                    }
                    Err(e) => {
                        warn!("improve stage 2 (persist_session_qa) failed: {e}");
                        StageResult::errored(PERSIST_SESSION_QA_STAGE, e.to_string())
                    }
                }
            }
            _ => {
                warn!(
                    "improve stage 2: session_store, add_pipeline, and the relational database are required; skipping persist_session_qa"
                );
                StageResult::skipped(PERSIST_SESSION_QA_STAGE, REASON_SESSION_MANAGER_UNAVAILABLE)
            }
        };
        stage.duration_ms = elapsed_ms(started_at);
        // The one fatal stage: losing session Q&A would be data loss, so an
        // error stops the run (Python `stage.fatal`, `improve.py:222-246`).
        if stage.status == StageStatus::Errored {
            let error = stage.error.clone().unwrap_or_else(|| "errored".to_string());
            return Err(fatal_stage_error(error));
        }
        stage_slots.insert(PERSIST_SESSION_QA_STAGE, stage);
    }

    // ---- Stage 2b: Persist Agent Trace Steps ----
    //
    // Mirrors Python's `_persist_session_traces` (improve.py:166-176).
    // Reads `session_feedback` from each trace step and cognifies it into the
    // permanent graph so that the plugin's tool-call activity reaches permanent
    // memory — not just QA entries.
    //
    // Scoped-down 0.1.0 implementation: collects trace `session_feedback` text
    // (the per-step LLM-generated feedback string) and runs it through the
    // add→cognify path with node_set `"agent_trace_feedbacks"`.
    //
    // TODO(parity): Python's `persist_agent_trace_feedbacks_in_knowledge_graph_pipeline`
    // uses per-step metadata (origin_function, status, method_params). The full
    // parity pass should introduce a dedicated `persist_trace_feedbacks_in_knowledge_graph`
    // function in cognee-cognify that preserves per-step provenance.
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");

        // Collect all trace feedback texts across the sessions.
        let started_at = Instant::now();
        let mut trace_error: Option<String> = None;
        let mut trace_texts: Vec<String> = Vec::new();
        if let Some(mgr) = session_manager.as_ref() {
            let user_id_str = owner_id.to_string();
            for sid in sids {
                match mgr
                    .get_agent_trace_session(&user_id_str, Some(sid), None)
                    .await
                {
                    Ok(steps) => {
                        for step in &steps {
                            if !step.session_feedback.is_empty() {
                                trace_texts.push(format!(
                                    "Session: {sid}\nFunction: {}\nStatus: {}\nFeedback: {}",
                                    step.origin_function, step.status, step.session_feedback,
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            session_id = sid,
                            "improve stage 2b: could not read trace steps (non-fatal): {e}"
                        );
                        trace_error = Some(format!("could not read trace steps of '{sid}': {e}"));
                    }
                }
            }
        }

        if !trace_texts.is_empty() {
            let stage2b_db = db.clone();
            let combined_text = trace_texts.join("\n\n");
            match (add_pipeline, stage2b_db) {
                (Some(pipeline), Some(database)) => {
                    match cognee_core::RayonThreadPool::with_default_threads() {
                        Ok(pool) => {
                            let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(pool);
                            let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                                Arc::clone(&database).pipeline_runs();
                            let add_params = AddParams {
                                node_set: Some(vec!["agent_trace_feedbacks".to_string()]),
                                ..Default::default()
                            };
                            match pipeline
                                .add_with_params(
                                    vec![DataInput::Text(combined_text)],
                                    &dataset_name,
                                    owner_id,
                                    tenant_id,
                                    &add_params,
                                )
                                .await
                                .map_err(|e| e.to_string())
                            {
                                Ok(data_rows) if !data_rows.is_empty() => {
                                    // Resolve dataset_id the same way persist_sessions does.
                                    let dataset_id_opt =
                                        cognee_database::IngestDb::get_dataset_by_name_in_tenant(
                                            &*database,
                                            &dataset_name,
                                            owner_id,
                                            tenant_id,
                                        )
                                        .await
                                        .ok()
                                        .flatten()
                                        .map(|ds| ds.id);
                                    if let Some(dataset_id) = dataset_id_opt {
                                        match cognee_cognify::tasks::cognify(
                                            data_rows,
                                            dataset_id,
                                            Some(owner_id),
                                            None,
                                            tenant_id,
                                            Arc::clone(&llm),
                                            Arc::clone(&storage),
                                            Arc::clone(&graph_db),
                                            Arc::clone(&vector_db),
                                            Arc::clone(&embedding_engine),
                                            Arc::clone(&database),
                                            pipeline_run_repo,
                                            thread_pool,
                                            Arc::clone(&ontology_resolver),
                                            cognify_config,
                                        )
                                        .await
                                        {
                                            Ok(_) => {
                                                info!(
                                                    trace_items = trace_texts.len(),
                                                    "improve stage 2b (persist_trace_steps) complete"
                                                );
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "improve stage 2b: cognify of trace steps failed (non-fatal): {e}"
                                                );
                                                trace_error = Some(format!(
                                                    "cognify of trace steps failed: {e}"
                                                ));
                                            }
                                        }
                                    } else {
                                        warn!(
                                            "improve stage 2b: dataset lookup returned None; trace steps not cognified"
                                        );
                                        trace_error = Some(
                                            "dataset lookup returned none; trace steps not cognified"
                                                .to_string(),
                                        );
                                    }
                                }
                                Ok(_) => {
                                    warn!(
                                        "improve stage 2b: add returned no rows; trace steps not cognified"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "improve stage 2b: add of trace text failed (non-fatal): {e}"
                                    );
                                    trace_error = Some(format!("add of trace text failed: {e}"));
                                }
                            }
                        }
                        Err(e) => {
                            warn!("improve stage 2b: rayon pool init failed (non-fatal): {e}");
                            trace_error = Some(format!("thread pool init failed: {e}"));
                        }
                    }
                }
                _ => {
                    warn!(
                        "improve stage 2b: add_pipeline and the relational database are required; trace steps not cognified"
                    );
                    trace_error =
                        Some("add pipeline and relational database are required".to_string());
                }
            }
        }
        let counts = BTreeMap::from([
            ("sessions".to_string(), sids.len() as i64),
            ("trace_items".to_string(), trace_texts.len() as i64),
        ]);
        let mut stage = match trace_error {
            Some(error) => StageResult {
                counts,
                ..StageResult::errored(PERSIST_AGENT_TRACES_STAGE, error)
            },
            None => StageResult {
                counts,
                ..StageResult::completed(PERSIST_AGENT_TRACES_STAGE)
            },
        };
        stage.duration_ms = elapsed_ms(started_at);
        stage_slots.insert(PERSIST_AGENT_TRACES_STAGE, stage);
    }

    // ---- Stage 2c: Distill Sessions into Learnings ----
    //
    // Mirrors Python's `_distill_sessions` (improve.py:190-201). Each session's
    // Q&A is curated into durable, entity-anchored lesson documents tagged with
    // the `session_learnings` node-set and cognified into the graph. Fail-open
    // per session; a session with no non-empty Q&A yields nothing.
    //
    // Scope cut vs. Python (locked): only Q&A is consumed — there is no
    // `SessionContextEntry` gated-guidance corpus in Rust — and Python's
    // `extract_agent_context` stage is intentionally NOT ported.
    //
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        let stage2c_db = db.clone();
        let started_at = Instant::now();
        let mut stage = match (session_store.as_ref(), add_pipeline, stage2c_db) {
            (Some(store), Some(pipeline), Some(database)) => {
                match cognee_core::RayonThreadPool::with_default_threads() {
                    Ok(pool) => {
                        let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(pool);
                        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                            Arc::clone(&database).pipeline_runs();
                        let r = distill_sessions_in_knowledge_graph(
                            sids,
                            &dataset_name,
                            owner_id,
                            tenant_id,
                            Arc::clone(store),
                            pipeline,
                            Arc::clone(&llm),
                            Arc::clone(&storage),
                            Arc::clone(&graph_db),
                            Arc::clone(&vector_db),
                            Arc::clone(&embedding_engine),
                            database,
                            pipeline_run_repo,
                            thread_pool,
                            Arc::clone(&ontology_resolver),
                            cognify_config,
                        )
                        .await;
                        info!(
                            sessions_distilled = r.sessions_distilled,
                            lessons_published = r.lessons_published,
                            "improve stage 2c (distill_sessions) complete"
                        );
                        StageResult {
                            counts: BTreeMap::from([
                                ("documents".to_string(), r.lessons_published as i64),
                                (
                                    "sessions_completed".to_string(),
                                    r.sessions_distilled as i64,
                                ),
                            ]),
                            ..StageResult::completed(DISTILL_SESSIONS_STAGE)
                        }
                    }
                    Err(e) => {
                        warn!(
                            "improve stage 2c: rayon pool init failed (non-fatal): {e}; skipping distill_sessions"
                        );
                        StageResult::errored(
                            DISTILL_SESSIONS_STAGE,
                            format!("thread pool init failed: {e}"),
                        )
                    }
                }
            }
            _ => {
                warn!(
                    "improve stage 2c: session_store, add_pipeline, and the relational database are required; skipping distill_sessions"
                );
                StageResult::skipped(DISTILL_SESSIONS_STAGE, REASON_SESSION_MANAGER_UNAVAILABLE)
            }
        };
        stage.duration_ms = elapsed_ms(started_at);
        stage_slots.insert(DISTILL_SESSIONS_STAGE, stage);
    }

    // ---- Stage 2d: Build Truth Subspace (opt-in) ----
    //
    // Mirrors Python's `BuildTruthSubspaceStage`
    // (`cognee/modules/improve/stages.py:478-520`) and the orchestrator's gate
    // order (`cognee/modules/improve/stage.py`): no session ids, then the
    // opt-in flag, then the backend's truth-state support. Fully inert (zero
    // graph/vector I/O) unless the caller opts in. Non-fatal: a failed build is
    // recorded as an errored stage and never aborts improve().
    let truth_stage = if !has_sessions {
        StageResult::skipped(TRUTH_SUBSPACE_STAGE, REASON_NO_SESSION_IDS)
    } else if !build_truth_subspace {
        StageResult::skipped(TRUTH_SUBSPACE_STAGE, REASON_OPT_IN_DISABLED)
    } else {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        let started_at = Instant::now();
        // Python times every stage whose `run()` was entered
        // (`cognee/modules/improve/stage.py`), whatever it returned; only a
        // gate skip (here: `backend_unsupported`) carries no duration.
        let mut timed = true;
        let mut stage = match db.as_ref() {
            None => {
                warn!("improve stage 2d: relational database required; not building");
                StageResult::errored(TRUTH_SUBSPACE_STAGE, "relational database required")
            }
            // Resolve the dataset by name.
            Some(database) => match cognee_database::IngestDb::get_dataset_by_name_in_tenant(
                &**database,
                &dataset_name,
                owner_id,
                tenant_id,
            )
            .await
            {
                // Python's build returns its empty result for a dataset it
                // cannot resolve, which the stage reports as completed.
                Ok(None) => {
                    warn!("improve stage 2d: dataset not found; nothing to build");
                    truth_subspace_stage_result(&cognee_cognify::TruthSubspaceResult::default())
                }
                Err(e) => {
                    warn!("improve stage 2d: dataset lookup failed (non-fatal): {e}");
                    StageResult::errored(
                        TRUTH_SUBSPACE_STAGE,
                        format!("dataset lookup failed: {e}"),
                    )
                }
                Ok(Some(ds)) => {
                    // Probe inside the dataset's scope: a per-dataset router
                    // answers for that dataset's engine.
                    let supported = cognee_models::dataset_scope::with_dataset(
                        ds.id,
                        graph_db.supports_truth_state(),
                    )
                    .await;
                    match supported {
                        Ok(true) => {
                            let ts = cognee_cognify::build_truth_subspace(
                                ds.id,
                                sids,
                                Arc::clone(&graph_db),
                                Arc::clone(&vector_db),
                                Arc::clone(&embedding_engine),
                                cognee_cognify::DEFAULT_K,
                            )
                            .await;
                            info!(
                                status = ts.status.as_str(),
                                anchors = ts.anchors,
                                nodes_scored = ts.nodes_scored,
                                nodes_skipped = ts.nodes_skipped,
                                truth_epoch = ts.truth_epoch,
                                error = ts.error.as_deref().unwrap_or(""),
                                "improve stage 2d (build_truth_subspace) finished"
                            );
                            truth_subspace_stage_result(&ts)
                        }
                        // The dataset's engine could not be reached: Python's
                        // `get_graph_engine()` raising inside `run()`.
                        Err(e) => {
                            warn!("improve stage 2d: graph engine unavailable: {e}");
                            StageResult::errored(
                                TRUTH_SUBSPACE_STAGE,
                                format!("graph engine unavailable: {e}"),
                            )
                        }
                        Ok(false) => {
                            timed = false;
                            info!(
                                "improve stage 2d: graph backend has no truth-state support; skipping"
                            );
                            StageResult::skipped(TRUTH_SUBSPACE_STAGE, REASON_BACKEND_UNSUPPORTED)
                        }
                    }
                }
            },
        };
        if timed {
            stage.duration_ms = elapsed_ms(started_at);
        }
        stage
    };
    stage_slots.insert(TRUTH_SUBSPACE_STAGE, truth_stage);

    // ---- Stages Rust does not port ----
    //
    // `extract_agent_context`, `update_user_preferences`, `review_conflicts`
    // and `global_context_index` have no Rust counterpart. They report as
    // Python does until the run-level gates stop them: `no_session_ids` for a
    // session-fed stage run without sessions, `opt_in_disabled` for an opt-in
    // stage the caller did not request (Rust has no `review_conflicts` opt-in,
    // so it never gets past that gate). A stage that would run reports
    // `not_implemented`.
    //
    // Python's `global_context_index_pipeline` builds bucket and root
    // summaries with an LLM pass. The stand-in Rust had stored a flat edge
    // list in the session graph-context snapshot, which Python removed with
    // graph-session sync (`3401da38a`) and which nothing read, so the
    // stand-in is gone too.
    // TODO(parity): implement `global_context_index_pipeline`
    // (`bucketing_strategy="graph"`, `max_bucket_size=4`).
    if build_global_context_index {
        warn!(
            run_in_background,
            "improve: global_context_index is not implemented; skipping"
        );
    }
    let unported_skip = |needs_sessions: bool, opted_in: bool| -> &'static str {
        if needs_sessions && !has_sessions {
            REASON_NO_SESSION_IDS
        } else if !opted_in {
            REASON_OPT_IN_DISABLED
        } else {
            REASON_NOT_IMPLEMENTED
        }
    };
    for (name, needs_sessions, opted_in) in [
        (EXTRACT_AGENT_CONTEXT_STAGE, true, true),
        (UPDATE_USER_PREFERENCES_STAGE, true, true),
        (REVIEW_CONFLICTS_STAGE, false, false),
        (
            GLOBAL_CONTEXT_INDEX_STAGE,
            false,
            build_global_context_index,
        ),
    ] {
        stage_slots.insert(
            name,
            StageResult::skipped(name, unported_skip(needs_sessions, opted_in)),
        );
    }

    // Registry order. A stage nothing filled was gated off: Python's
    // `no_session_ids` for the session-fed stages run without sessions.
    result.stages = IMPROVE_STAGES
        .iter()
        .map(|name| {
            stage_slots
                .remove(name)
                .unwrap_or_else(|| StageResult::skipped(name, REASON_NO_SESSION_IDS))
        })
        .collect();
    result.dataset_id = dataset_id_by_name(db.as_ref(), &dataset_name, owner_id, tenant_id)
        .await
        .ok()
        .flatten();

    Ok(result)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn improve_result_default_fields() {
        let result = ImproveResult::default();
        assert!(result.stages.is_empty());
        assert!(result.finished && !result.background);
        assert_eq!(result.error, None);
    }

    fn result_with(stages: Vec<StageResult>) -> ImproveResult {
        ImproveResult {
            stages,
            ..ImproveResult::default()
        }
    }

    // Python `ImproveResult.status`: errored beats skipped beats completed.
    #[test]
    fn status_summarises_the_stages_as_python_does() {
        let skipped = StageResult::skipped("a", REASON_NO_SESSION_IDS);
        let done = StageResult::completed("b");
        let failed = StageResult::errored("c", "boom");
        assert_eq!(result_with(vec![]).status(), ImproveStatus::Completed);
        assert_eq!(
            result_with(vec![skipped.clone()]).status(),
            ImproveStatus::Skipped
        );
        assert_eq!(
            result_with(vec![skipped.clone(), done.clone()]).status(),
            ImproveStatus::Completed
        );
        assert_eq!(
            result_with(vec![skipped, done, failed]).status(),
            ImproveStatus::Errored
        );
        let mut aborted = result_with(vec![StageResult::completed("a")]);
        aborted.error = Some("fatal".to_string());
        assert_eq!(aborted.status(), ImproveStatus::Errored);
        let running = ImproveResult {
            finished: false,
            ..ImproveResult::default()
        };
        assert_eq!(running.status(), ImproveStatus::Running);
    }

    // Python `_improve_error_text`: the fatal error first, else the errored
    // stages as "<stage>: <error>".
    #[test]
    fn error_text_matches_python() {
        let ok = result_with(vec![StageResult::completed("a")]);
        assert_eq!(ok.error_text(), None);
        let two = result_with(vec![
            StageResult::errored("distill_sessions", "no llm"),
            StageResult::completed("b"),
            StageResult::errored("feedback_weights", "down"),
        ]);
        assert_eq!(
            two.error_text().as_deref(),
            Some("distill_sessions: no llm; feedback_weights: down")
        );
        let mut fatal = two;
        fatal.error = Some("fatal".to_string());
        assert_eq!(fatal.error_text().as_deref(), Some("fatal"));
    }

    // The registry order and names are Python's `DEFAULT_STAGES`.
    #[test]
    fn stage_names_are_pythons_in_order() {
        assert_eq!(
            IMPROVE_STAGES,
            [
                "feedback_weights",
                "persist_session_qa",
                "persist_agent_traces",
                "extract_agent_context",
                "distill_sessions",
                "update_user_preferences",
                "build_truth_subspace",
                "review_conflicts",
                "global_context_index",
            ]
        );
    }

    // The serialized form carries Python's field names and the computed status.
    #[test]
    fn serializes_with_python_field_names() {
        let result = ImproveResult {
            dataset_name: Some("ds".to_string()),
            session_ids: vec!["s1".to_string()],
            stages: vec![StageResult::skipped(
                "feedback_weights",
                REASON_NO_SESSION_IDS,
            )],
            ..ImproveResult::default()
        };
        let v = serde_json::to_value(&result).expect("serialize");
        assert_eq!(v["status"], "skipped");
        assert_eq!(v["dataset_name"], "ds");
        assert_eq!(v["session_ids"], serde_json::json!(["s1"]));
        assert_eq!(v["finished"], true);
        assert_eq!(v["background"], false);
        assert_eq!(v["error"], serde_json::Value::Null);
        let stage = &v["stages"][0];
        assert_eq!(stage["stage"], "feedback_weights");
        assert_eq!(stage["status"], "skipped");
        assert_eq!(stage["reason"], "no_session_ids");
        assert_eq!(stage["duration_ms"], 0);
        assert_eq!(stage["counts"], serde_json::json!({}));
    }

    fn build(status: cognee_cognify::TruthBuildStatus) -> cognee_cognify::TruthSubspaceResult {
        cognee_cognify::TruthSubspaceResult {
            anchors: 3,
            nodes_scored: 5,
            nodes_skipped: 2,
            signature: "sig".to_string(),
            truth_epoch: 4,
            status,
            reason: None,
            error: None,
        }
    }

    // Python `BuildTruthSubspaceStage.run`: the build's integer fields become
    // the stage counts.
    #[test]
    fn completed_build_maps_to_a_completed_stage_with_counts() {
        let stage =
            truth_subspace_stage_result(&build(cognee_cognify::TruthBuildStatus::Completed));
        assert_eq!(stage.stage, TRUTH_SUBSPACE_STAGE);
        assert_eq!(stage.status, StageStatus::Completed);
        assert_eq!((stage.reason, stage.error), (None, None));
        let counts: Vec<(&str, i64)> = stage.counts.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(
            counts,
            [
                ("anchors", 3),
                ("nodes_scored", 5),
                ("nodes_skipped", 2),
                ("truth_epoch", 4)
            ]
        );
    }

    // A fail-open build that comes back errored must not be reported as
    // completed: the stage carries its error and counts.
    #[test]
    fn errored_build_maps_to_an_errored_stage() {
        let mut errored = build(cognee_cognify::TruthBuildStatus::Errored);
        errored.error = Some("centroid upsert failed: down".to_string());
        let stage = truth_subspace_stage_result(&errored);
        assert_eq!(stage.status, StageStatus::Errored);
        assert_eq!(stage.error.as_deref(), Some("centroid upsert failed: down"));
        assert_eq!(stage.counts.get("nodes_scored"), Some(&5));

        // Without an error text, Python's default message.
        let bare = truth_subspace_stage_result(&build(cognee_cognify::TruthBuildStatus::Errored));
        assert_eq!(bare.error.as_deref(), Some("truth subspace build errored"));
    }

    #[test]
    fn skipped_build_maps_to_a_skipped_stage_with_its_reason() {
        let mut skipped = build(cognee_cognify::TruthBuildStatus::Skipped);
        skipped.reason = Some(REASON_BACKEND_UNSUPPORTED.to_string());
        let stage = truth_subspace_stage_result(&skipped);
        assert_eq!(stage.status, StageStatus::Skipped);
        assert_eq!(stage.reason.as_deref(), Some(REASON_BACKEND_UNSUPPORTED));
        assert!(stage.counts.is_empty());
    }

    #[test]
    fn stage_status_names_match_python() {
        let names: Vec<&str> = [
            StageStatus::Completed,
            StageStatus::AlreadyCompleted,
            StageStatus::Skipped,
            StageStatus::Errored,
        ]
        .iter()
        .map(|s| s.as_str())
        .collect();
        assert_eq!(
            names,
            ["completed", "already_completed", "skipped", "errored"]
        );
    }
}
