//! `improve()`: the staged orchestrator of the self-improvement loop —
//! Python's `cognee.api.v1.improve.improve()`.
//!
//! A run is one gated stage after another, in registry order
//! ([`DEFAULT_STAGES`]): `feedback_weights`, `persist_session_qa`,
//! `persist_agent_traces`, `extract_agent_context`, `distill_sessions`,
//! `update_user_preferences`, `build_truth_subspace`, `review_conflicts`,
//! `global_context_index`. Every stage reports a [`StageResult`] on the
//! returned [`ImproveResult`].
//!
//! Gates apply in order: `disabled_by_config`, then `no_session_ids` for a
//! session-fed stage, then the stage's own gate. The run fails open — a
//! stage's failure is recorded and the next stage still runs — except for:
//!
//! - the one fatal stage, `persist_session_qa` (losing session Q&A would be
//!   data loss): it stops the run, the remaining stages are skipped with
//!   `aborted_by_fatal_stage` (or `budget_exhausted`), and `improve()` returns
//!   [`ApiError::ImproveAborted`] carrying the partial result;
//! - an exhausted LLM budget in a non-fatal stage: every later stage would
//!   fail the same way, so they are skipped with `budget_exhausted` and the
//!   run ends there, without an error.

mod result;
mod stages;

use std::sync::Arc;
use std::time::Instant;

use cognee_cognify::CognifyConfig;
use cognee_database::{CheckpointStore, RelationalDb};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_ontology::OntologyResolver;
use cognee_session::{ImproveLockGuard, SessionManager, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::{Instrument, info, warn};
use uuid::Uuid;

use super::error::ApiError;
use stages::{GraphCapabilities, ImproveStages, StageExecutor, StageRun};

pub use result::{
    ImproveResult, ImproveStatus, REASON_ABORTED_BY_FATAL_STAGE, REASON_BACKEND_UNSUPPORTED,
    REASON_BUDGET_EXHAUSTED, REASON_DISABLED_BY_CONFIG, REASON_NO_SESSION_IDS, StageResult,
    StageStatus,
};
pub use stages::{
    DEFAULT_STAGES, REASON_NO_LLM_CONFIGURED, REASON_NO_NEW_TRACE_STEPS, REASON_NOT_PORTED,
    REASON_OPT_IN_DISABLED, REASON_PERSONALIZATION_DISABLED, REASON_SESSION_MANAGER_UNAVAILABLE,
    REMOVED_STAGE_NAMES, StageKind, TRUTH_SUBSPACE_STAGE, truth_subspace_stage_result,
};

/// Parameters for [`improve`].
///
/// All fields are required at construction time — `Default` is intentionally
/// not derived because several fields (`owner_id`, the engine handles, and
/// `cognify_config`) have no sensible default value. Callers that omit
/// optional behavior pass `None` explicitly for the `Option<...>` fields.
pub struct ImproveParams<'a> {
    /// Dataset name to operate on.
    pub dataset_name: String,
    /// The dataset's id when the caller already resolved it (an HTTP request
    /// naming a dataset by id, possibly one shared with the caller). `None`
    /// resolves `dataset_name` among the owner's own datasets.
    pub dataset_id: Option<Uuid>,
    /// Session ids that feed the session stages. `None` or empty skips them
    /// with `no_session_ids`; empty ids are dropped.
    pub session_ids: Option<Vec<String>>,
    /// Deprecated in Python (SDK-1115): it only scoped the removed triplet
    /// stage and the deprecated custom tasks. Ignored.
    pub node_name: Option<Vec<String>>,
    /// Owner UUID under which graph/session reads and writes are scoped.
    pub owner_id: Uuid,
    /// Optional tenant UUID for multi-tenant deployments.
    pub tenant_id: Option<Uuid>,
    /// Mixing factor of the `feedback_weights` stage.
    pub feedback_alpha: f64,

    /// Deprecated in Python (SDK-1115): custom extraction tasks, which Python
    /// runs as a trailing `custom_tasks` stage. Not supported; ignored.
    pub extraction_tasks: Option<Vec<String>>,
    /// Deprecated in Python (SDK-1115), as `extraction_tasks`. Ignored.
    pub enrichment_tasks: Option<Vec<String>>,
    /// Deprecated in Python (SDK-1115), as `extraction_tasks`. Ignored.
    pub data: Option<String>,

    /// Opt in to the `global_context_index` stage (Python's
    /// `build_global_context_index`). Default `false`.
    pub build_global_context_index: bool,

    /// Opt in to the `build_truth_subspace` stage (Python's
    /// `build_truth_subspace`). Default `false`.
    pub build_truth_subspace: bool,

    /// The host dispatched this run in the background; reported on
    /// [`ImproveResult::background`]. Background dispatch itself is the
    /// host's (HTTP server, CLI): this function always runs to completion.
    pub run_in_background: bool,

    /// LLM handle (session persistence and distillation).
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

    /// Metadata DB connection: dataset lookup and the cognify-backed stages.
    pub db: Option<Arc<dyn RelationalDb>>,
    /// Session backing store. Required by the session stages.
    pub session_store: Option<Arc<dyn SessionStore>>,
    /// Session manager. Required by the session stages.
    pub session_manager: Option<Arc<SessionManager>>,
    /// Add pipeline (borrowed). Required by the cognify-backed stages.
    pub add_pipeline: Option<&'a AddPipeline>,
    /// Checkpoint store. Unused since the graph-to-session sync stage was
    /// removed; reserved for the stage watermarks (SDK-1044 step 4).
    pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,

    /// Borrowed cognify configuration of the cognify-backed stages.
    pub cognify_config: &'a CognifyConfig,
}

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

