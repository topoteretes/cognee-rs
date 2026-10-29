//! The improve stage registry and the stages themselves — Python's
//! `cognee/modules/improve/{registry,stage,stages}.py`.
//!
//! [`DEFAULT_STAGES`] lists the stages in execution order; the order is
//! load-bearing and pinned by a test. Each stage is a gate (zero LLM calls), a
//! call into code that lives elsewhere (`cognee_cognify::memify`) and a mapping
//! of that code's return onto a [`StageResult`]. A stage never returns an
//! error: a failure comes back as an `errored` result.

use std::future::Future;
use std::sync::Arc;

use chrono::Utc;
use cognee_cognify::{
    CognifyConfig, apply_feedback_weights_pipeline, distill_sessions_in_knowledge_graph,
    persist_sessions_in_knowledge_graph,
};
use cognee_core::CpuPool;
use cognee_database::{CheckpointStore, PipelineRunRepository, RelationalDb};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_models::dataset_scope::maybe_with_dataset;
use cognee_ontology::OntologyResolver;
use cognee_search::PreferenceConfig;
use cognee_session::{SessionManager, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::{info, warn};
use uuid::Uuid;

use super::result::{REASON_BACKEND_UNSUPPORTED, StageResult, StageStatus};
use super::watermark::{
    REASON_NO_WRITES_SINCE_LAST_IMPROVE, StageStamp, has_dataset_changed_since_last_improve,
};

/// Skip reason of `extract_agent_context` without a usable session manager.
pub const REASON_SESSION_MANAGER_UNAVAILABLE: &str = "session_manager_unavailable";
/// Skip reason of `extract_agent_context` while automatic session context
/// is off (`auto_feedback`, or session caching, off).
pub const REASON_AUTO_FEEDBACK_DISABLED: &str = "auto_feedback_disabled";
/// Skip reason of `update_user_preferences` while personalization is off.
pub const REASON_PERSONALIZATION_DISABLED: &str = "personalization_disabled";
/// Skip reason of an opt-in stage the caller did not opt into.
pub const REASON_OPT_IN_DISABLED: &str = "opt_in_disabled";
/// Skip reason of a stage that drafts text with an LLM when none is
/// configured (keyless installs).
pub const REASON_NO_LLM_CONFIGURED: &str = "no_llm_configured";
/// `already_completed` reason of `persist_agent_traces` with nothing to persist.
pub const REASON_NO_NEW_TRACE_STEPS: &str = "no_new_trace_steps";
/// Skip reason of a stage that exists in Python but is not ported to Rust yet.
/// Rust-only; goes away once the stage is ported.
pub const REASON_NOT_PORTED: &str = "not_ported";

/// Node-set tag attached to the cognified agent-trace feedback.
const AGENT_TRACE_FEEDBACKS_NODE_SET: &str = "agent_trace_feedbacks";

/// One improve stage, in Python's registry (`cognee/modules/improve/registry.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageKind {
    FeedbackWeights,
    PersistSessionQa,
    PersistAgentTraces,
    ExtractAgentContext,
    DistillSessions,
    UpdateUserPreferences,
    BuildTruthSubspace,
    ReviewConflicts,
    GlobalContextIndex,
}

/// The stages `improve()` runs, in execution order.
pub const DEFAULT_STAGES: [StageKind; 9] = [
    StageKind::FeedbackWeights,
    StageKind::PersistSessionQa,
    StageKind::PersistAgentTraces,
    StageKind::ExtractAgentContext,
    StageKind::DistillSessions,
    StageKind::UpdateUserPreferences,
    StageKind::BuildTruthSubspace,
    StageKind::ReviewConflicts,
    StageKind::GlobalContextIndex,
];

/// Stages that existed in earlier releases. A disabled-stage list may still
/// name one; it is ignored with a warning instead of rejected.
pub const REMOVED_STAGE_NAMES: [&str; 1] = ["triplet_enrichment"];

/// Name of the truth-subspace stage, as Python's `BuildTruthSubspaceStage.name`.
pub const TRUTH_SUBSPACE_STAGE: &str = StageKind::BuildTruthSubspace.name();

impl StageKind {
    /// The stage's wire name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::FeedbackWeights => "feedback_weights",
            Self::PersistSessionQa => "persist_session_qa",
            Self::PersistAgentTraces => "persist_agent_traces",
            Self::ExtractAgentContext => "extract_agent_context",
            Self::DistillSessions => "distill_sessions",
            Self::UpdateUserPreferences => "update_user_preferences",
            Self::BuildTruthSubspace => "build_truth_subspace",
            Self::ReviewConflicts => "review_conflicts",
            Self::GlobalContextIndex => "global_context_index",
        }
    }

    /// The stage of that wire name, if any.
    pub fn from_name(name: &str) -> Option<Self> {
        DEFAULT_STAGES
            .into_iter()
            .find(|stage| stage.name() == name)
    }

    /// Session-fed stages are skipped with `no_session_ids` when the run was
    /// given none; the rest work on the graph alone.
    pub const fn needs_sessions(self) -> bool {
        !matches!(self, Self::ReviewConflicts | Self::GlobalContextIndex)
    }

    /// A fatal stage that errors stops the run. Only `persist_session_qa`:
    /// losing session Q&A would be data loss.
    pub const fn fatal(self) -> bool {
        matches!(self, Self::PersistSessionQa)
    }
}

/// What the run's graph backend can do, probed once per run — Python's
/// `GraphCapabilities` (`cognee/modules/improve/capabilities.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GraphCapabilities {
    pub supports_feedback_weights: bool,
    pub supports_truth_state: bool,
}

impl GraphCapabilities {
    /// Probe `graph_db` in the dataset's scope. Fails open: a probe that
    /// cannot reach the engine assumes support and lets the stages report
    /// their own errors, as Python's `resolve_graph_capabilities` does.
    pub async fn probe(graph_db: &dyn GraphDBTrait, dataset_id: Option<Uuid>) -> Self {
        let probe = async {
            Ok::<_, cognee_graph::GraphDBError>(Self {
                supports_feedback_weights: graph_db.supports_feedback_weights().await?,
                supports_truth_state: graph_db.supports_truth_state().await?,
            })
        };
        match maybe_with_dataset(dataset_id, probe).await {
            Ok(capabilities) => capabilities,
            Err(e) => {
                warn!("improve: graph capability probe failed, assuming support: {e}");
                Self {
                    supports_feedback_weights: true,
                    supports_truth_state: true,
                }
            }
        }
    }
}

/// One stage's outcome as the orchestrator sees it.
#[derive(Debug)]
pub(crate) struct StageRun {
    pub result: StageResult,
    /// The stage errored because the LLM budget is exhausted (Python's
    /// `StageResult.budget_exhausted`).
    pub budget_exhausted: bool,
    /// The watermark to save once the run is known not to have errored
    /// (Python's `StageResult.run_info_stamp`).
    pub stamp: Option<StageStamp>,
}

impl From<StageResult> for StageRun {
    fn from(result: StageResult) -> Self {
        Self {
            result,
            budget_exhausted: false,
            stamp: None,
        }
    }
}

impl StageRun {
    /// A run that errored on an exhausted LLM budget when `budget_exhausted`.
    pub fn with_budget(result: StageResult, budget_exhausted: bool) -> Self {
        Self {
            result,
            budget_exhausted,
            stamp: None,
        }
    }
}

/// A stage-internal failure: its rendered message, and whether it was an
/// exhausted LLM budget (kept typed until the message is all that is left).
#[derive(Debug)]
struct StageFailure {
    message: String,
    budget_exhausted: bool,
}

impl From<String> for StageFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            budget_exhausted: false,
        }
    }
}

impl From<&str> for StageFailure {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

/// The two halves of a stage the orchestrator drives: its own gate and its
/// run. Split out so the loop can be tested with scripted stages.
pub(crate) trait StageExecutor {
    /// The stage's own skip reason, or `None` to run. Makes no LLM call.
    fn gate(&self, stage: StageKind) -> Option<&'static str>;

    /// Run the stage. Never fails: an error is an `errored` result.
    fn run(&mut self, stage: StageKind) -> impl Future<Output = StageRun> + Send;