/// How the stage loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopEnd {
    /// Every stage was recorded (some possibly `budget_exhausted`-skipped).
    Finished,
    /// The fatal stage errored; the rest were skipped.
    Aborted { budget_exhausted: bool },
}

/// The run-level gates every stage shares, then the stage's own — Python's
/// `evaluate_gate` (`cognee/modules/improve/stage.py`).
fn gate_reason<E: StageExecutor>(
    stage: StageKind,
    has_sessions: bool,
    stages_disabled: &[String],
    executor: &E,
) -> Option<&'static str> {
    if stages_disabled.iter().any(|name| name == stage.name()) {
        return Some(REASON_DISABLED_BY_CONFIG);
    }
    if stage.needs_sessions() && !has_sessions {
        return Some(REASON_NO_SESSION_IDS);
    }
    executor.gate(stage)
}

/// Record `stages` as skipped with `reason`, in order.
fn skip_stages(result: &mut ImproveResult, stages: &[StageKind], reason: &str) {
    result.stages.extend(
        stages
            .iter()
            .map(|s| StageResult::skipped(s.name(), reason)),
    );
}

/// One [`StageResult`] per stage, in order — Python's `execute_stages`
/// (`cognee/api/v1/improve/improve.py:242-280`).
///
/// A gated stage is skipped with its reason and no duration; a stage that
/// runs is timed. A fatal stage that errors skips the rest (with
/// `aborted_by_fatal_stage`, or `budget_exhausted` when that is why it
/// failed), sets [`ImproveResult::error`] and ends the loop as
/// [`LoopEnd::Aborted`]. A non-fatal stage that errored on an exhausted LLM
/// budget skips the rest with `budget_exhausted` and ends it as
/// [`LoopEnd::Finished`].
async fn execute_stages<E: StageExecutor>(
    stages: &[StageKind],
    has_sessions: bool,
    stages_disabled: &[String],
    executor: &mut E,
    result: &mut ImproveResult,
) -> LoopEnd {
    for (index, &stage) in stages.iter().enumerate() {
        let run = match gate_reason(stage, has_sessions, stages_disabled, executor) {
            Some(reason) => StageRun::from(StageResult::skipped(stage.name(), reason)),
            None => {
                let started_at = Instant::now();
                let mut run = executor.run(stage).await;
                run.result.duration_ms =
                    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                run
            }
        };
        let errored = run.result.status == StageStatus::Errored;
        // Python's `StageResult.budget_exhausted`: the stage's typed signal,
        // else its recorded error text (`is_budget_exhausted_record`), so a
        // stage that could only render the provider's failure still stops
        // the run.
        let budget_exhausted = errored
            && (run.budget_exhausted
                || run
                    .result
                    .error
                    .as_deref()
                    .is_some_and(cognee_llm::is_budget_exhausted_message));
        if errored {
            warn!(
                stage = stage.name(),
                error = run.result.error.as_deref().unwrap_or(""),
                "improve: stage errored"
            );
        }
        let error = run.result.error.clone();
        result.stages.push(run.result);
        let remaining = &stages[index + 1..];

        if stage.fatal() && errored {
            let reason = if budget_exhausted {
                REASON_BUDGET_EXHAUSTED
            } else {
                REASON_ABORTED_BY_FATAL_STAGE
            };
            skip_stages(result, remaining, reason);
            result.error = Some(error.unwrap_or_else(|| format!("{} errored", stage.name())));
            warn!(
                stage = stage.name(),
                "improve: fatal stage failed, run stopped"
            );
            return LoopEnd::Aborted { budget_exhausted };
        }
        if budget_exhausted {
            skip_stages(result, remaining, REASON_BUDGET_EXHAUSTED);
            warn!(
                stage = stage.name(),
                remaining = remaining.len(),
                "improve: LLM budget exhausted; run stopped, remaining stages skipped"
            );
            break;
        }
    }
    LoopEnd::Finished
}