    /// Record that this run wrote to the dataset's graph, once, right after
    /// the first stage that may have (see [`super::watermark`]). Best effort.
    fn record_graph_write(&mut self) -> impl Future<Output = ()> + Send {
        std::future::ready(())
    }
}

/// The stages of one real `improve()` run, over the run's inputs.
pub(crate) struct ImproveStages<'a> {
    pub dataset_name: String,
    pub owner_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub session_ids: Vec<String>,
    pub feedback_alpha: f64,
    pub build_truth_subspace: bool,
    pub build_global_context_index: bool,
    /// Opt in to `review_conflicts` (`improve.review_conflicts`).
    pub review_conflicts: bool,
    /// The personalization settings (`personalization_enabled`,
    /// `preference_alpha`, `preference_beta`).
    pub personalization: PreferenceConfig,
    /// Automatic session context is on (Python's `is_auto_feedback_enabled`).
    pub auto_feedback: bool,
    pub capabilities: GraphCapabilities,
    /// The resolved dataset, once it exists; a session-persisting stage may
    /// create it mid-run, so a `None` is looked up again on demand.
    pub dataset_id: Option<Uuid>,
    pub llm: Arc<dyn Llm>,
    pub storage: Arc<dyn StorageTrait>,
    pub graph_db: Arc<dyn GraphDBTrait>,
    pub vector_db: Arc<dyn VectorDB>,
    pub embedding_engine: Arc<dyn EmbeddingEngine>,
    pub ontology_resolver: Arc<dyn OntologyResolver>,
    pub db: Option<Arc<dyn RelationalDb>>,
    pub session_store: Option<Arc<dyn SessionStore>>,
    pub session_manager: Option<Arc<SessionManager>>,
    pub add_pipeline: Option<&'a AddPipeline>,
    /// Where the stage watermarks live.
    pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    pub cognify_config: &'a CognifyConfig,
}

impl StageExecutor for ImproveStages<'_> {
    fn gate(&self, stage: StageKind) -> Option<&'static str> {
        match stage {
            StageKind::FeedbackWeights if !self.capabilities.supports_feedback_weights => {
                Some(REASON_BACKEND_UNSUPPORTED)
            }
            StageKind::ExtractAgentContext
                if !self
                    .session_manager
                    .as_ref()
                    .is_some_and(|m| m.is_available()) =>
            {
                Some(REASON_SESSION_MANAGER_UNAVAILABLE)
            }
            StageKind::ExtractAgentContext if !self.auto_feedback => {
                Some(REASON_AUTO_FEEDBACK_DISABLED)
            }
            StageKind::ExtractAgentContext | StageKind::DistillSessions
                if !self.llm.is_configured() =>
            {
                Some(REASON_NO_LLM_CONFIGURED)
            }
            StageKind::UpdateUserPreferences if !self.personalization.enabled => {
                Some(REASON_PERSONALIZATION_DISABLED)
            }
            StageKind::BuildTruthSubspace if !self.build_truth_subspace => {
                Some(REASON_OPT_IN_DISABLED)
            }
            StageKind::BuildTruthSubspace if !self.capabilities.supports_truth_state => {
                Some(REASON_BACKEND_UNSUPPORTED)
            }
            // Conflict review is opt-in and not ported yet (SDK-1038).
            StageKind::ReviewConflicts => Some(if !self.review_conflicts {
                REASON_OPT_IN_DISABLED
            } else if !self.llm.is_configured() {
                REASON_NO_LLM_CONFIGURED
            } else {
                REASON_NOT_PORTED
            }),
            StageKind::GlobalContextIndex if !self.build_global_context_index => {
                Some(REASON_OPT_IN_DISABLED)
            }
            _ => None,
        }
    }

    async fn record_graph_write(&mut self) {
        let Some(db) = self.db.clone() else {
            return;
        };
        let dataset_id = match self.dataset_id().await {
            Ok(id) => id,
            Err(e) => {
                warn!("improve: could not resolve the dataset to record its graph write: {e}");
                return;
            }
        };
        super::watermark::record_graph_write(&*db.pipeline_runs(), dataset_id).await;
    }

    async fn run(&mut self, stage: StageKind) -> StageRun {
        match stage {
            StageKind::FeedbackWeights => self.feedback_weights().await.into(),
            StageKind::PersistSessionQa => self.persist_session_qa().await,
            StageKind::PersistAgentTraces => self.persist_agent_traces().await,
            StageKind::ExtractAgentContext => self.extract_agent_context().await,
            StageKind::DistillSessions => self.distill_sessions().await,
            StageKind::UpdateUserPreferences => self.update_user_preferences().await.into(),
            StageKind::BuildTruthSubspace => self.build_truth_subspace().await.into(),
            StageKind::GlobalContextIndex => self.global_context_index().await,
            // Its gate always skips it (see `gate`).
            StageKind::ReviewConflicts => {
                StageResult::skipped(stage.name(), REASON_NOT_PORTED).into()
            }
        }
    }
}

/// The CPU pool the cognify-backed stages run on.
fn thread_pool() -> Result<Arc<dyn CpuPool>, String> {
    cognee_core::RayonThreadPool::with_default_threads()
        .map(|pool| Arc::new(pool) as Arc<dyn CpuPool>)
        .map_err(|e| format!("failed to construct thread pool: {e}"))
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl ImproveStages<'_> {
    fn sessions(&self) -> i64 {
        count(self.session_ids.len())
    }

    /// The run's dataset id, looking it up again while it does not exist.
    async fn dataset_id(&mut self) -> Result<Option<Uuid>, String> {
        if self.dataset_id.is_none()
            && let Some(db) = self.db.as_ref()
        {
            self.dataset_id = cognee_database::IngestDb::get_dataset_by_name_in_tenant(
                &**db,
                &self.dataset_name,
                self.owner_id,
                self.tenant_id,
            )
            .await
            .map_err(|e| format!("dataset lookup failed for '{}': {e}", self.dataset_name))?
            .map(|dataset| dataset.id);
        }
        Ok(self.dataset_id)
    }

    /// Stage 1: move `feedback_weight` on the graph elements scored answers used.
    async fn feedback_weights(&mut self) -> StageResult {
        const STAGE: &str = StageKind::FeedbackWeights.name();
        let (Some(store), Some(manager)) = (&self.session_store, &self.session_manager) else {
            return StageResult::errored(STAGE, "session store and session manager are required");
        };
        let applied = maybe_with_dataset(
            self.dataset_id,
            apply_feedback_weights_pipeline(
                &self.session_ids,
                self.owner_id,
                self.feedback_alpha,
                &*self.graph_db,
                Arc::clone(store),
                Arc::clone(manager),
            ),
        )
        .await;
        match applied {
            Ok(r) => {
                info!(
                    processed = r.processed,
                    applied = r.applied,
                    skipped = r.skipped,
                    "improve: feedback weights applied"
                );
                StageResult::completed(STAGE).with_counts([
                    ("sessions", self.sessions()),
                    ("entries_processed", count(r.processed)),
                    ("entries_applied", count(r.applied)),
                ])
            }
            Err(e) => StageResult::errored(STAGE, e.to_string()),
        }
    }

    /// Stage 2: cognify session Q&A into the graph. The single fatal stage.
    async fn persist_session_qa(&mut self) -> StageRun {
        const STAGE: &str = StageKind::PersistSessionQa.name();
        let (Some(store), Some(pipeline), Some(database)) =
            (&self.session_store, self.add_pipeline, &self.db)
        else {
            return StageResult::errored(
                STAGE,
                "session store, add pipeline and relational database are required",
            )
            .into();
        };
        let thread_pool = match thread_pool() {
            Ok(pool) => pool,
            Err(e) => return StageResult::errored(STAGE, e).into(),
        };
        let persisted = persist_sessions_in_knowledge_graph(
            &self.session_ids,
            &self.dataset_name,
            self.owner_id,
            self.tenant_id,
            Arc::clone(store),
            pipeline,
            Arc::clone(&self.llm),
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
            Arc::clone(&self.embedding_engine),
            Arc::clone(database),
            Arc::clone(database).pipeline_runs(),
            thread_pool,
            Arc::clone(&self.ontology_resolver),
            self.cognify_config,
        )
        .await;
        match persisted {
            Ok(r) => {
                info!(
                    persisted = r.sessions_persisted,
                    skipped = r.sessions_skipped,
                    failed = r.sessions_failed,
                    "improve: session Q&A persisted"
                );
                let counts = [
                    ("sessions", self.sessions()),
                    ("sessions_persisted", count(r.sessions_persisted)),
                ];
                match r.last_error {
                    Some(error) if r.sessions_failed > 0 => StageRun::with_budget(
                        StageResult::errored(STAGE, error).with_counts(counts),
                        r.budget_exhausted,
                    ),
                    _ => StageResult::completed(STAGE).with_counts(counts).into(),
                }
            }
            Err(e) => StageRun::with_budget(
                StageResult::errored(STAGE, e.to_string()),
                e.is_budget_exhausted(),
            ),
        }
    }

    /// Stage 3: cognify the agent trace steps' feedback into the graph.
    ///
    /// Scoped-down port: every session's trace `session_feedback` text is
    /// cognified as one document under the `agent_trace_feedbacks` node-set.
    /// There is no per-session trace watermark yet (SDK-1047), so `windows`
    /// counts the sessions that contributed text.
    async fn persist_agent_traces(&mut self) -> StageRun {
        const STAGE: &str = StageKind::PersistAgentTraces.name();
        let Some(manager) = self.session_manager.as_ref() else {
            return StageResult::errored(STAGE, "session manager is required").into();
        };
        let user_id = self.owner_id.to_string();
        let mut trace_texts: Vec<String> = Vec::new();
        let mut windows = 0usize;
        for sid in &self.session_ids {
            let steps = match manager
                .get_agent_trace_session(&user_id, Some(sid), None)
                .await
            {
                Ok(steps) => steps,
                Err(e) => {
                    return StageResult::errored(
                        STAGE,
                        format!("could not read the trace steps of session {sid}: {e}"),
                    )
                    .with_counts([("sessions", self.sessions()), ("windows", 0)])
                    .into();
                }
            };
            let before = trace_texts.len();
            trace_texts.extend(
                steps
                    .iter()
                    .filter(|step| !step.session_feedback.is_empty())
                    .map(|step| {
                        format!(
                            "Session: {sid}\nFunction: {}\nStatus: {}\nFeedback: {}",
                            step.origin_function, step.status, step.session_feedback,
                        )
                    }),
            );
            if trace_texts.len() > before {
                windows += 1;
            }
        }
        if trace_texts.is_empty() {
            return StageResult::already_completed(STAGE, REASON_NO_NEW_TRACE_STEPS).into();
        }

        let counts = [("sessions", self.sessions()), ("windows", count(windows))];
        match self.cognify_trace_texts(trace_texts.join("\n\n")).await {
            Ok(()) => {
                info!(
                    trace_items = trace_texts.len(),
                    windows, "improve: agent trace feedback persisted"
                );
                StageResult::completed(STAGE).with_counts(counts).into()
            }
            Err(e) => StageRun {
                result: StageResult::errored(STAGE, e.message).with_counts(counts),
                budget_exhausted: e.budget_exhausted,
                stamp: None,
            },
        }
    }

    async fn cognify_trace_texts(&mut self, text: String) -> Result<(), StageFailure> {
        let (Some(pipeline), Some(database)) = (self.add_pipeline, self.db.clone()) else {
            return Err("add pipeline and relational database are required".into());
        };
        let add_params = AddParams {
            node_set: Some(vec![AGENT_TRACE_FEEDBACKS_NODE_SET.to_string()]),
            ..Default::default()
        };
        let data_rows = pipeline
            .add_with_params(
                vec![DataInput::Text(text)],
                &self.dataset_name,
                self.owner_id,
                self.tenant_id,
                &add_params,
            )
            .await
            .map_err(|e| StageFailure::from(format!("add of the trace feedback failed: {e}")))?;
        if data_rows.is_empty() {
            return Err("add of the trace feedback returned no rows".into());
        }
        let dataset_id = self.dataset_id().await?.ok_or_else(|| {
            StageFailure::from(format!(
                "dataset '{}' not found after add",
                self.dataset_name
            ))
        })?;
        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::clone(&database).pipeline_runs();
        let thread_pool = thread_pool()?;
        cognee_cognify::tasks::cognify(
            data_rows,
            dataset_id,
            Some(self.owner_id),
            None,
            self.tenant_id,
            Arc::clone(&self.llm),
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
            Arc::clone(&self.embedding_engine),
            database,
            pipeline_run_repo,
            thread_pool,
            Arc::clone(&self.ontology_resolver),
            self.cognify_config,
        )
        .await
        .map(|_| ())
        .map_err(|e| StageFailure {
            budget_exhausted: e.is_budget_exhausted(),
            message: format!("cognify of the trace feedback failed: {e}"),
        })
    }

    /// Stage 4: flush each session's pending trace steps into agent-profile
    /// lessons (Python's `ExtractAgentContextStage`), with the watermark the
    /// mid-session extraction shares and `min_new_traces = 1`.
    ///
    /// Fail-open per session: one failing session never blocks the others,
    /// and the stage is `errored` when any failed (an exhausted LLM budget
    /// when the last failure was a 402).
    async fn extract_agent_context(&mut self) -> StageRun {
        const STAGE: &str = StageKind::ExtractAgentContext.name();
        let Some(manager) = self.session_manager.as_ref() else {
            return StageResult::errored(STAGE, "session manager is required").into();
        };
        let user_id = self.owner_id.to_string();
        let mut lessons = 0usize;
        let mut failed = 0usize;
        let mut last_error: Option<cognee_session::SessionError> = None;
        for session_id in &self.session_ids {
            match cognee_session::extract_pending_agent_context(
                manager,
                self.llm.as_ref(),
                session_id,
                &user_id,
                1,
                cognee_session::TRACE_EXTRACTION_OVERLAP,
                cognee_session::BATCH_TRACE_LIMIT,
            )
            .await
            {
                Ok(touched) => lessons += touched.len(),
                Err(error) => {
                    warn!(
                        session_id,
                        %error,
                        "improve: agent-context extraction failed (non-fatal)"
                    );
                    failed += 1;
                    last_error = Some(error);
                }
            }
        }
        info!(
            lessons,
            sessions_failed = failed,
            "improve: agent context extracted"
        );
        let counts = [
            ("lessons", count(lessons)),
            ("sessions_failed", count(failed)),
        ];
        match last_error {
            Some(error) => StageRun::with_budget(
                StageResult::errored(STAGE, error.to_string()).with_counts(counts),
                error.is_budget_exhausted(),
            ),
            None => StageResult::completed(STAGE).with_counts(counts).into(),
        }
    }

    /// Stage 6: fold rated turns and stated preferences into the user's
    /// `prefers` subgraph (Python's `UpdateUserPreferencesStage`), in the
    /// dataset's storage scope. `completed` carries the reason `no_changes`
    /// when there was nothing to fold.
    async fn update_user_preferences(&mut self) -> StageResult {
        const STAGE: &str = StageKind::UpdateUserPreferences.name();
        let Some(manager) = self.session_manager.clone() else {
            return StageResult::errored(STAGE, "session manager is required");
        };
        let dataset_id = match self.dataset_id().await {
            Ok(Some(id)) => id,
            // Python's `resolve_preference_scope` raises: the edges must land
            // in the graph they refer to.
            Ok(None) => {
                return StageResult::errored(
                    STAGE,
                    format!(
                        "Dataset '{}' not found or not writable for this user.",
                        self.dataset_name
                    ),
                );
            }
            Err(e) => return StageResult::errored(STAGE, e),
        };
        let (user_id, dataset_key) = (self.owner_id.to_string(), dataset_id.to_string());
        let update = super::user_preferences::update_user_preferences(
            self.graph_db.as_ref(),
            &manager,
            &self.personalization,
            &user_id,
            &dataset_key,
            &self.session_ids,
        );
        match maybe_with_dataset(Some(dataset_id), update).await {
            Ok(r) => {
                info!(
                    status = r.status,
                    turns = r.turns_applied,
                    edges = r.edges_written,
                    pruned = r.edges_pruned,
                    text_lines = r.text_lines_added,
                    "improve: user preferences updated"
                );
                let mut result = StageResult::completed(STAGE).with_counts([
                    ("turns_applied", count(r.turns_applied)),
                    ("edges_written", count(r.edges_written)),
                    ("edges_pruned", count(r.edges_pruned)),
                    ("text_lines_added", count(r.text_lines_added)),
                ]);
                if r.status != super::user_preferences::STATUS_COMPLETED {
                    result.reason = Some(r.status.to_string());
                }
                result
            }
            Err(e) => StageResult::errored(STAGE, e),
        }
    }

    /// Stage 5: distill each session's Q&A into lessons (`session_learnings`).
    ///
    /// Scoped-down port: only Q&A is consumed (no session-context entry
    /// corpus yet), so Python's `entries_uncurated` count has no counterpart.
    async fn distill_sessions(&mut self) -> StageRun {
        const STAGE: &str = StageKind::DistillSessions.name();
        let (Some(store), Some(pipeline), Some(database)) =
            (&self.session_store, self.add_pipeline, &self.db)
        else {
            return StageResult::errored(
                STAGE,
                "session store, add pipeline and relational database are required",
            )
            .into();
        };
        let thread_pool = match thread_pool() {
            Ok(pool) => pool,
            Err(e) => return StageResult::errored(STAGE, e).into(),
        };
        let r = distill_sessions_in_knowledge_graph(
            &self.session_ids,
            &self.dataset_name,
            self.owner_id,
            self.tenant_id,
            Arc::clone(store),
            pipeline,
            Arc::clone(&self.llm),
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
            Arc::clone(&self.embedding_engine),
            Arc::clone(database),
            Arc::clone(database).pipeline_runs(),
            thread_pool,
            Arc::clone(&self.ontology_resolver),
            self.cognify_config,
        )
        .await;
        info!(
            sessions_distilled = r.sessions_distilled,
            lessons_published = r.lessons_published,
            sessions_failed = r.sessions_failed,
            "improve: sessions distilled"
        );
        let counts = [
            ("documents", count(r.lessons_published)),
            ("sessions_completed", count(r.sessions_distilled)),
            ("sessions_failed", count(r.sessions_failed)),
        ];
        match r.last_error {
            Some(error) if r.sessions_failed > 0 => StageRun::with_budget(
                StageResult::errored(STAGE, error).with_counts(counts),
                r.budget_exhausted,
            ),
            _ => StageResult::completed(STAGE).with_counts(counts).into(),
        }
    }

    /// Stage 7: build the truth subspace from distilled learnings (opt-in).
    async fn build_truth_subspace(&mut self) -> StageResult {
        let dataset_id = match self.dataset_id().await {
            Ok(Some(id)) => id,
            // Python's build returns its empty result for a dataset it
            // cannot resolve, which the stage reports as completed.
            Ok(None) => {
                warn!("improve: dataset not found; no truth subspace to build");
                return truth_subspace_stage_result(&cognee_cognify::TruthSubspaceResult::default());
            }
            Err(e) => return StageResult::errored(TRUTH_SUBSPACE_STAGE, e),
        };
        let ts = cognee_cognify::build_truth_subspace(
            dataset_id,
            &self.session_ids,
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
            Arc::clone(&self.embedding_engine),
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
            "improve: truth subspace built"
        );
        truth_subspace_stage_result(&ts)
    }

    /// Stage 9: the global context index (opt-in).
    ///
    /// `already_completed` (`no_writes_since_last_improve`) when only reads
    /// ran since its last build, which a completed run stamps (see
    /// [`super::watermark`]).
    async fn global_context_index(&mut self) -> StageRun {
        const KIND: StageKind = StageKind::GlobalContextIndex;
        // Captured before the change check and the build, so a write racing
        // this run stays visible to the next run's check.
        let started_at = Utc::now();
        let dataset_id = match self.dataset_id().await {
            Ok(id) => id,
            Err(e) => return StageResult::errored(KIND.name(), e).into(),
        };
        let unchanged = match dataset_id {
            Some(id) => {
                let runs = self.db.clone().map(|db| db.pipeline_runs());
                // Rust has no notion of whether datasets share one graph here,
                // so every dataset's runs count (Python narrows that to the
                // dataset under backend access control).
                !has_dataset_changed_since_last_improve(
                    self.checkpoint_store.as_deref(),
                    runs.as_deref(),
                    KIND,
                    id,
                    true,
                )
                .await
            }
            None => false,
        };
        let (result, budget_exhausted) = if unchanged {
            (
                StageResult::already_completed(KIND.name(), REASON_NO_WRITES_SINCE_LAST_IMPROVE),
                false,
            )
        } else {
            self.build_global_context_index(dataset_id).await
        };
        let stamp = match (dataset_id, result.status) {
            (Some(id), StageStatus::Completed | StageStatus::AlreadyCompleted) => {
                Some(StageStamp::new(KIND, id, started_at))
            }
            _ => None,
        };
        StageRun {
            result,
            budget_exhausted,
            stamp,
        }
    }

    /// The global context index build: Python's default method,
    /// `from_entities` (no LLM), in the dataset's storage scope.
    ///
    /// Counts `nodes_written`, the index nodes whose text changed (Rust
    /// extra). A dataset that does not exist yet has nothing to index.
    ///
    /// Also says whether it failed on an exhausted LLM budget (the LLM
    /// methods).
    async fn build_global_context_index(
        &mut self,
        dataset_id: Option<Uuid>,
    ) -> (StageResult, bool) {
        const STAGE: &str = StageKind::GlobalContextIndex.name();
        let Some(dataset_id) = dataset_id else {
            info!("improve: dataset not found; no global context index to build");
            return (StageResult::completed(STAGE), false);
        };
        let Some(db) = self.db.as_ref() else {
            return (
                StageResult::errored(STAGE, "relational database is required"),
                false,
            );
        };
        let store = cognee_cognify::IndexStore {
            graph_db: self.graph_db.as_ref(),
            vector_db: self.vector_db.as_ref(),
            embedding_engine: self.embedding_engine.as_ref(),
            db: db.as_ref(),
            dataset_id,
            owner_id: self.owner_id,
            tenant_id: self.tenant_id,
        };
        let method = cognee_cognify::GlobalContextIndexMethod::default();
        let build = cognee_cognify::build_global_context_index(&store, &self.llm, &method);
        match maybe_with_dataset(Some(dataset_id), build).await {
            Ok(written) => {
                info!(
                    nodes = written.len(),
                    "improve: global context index updated"
                );
                (
                    StageResult::completed(STAGE)
                        .with_counts([("nodes_written", count(written.len()))]),
                    false,
                )
            }
            Err(e) => (
                StageResult::errored(STAGE, format!("global context index failed: {e}")),
                e.is_budget_exhausted(),
            ),
        }
    }
}