/// Run the improve stages over a dataset and report what each one did.
///
/// Returns [`ApiError::ImproveAborted`] — carrying the partial result — when
/// the fatal `persist_session_qa` stage errors, and an error when the dataset
/// lookup itself fails. Every other stage failure is reported on the result.
pub async fn improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError> {
    let session_ids: Vec<String> = params
        .session_ids
        .iter()
        .flatten()
        .filter(|id| !id.is_empty())
        .cloned()
        .collect();
    // Mirrors Python's `cognee.api.improve` span (gap 03 / task 03-07).
    let span = tracing::info_span!(
        "cognee.api.improve",
        dataset = %params.dataset_name,
        session_count = session_ids.len(),
        run_in_background = params.run_in_background,
    );
    run_improve(params, session_ids).instrument(span).await
}

async fn run_improve(
    params: ImproveParams<'_>,
    session_ids: Vec<String>,
) -> Result<ImproveResult, ApiError> {
    let ImproveParams {
        dataset_name,
        dataset_id,
        session_ids: _,
        // Deprecated options (SDK-1115), ignored as Python ignores them
        // without custom tasks.
        node_name: _,
        extraction_tasks: _,
        enrichment_tasks: _,
        data: _,
        checkpoint_store: _,
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

    let mut result = ImproveResult::new(dataset_name.clone(), session_ids.clone());
    result.background = run_in_background;

    // Single-session improve lock (Python `session_lock.py`, before its
    // waiting rework, which lands in SDK-1044 step 4): a concurrent improve of
    // the same session returns without running any stage.
    let _improve_guard = match session_ids.as_slice() {
        [sid] => match ImproveLockGuard::acquire(sid) {
            Some(guard) => Some(guard),
            None => {
                info!(session_id = %sid, "improve: session already being improved, skipping");
                return Ok(result);
            }
        },
        _ => None,
    };

    // Mirrors Python `send_telemetry("cognee.improve", ...)`.
    #[cfg(feature = "telemetry")]
    cognee_telemetry::send_telemetry(
        "cognee.improve",
        owner_id,
        Some(serde_json::json!({
            "dataset": dataset_name.clone(),
            "session_count": session_ids.len(),
            "session_ids": session_ids.clone(),
            "run_in_background": run_in_background,
            "cognee_version": env!("CARGO_PKG_VERSION"),
        })),
    );

    // Under per-dataset storage the graph the stages read and write is the
    // dataset's own. A dataset that does not exist yet has no graph to
    // weight; `persist_session_qa` may create it, so the later stages look it
    // up again.
    let dataset_id = match dataset_id {
        Some(id) => Some(id),
        None => dataset_id_by_name(db.as_ref(), &dataset_name, owner_id, tenant_id).await?,
    };
    let capabilities = GraphCapabilities::probe(&*graph_db, dataset_id).await;

    let has_sessions = !session_ids.is_empty();
    let mut stages = ImproveStages {
        dataset_name,
        owner_id,
        tenant_id,
        session_ids,
        feedback_alpha,
        build_truth_subspace,
        build_global_context_index,
        capabilities,
        dataset_id,
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
    };
    // `disabled_by_config` is wired to the improve settings in SDK-1044 step 3.
    let end = execute_stages(&DEFAULT_STAGES, has_sessions, &[], &mut stages, &mut result).await;
    result.dataset_id = match stages.dataset_id {
        Some(id) => Some(id),
        // A stage may have created the dataset without looking it up.
        None => dataset_id_by_name(
            stages.db.as_ref(),
            &stages.dataset_name,
            owner_id,
            tenant_id,
        )
        .await
        .unwrap_or(None),
    };
    info!(
        status = result.status().as_str(),
        stages = %result.stage_summary(),
        "improve: run finished"
    );

    match end {
        LoopEnd::Finished => Ok(result),
        LoopEnd::Aborted { budget_exhausted } => Err(ApiError::ImproveAborted {
            result: Box::new(result),
            budget_exhausted,
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    //! The stage loop, ported from Python's
    //! `cognee/tests/unit/api/v1/improve/test_improve_orchestration.py` and
    //! `test_improve_budget_exhaustion.py`, over scripted stages.

    use std::collections::HashMap;
    use std::future::Future;

    use super::*;

    /// A scripted stage outcome.
    #[derive(Clone)]
    enum Script {
        Complete,
        Error(&'static str),
        BudgetError,
        Gate(&'static str),
    }

    #[derive(Default)]
    struct Scripted {
        scripts: HashMap<StageKind, Script>,
        calls: Vec<&'static str>,
    }

    impl Scripted {
        fn with(mut self, stage: StageKind, script: Script) -> Self {
            self.scripts.insert(stage, script);
            self
        }

        fn script(&self, stage: StageKind) -> Script {
            self.scripts
                .get(&stage)
                .cloned()
                .unwrap_or(Script::Complete)
        }
    }

    impl StageExecutor for Scripted {
        fn gate(&self, stage: StageKind) -> Option<&'static str> {
            match self.script(stage) {
                Script::Gate(reason) => Some(reason),
                _ => None,
            }
        }

        fn run(&mut self, stage: StageKind) -> impl Future<Output = StageRun> + Send {
            self.calls.push(stage.name());
            let run = match self.script(stage) {
                Script::Complete | Script::Gate(_) => StageResult::completed(stage.name()).into(),
                Script::Error(error) => StageResult::errored(stage.name(), error).into(),
                Script::BudgetError => StageRun {
                    result: StageResult::errored(stage.name(), "LLM payment required: 402"),
                    budget_exhausted: true,
                },
            };
            std::future::ready(run)
        }
    }

    async fn run_loop(
        executor: &mut Scripted,
        has_sessions: bool,
        disabled: &[&str],
    ) -> (ImproveResult, LoopEnd) {
        let disabled: Vec<String> = disabled.iter().map(|s| s.to_string()).collect();
        let mut result = ImproveResult::new("docs", vec![]);
        let end = execute_stages(
            &DEFAULT_STAGES,
            has_sessions,
            &disabled,
            executor,
            &mut result,
        )
        .await;
        (result, end)
    }

    fn statuses(result: &ImproveResult) -> Vec<(&str, StageStatus, Option<&str>)> {
        result
            .stages
            .iter()
            .map(|s| (s.stage.as_str(), s.status, s.reason.as_deref()))
            .collect()
    }

    // test_stages_run_in_registry_order_and_result_lists_each
    #[tokio::test]
    async fn stages_run_in_registry_order_and_result_lists_each() {
        let mut executor = Scripted::default();
        let (result, end) = run_loop(&mut executor, true, &[]).await;
        let names: Vec<&str> = DEFAULT_STAGES.iter().map(|s| s.name()).collect();
        assert_eq!(executor.calls, names);
        assert_eq!(
            result
                .stages
                .iter()
                .map(|s| s.stage.as_str())
                .collect::<Vec<_>>(),
            names
        );
        assert!(
            result
                .stages
                .iter()
                .all(|s| s.status == StageStatus::Completed)
        );
        assert_eq!(result.status(), ImproveStatus::Completed);
        assert!(result.finished);
        assert_eq!(end, LoopEnd::Finished);
    }

    // test_gate_reasons: disabled_by_config, then no_session_ids, then the
    // stage's own gate.
    #[tokio::test]
    async fn gate_reasons_apply_in_order() {
        let mut executor = Scripted::default()
            .with(
                StageKind::FeedbackWeights,
                Script::Gate("backend_unsupported"),
            )
            .with(
                StageKind::ReviewConflicts,
                Script::Gate(REASON_OPT_IN_DISABLED),
            );
        let (result, _) = run_loop(
            &mut executor,
            false,
            &["global_context_index", "feedback_weights"],
        )
        .await;
        // A disabled stage wins over every other gate.
        assert_eq!(
            result.stage("feedback_weights").unwrap().reason.as_deref(),
            Some(REASON_DISABLED_BY_CONFIG)
        );
        assert_eq!(
            result
                .stage("global_context_index")
                .unwrap()
                .reason
                .as_deref(),
            Some(REASON_DISABLED_BY_CONFIG)
        );
        // Session-fed stages without sessions.
        for stage in DEFAULT_STAGES.iter().filter(|s| s.needs_sessions()).skip(1) {
            let recorded = result.stage(stage.name()).unwrap();
            assert_eq!(recorded.status, StageStatus::Skipped);
            assert_eq!(recorded.reason.as_deref(), Some(REASON_NO_SESSION_IDS));
            assert_eq!(recorded.duration_ms, 0);
        }
        // The stage's own gate.
        assert_eq!(
            result.stage("review_conflicts").unwrap().reason.as_deref(),
            Some(REASON_OPT_IN_DISABLED)
        );
        // Nothing ran: every stage was gated.
        assert!(executor.calls.is_empty());
        assert_eq!(result.status(), ImproveStatus::Skipped);
    }

    // test_session_stage_runs_when_session_ids_given; the graph-only stages
    // run without sessions.
    #[tokio::test]
    async fn graph_only_stages_run_without_sessions() {
        let mut executor = Scripted::default();
        let (result, _) = run_loop(&mut executor, false, &[]).await;
        assert_eq!(executor.calls, ["review_conflicts", "global_context_index"]);
        assert_eq!(result.status(), ImproveStatus::Completed);
    }

    // test_fatal_stage_stops_run_and_raises_with_partial_result
    #[tokio::test]
    async fn fatal_stage_stops_run_with_partial_result() {
        let mut executor =
            Scripted::default().with(StageKind::PersistSessionQa, Script::Error("persist failed"));
        let (result, end) = run_loop(&mut executor, true, &[]).await;
        assert_eq!(
            end,
            LoopEnd::Aborted {
                budget_exhausted: false
            }
        );
        assert_eq!(executor.calls, ["feedback_weights", "persist_session_qa"]);
        let recorded = statuses(&result);
        assert_eq!(recorded.len(), DEFAULT_STAGES.len());
        assert_eq!(
            recorded[0],
            ("feedback_weights", StageStatus::Completed, None)
        );
        assert_eq!(
            recorded[1],
            ("persist_session_qa", StageStatus::Errored, None)
        );
        assert_eq!(result.stages[1].error.as_deref(), Some("persist failed"));
        for (_, status, reason) in &recorded[2..] {
            assert_eq!(*status, StageStatus::Skipped);
            assert_eq!(*reason, Some(REASON_ABORTED_BY_FATAL_STAGE));
        }
        assert_eq!(result.error.as_deref(), Some("persist failed"));
        assert_eq!(result.status(), ImproveStatus::Errored);
    }

    // test_errored_non_fatal_stage_records_and_continues
    #[tokio::test]
    async fn errored_non_fatal_stage_records_and_continues() {
        let mut executor =
            Scripted::default().with(StageKind::FeedbackWeights, Script::Error("nope"));
        let (result, end) = run_loop(&mut executor, true, &[]).await;
        assert_eq!(end, LoopEnd::Finished);
        assert_eq!(executor.calls.len(), DEFAULT_STAGES.len());
        assert_eq!(result.stages[0].status, StageStatus::Errored);
        assert_eq!(result.stages[0].error.as_deref(), Some("nope"));
        assert_eq!(result.stages[1].status, StageStatus::Completed);
        assert!(result.error.is_none());
        assert_eq!(result.status(), ImproveStatus::Errored);
    }

    // test_improve_budget_exhaustion: a non-fatal stage on an exhausted
    // budget skips the rest and ends the run without an error.
    #[tokio::test]
    async fn budget_exhaustion_in_a_non_fatal_stage_skips_the_rest() {
        let mut executor =
            Scripted::default().with(StageKind::DistillSessions, Script::BudgetError);
        let (result, end) = run_loop(&mut executor, true, &[]).await;
        assert_eq!(end, LoopEnd::Finished);
        assert_eq!(executor.calls.last(), Some(&"distill_sessions"));
        let distill = result.stage("distill_sessions").unwrap();
        assert_eq!(distill.status, StageStatus::Errored);
        let after: Vec<_> = statuses(&result).into_iter().skip(5).collect();
        assert_eq!(after.len(), 4);
        for (_, status, reason) in after {
            assert_eq!(status, StageStatus::Skipped);
            assert_eq!(reason, Some(REASON_BUDGET_EXHAUSTED));
        }
        assert!(result.error.is_none());
        assert_eq!(result.status(), ImproveStatus::Errored);
    }

    // A budget error in the fatal stage aborts, with the remaining stages
    // skipped as budget_exhausted (Python `_abort_run(reason=...)`).
    #[tokio::test]
    async fn budget_exhaustion_in_the_fatal_stage_aborts_with_budget_reason() {
        let mut executor =
            Scripted::default().with(StageKind::PersistSessionQa, Script::BudgetError);
        let (result, end) = run_loop(&mut executor, true, &[]).await;
        assert_eq!(
            end,
            LoopEnd::Aborted {
                budget_exhausted: true
            }
        );
        assert!(
            result.stages[2..]
                .iter()
                .all(|s| s.reason.as_deref() == Some(REASON_BUDGET_EXHAUSTED))
        );
    }

    // Python `StageResult.budget_exhausted` falls back to the recorded error
    // (`is_budget_exhausted_record`): a stage that could only render the
    // provider's 402 — here `feedback_weights`, which reports no typed flag —
    // still stops the run.
    #[tokio::test]
    async fn a_rendered_budget_error_stops_the_run() {
        let mut executor = Scripted::default().with(
            StageKind::FeedbackWeights,
            Script::Error("graph write failed: LLM payment required: credit exhausted"),
        );
        let (result, end) = run_loop(&mut executor, true, &[]).await;
        assert_eq!(end, LoopEnd::Finished);
        assert_eq!(executor.calls, ["feedback_weights"]);
        assert!(
            result.stages[1..]
                .iter()
                .all(|s| s.reason.as_deref() == Some(REASON_BUDGET_EXHAUSTED))
        );

        // A LiteLLM budget sentence, in a fatal stage: aborts with the reason.
        let mut executor = Scripted::default().with(
            StageKind::PersistSessionQa,
            Script::Error("Budget has been exceeded! Current cost: 2.0, Max budget: 1.0"),
        );
        let (result, end) = run_loop(&mut executor, true, &[]).await;
        assert_eq!(
            end,
            LoopEnd::Aborted {
                budget_exhausted: true
            }
        );
        assert_eq!(
            result.stages[2].reason.as_deref(),
            Some(REASON_BUDGET_EXHAUSTED)
        );
    }

    // A budget signal on a stage that did not error is not an exhaustion.
    #[tokio::test]
    async fn a_budget_flag_without_an_error_is_ignored() {
        struct FlaggedButCompleted;
        impl StageExecutor for FlaggedButCompleted {
            fn gate(&self, _: StageKind) -> Option<&'static str> {
                None
            }
            fn run(&mut self, stage: StageKind) -> impl Future<Output = StageRun> + Send {
                std::future::ready(StageRun {
                    result: StageResult::completed(stage.name()),
                    budget_exhausted: true,
                })
            }
        }
        let mut result = ImproveResult::new("docs", vec![]);
        let end = execute_stages(
            &DEFAULT_STAGES,
            true,
            &[],
            &mut FlaggedButCompleted,
            &mut result,
        )
        .await;
        assert_eq!(end, LoopEnd::Finished);
        assert_eq!(result.status(), ImproveStatus::Completed);
    }
}