/// Map a truth-subspace build onto its stage result, as Python's
/// `BuildTruthSubspaceStage.run` does (`cognee/modules/improve/stages.py:491-525`).
///
/// The build fails open — a failed centroid commit or chunk write comes back
/// as `Errored` instead of an error — so the status is translated rather than
/// reported as completed, which would hide the failure. Counts are the
/// build's integer fields.
pub fn truth_subspace_stage_result(build: &cognee_cognify::TruthSubspaceResult) -> StageResult {
    use cognee_cognify::TruthBuildStatus;

    let counts = [
        ("anchors", count(build.anchors)),
        ("nodes_scored", count(build.nodes_scored)),
        ("nodes_skipped", count(build.nodes_skipped)),
        ("truth_epoch", build.truth_epoch),
    ];
    match build.status {
        TruthBuildStatus::Errored => StageResult::errored(
            TRUTH_SUBSPACE_STAGE,
            build
                .error
                .clone()
                .unwrap_or_else(|| "truth subspace build errored".to_string()),
        )
        .with_counts(counts),
        // Normally unreachable: the stage gate checks the same capability
        // first. Reachable by a mid-run capability change; the mapping should
        // not lie about it.
        TruthBuildStatus::Skipped => StageResult::skipped(
            TRUTH_SUBSPACE_STAGE,
            build.reason.as_deref().unwrap_or("skipped"),
        ),
        TruthBuildStatus::Completed => {
            StageResult::completed(TRUTH_SUBSPACE_STAGE).with_counts(counts)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::result::StageStatus;
    use super::*;

    // Python `test_registry_order`.
    #[test]
    fn registry_order_matches_python() {
        let names: Vec<&str> = DEFAULT_STAGES.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
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
        for stage in DEFAULT_STAGES {
            assert_eq!(StageKind::from_name(stage.name()), Some(stage));
        }
        assert_eq!(StageKind::from_name("triplet_enrichment"), None);
        assert_eq!(REMOVED_STAGE_NAMES, ["triplet_enrichment"]);
    }

    // Python `validate_fatal_stage_policy`: exactly persist_session_qa.
    #[test]
    fn only_persist_session_qa_is_fatal() {
        let fatal: Vec<&str> = DEFAULT_STAGES
            .iter()
            .filter(|s| s.fatal())
            .map(|s| s.name())
            .collect();
        assert_eq!(fatal, ["persist_session_qa"]);
    }

    #[test]
    fn only_the_graph_stages_run_without_sessions() {
        let graph_only: Vec<&str> = DEFAULT_STAGES
            .iter()
            .filter(|s| !s.needs_sessions())
            .map(|s| s.name())
            .collect();
        assert_eq!(graph_only, ["review_conflicts", "global_context_index"]);
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
}
