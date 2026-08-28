//! Run orchestration's policy layer: what a finished run does about its
//! failures.
//!
//! The sweep ([`cognee_delete::RunSweeper`]) knows how to remove a scope; the
//! stages know how to record a failure. This module is the only place that
//! reads the two axes and decides *which* scope, *which* items get marked
//! complete, and what the run record says afterwards. It is deliberately the
//! last thing built and the easiest thing to revise.
//!
//! Nothing here ever returns an error to the caller. A sweep runs on the error
//! path, and a marker write runs after a run that already succeeded; in both
//! cases an error the caller never asked about must not replace the outcome
//! they did.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_core::pipeline::{PipelineRunInfo, PipelineStatus, PipelineWatcher, TaskStatus};
use cognee_core::pipeline_run_registry::{DbPipelineWatcher, data_info, run_info_for_errored};
use cognee_database::ops::data;
use cognee_database::{DatabaseConnection, PipelineRunRepository, PipelineRunStatus};
use cognee_delete::{RunSweeper, SweepScope};
use cognee_graph::GraphDBTrait;
use cognee_models::Data;
use cognee_vector::VectorDB;
use serde_json::{Map, Value};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::failure::{FailureReport, RollbackScope};

/// The `run_info` key a tolerantly-completed run's failure summary rides
/// under.
///
/// Additive: the `data` key Python's shape requires is still written first, so
/// the cross-SDK `pipeline_runs` shape assertions keep passing and a reader
/// that only knows Python's shape sees exactly what it expects.
pub const RUN_INFO_FAILURES_KEY: &str = "cognify_failures";

// ---------------------------------------------------------------------------
// Run-id capture
// ---------------------------------------------------------------------------

/// A [`PipelineWatcher`] that forwards to a [`DbPipelineWatcher`] and
/// remembers the run the executor minted.
///
/// The executor mints `run_id` internally and hands it to tasks through a
/// *cloned* [`cognee_core::TaskContext`], so `cognify()`'s own context never
/// learns it — and a run that fails produces no `CognifyResult` to read it
/// from. Without this, a failed run could not be swept: the sweep would have
/// no run to select on, and a sweep that selects nothing is the silent no-op
/// the plan calls worse than an honest refusal. Observing
/// `on_pipeline_run_initiated`, the first event the executor emits, gets the
/// id on every exit path — collected failure, persistence error, cancellation
/// alike.
pub(crate) struct RunIdCapturingWatcher {
    inner: DbPipelineWatcher,
    /// `(pipeline_run_id, pipeline_id)`, both needed to append a further row
    /// to the run's own trail.
    captured: Mutex<Option<(Uuid, Uuid)>>,
}

impl RunIdCapturingWatcher {
    pub(crate) fn new(repo: Arc<dyn PipelineRunRepository>) -> Self {
        Self {
            inner: DbPipelineWatcher::new(repo),
            captured: Mutex::new(None),
        }
    }

    /// The one place the mutex is taken. Lock poisoning means a thread already
    /// panicked while holding it; there is no state to recover to, so the
    /// project's sanctioned `lock().unwrap()` applies.
    #[allow(
        clippy::unwrap_used,
        reason = "lock poison is unrecoverable — the holder already panicked"
    )]
    fn ids(&self) -> std::sync::MutexGuard<'_, Option<(Uuid, Uuid)>> {
        self.captured.lock().unwrap()
    }

    /// The run id, once the executor has emitted its first lifecycle event.
    /// `None` when the run never started — a pipeline that failed to build.
    pub(crate) fn run_id(&self) -> Option<Uuid> {
        self.ids().map(|(run_id, _)| run_id)
    }

    /// The deterministic pipeline id the executor derived for this run.
    pub(crate) fn pipeline_id(&self) -> Option<Uuid> {
        self.ids().map(|(_, pipeline_id)| pipeline_id)
    }
}

#[async_trait]
impl PipelineWatcher for RunIdCapturingWatcher {
    async fn on_pipeline(&self, pipeline_id: Uuid, status: PipelineStatus) {
        self.inner.on_pipeline(pipeline_id, status).await;
    }

    async fn on_task(
        &self,
        pipeline_id: Uuid,
        task_index: usize,
        task_name: Option<&str>,
        total_tasks: usize,
        status: TaskStatus,
    ) {
        self.inner
            .on_task(pipeline_id, task_index, task_name, total_tasks, status)
            .await;
    }

    async fn on_pipeline_run_initiated(&self, run: &PipelineRunInfo) {
        *self.ids() = Some((run.run_id, run.pipeline_id));
        self.inner.on_pipeline_run_initiated(run).await;
    }

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        self.inner.on_pipeline_run_started(run).await;
    }

    async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, output_count: usize) {
        self.inner
            .on_pipeline_run_completed(run, output_count)
            .await;
    }

    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
        self.inner.on_pipeline_run_errored(run, error).await;
    }

    async fn on_task_started(&self, run: &PipelineRunInfo, task_name: &str, task_index: usize) {
        self.inner.on_task_started(run, task_name, task_index).await;
    }

    async fn on_task_completed(&self, run: &PipelineRunInfo, task_name: &str, result_count: usize) {
        self.inner
            .on_task_completed(run, task_name, result_count)
            .await;
    }

    async fn on_task_errored(&self, run: &PipelineRunInfo, task_name: &str, error: &str) {
        self.inner.on_task_errored(run, task_name, error).await;
    }

    async fn on_payload_field(&self, run_id: Uuid, key: &str, value: Value) {
        self.inner.on_payload_field(run_id, key, value).await;
    }
}

// ---------------------------------------------------------------------------
// The decision
// ---------------------------------------------------------------------------

/// How a run ended, from the policy layer's point of view.
pub(crate) enum RunEnding<'a> {
    /// The executor or the post-pipeline teardown returned an error. Covers
    /// collected stage failures the policy judged fatal, persistence errors,
    /// and cancellation alike — all of them mean "this run did not finish".
    ///
    /// Cancellation being in this list is a deliberate divergence: Python's
    /// rollback handler catches `Exception`, which excludes `CancelledError`,
    /// so a cancelled Python run keeps its partial graph.
    Failed,

    /// The executor returned `Ok`. The report may still carry failures the
    /// policy tolerated.
    Completed(&'a FailureReport),
}

/// What a finished run sweeps.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SweepDecision {
    /// Nothing is removed.
    Nothing,
    /// Everything this run created in the dataset.
    WholeRun,
    /// The failed and unreached items only. Never empty.
    Items(Vec<Uuid>),
}

/// Read the sweep axis and the run's ending, and say what goes.
///
/// | scope \ ending | `Failed` | `Completed` |
/// |---|---|---|
/// | `WholeRun` | `WholeRun` | `Nothing` |
/// | `FailedItems` | `WholeRun` — the escalation | `Items(failed ∪ unreached)` |
/// | `Nothing` | `Nothing` | `Nothing` |
///
/// The escalation is the one entry worth spelling out. A `FailedItems` run
/// that ended in failure did so because the chunk failure ratio was exceeded,
/// because no item survived, or because the failure was run-fatal
/// (persistence, cancellation). In every one of those the run as a whole did
/// not finish, so the honest end state is the pre-run one.
///
/// `unreached` is folded in with `failed` because neither is marked complete
/// and neither should keep artifacts. Under `FailFast` an unreached item wrote
/// no ownership rows, so the sweep is a no-op for it; under `RunToEnd` the set
/// is empty. Including it costs nothing and cannot under-sweep.
pub(crate) fn decide_sweep(scope: RollbackScope, ending: &RunEnding<'_>) -> SweepDecision {
    match (scope, ending) {
        (RollbackScope::Nothing, _) => SweepDecision::Nothing,
        (RollbackScope::WholeRun, RunEnding::Failed) => SweepDecision::WholeRun,
        // A completed `WholeRun` run has no failed items — `is_fatal` is true
        // for any of them under this scope — so there is nothing to remove.
        (RollbackScope::WholeRun, RunEnding::Completed(_)) => SweepDecision::Nothing,
        (RollbackScope::FailedItems, RunEnding::Failed) => SweepDecision::WholeRun,
        (RollbackScope::FailedItems, RunEnding::Completed(report)) => {
            let items = outstanding_items(report);
            if items.is_empty() {
                SweepDecision::Nothing
            } else {
                SweepDecision::Items(items)
            }
        }
    }
}

/// The items a completed run left behind: failed plus never attempted.
/// Deduplicated and ordered, because it is both a sweep selection and a
/// serialised run-record field.
fn outstanding_items(report: &FailureReport) -> Vec<Uuid> {
    report
        .failed_items()
        .union(report.unreached_items())
        .copied()
        .collect()
}

// ---------------------------------------------------------------------------
// The run record
// ---------------------------------------------------------------------------

/// `{"data": [...], "cognify_failures": {…}}` for a run that completed with
/// files still outstanding.
///
/// `data` is built with [`data_info`] so the shape cannot drift from the one
/// the watcher writes, and is inserted first so it stays first on the wire.
pub(crate) fn run_info_with_failures(data_ids: &[Uuid], report: &FailureReport) -> Value {
    let mut failures = Map::with_capacity(4);
    failures.insert(
        "failed_data_ids".into(),
        Value::Array(
            report
                .failed_items()
                .iter()
                .map(|id| Value::String(id.to_string()))
                .collect(),
        ),
    );
    failures.insert(
        "unreached_data_ids".into(),
        Value::Array(
            report
                .unreached_items()
                .iter()
                .map(|id| Value::String(id.to_string()))
                .collect(),
        ),
    );
    failures.insert("failure_count".into(), Value::from(report.total()));
    failures.insert(
        "chunk_failure_ratio".into(),
        Value::from(report.chunk_failure_ratio()),
    );

    let mut info = Map::with_capacity(2);
    info.insert("data".into(), data_info(data_ids));
    info.insert(RUN_INFO_FAILURES_KEY.into(), Value::Object(failures));
    Value::Object(info)
}

/// Whether a prior run's `run_info` says files it was responsible for are
/// still outstanding.
///
/// A completed run with outstanding files must never become a pipeline-cache
/// hit — the whole reason [`RUN_INFO_FAILURES_KEY`] exists. A row written
/// before this key existed, or by Python, carries no such key and is treated
/// as clean, which is the pre-change behaviour.
pub(crate) fn run_info_has_outstanding_failures(run_info: Option<&Value>) -> bool {
    let Some(failures) = run_info.and_then(|info| info.get(RUN_INFO_FAILURES_KEY)) else {
        return false;
    };
    ["failed_data_ids", "unreached_data_ids"].iter().any(|key| {
        failures
            .get(*key)
            .and_then(Value::as_array)
            .is_some_and(|ids| !ids.is_empty())
    })
}

// ---------------------------------------------------------------------------
// The two exit paths
// ---------------------------------------------------------------------------

/// Everything the policy layer needs about the run that just ended.
pub(crate) struct RunContext<'a> {
    pub(crate) database: Arc<DatabaseConnection>,
    pub(crate) graph_db: Arc<dyn GraphDBTrait>,
    pub(crate) vector_db: Arc<dyn VectorDB>,
    pub(crate) repo: Arc<dyn PipelineRunRepository>,
    pub(crate) dataset_id: Uuid,
    /// The run the executor minted, from [`RunIdCapturingWatcher::run_id`].
    /// `None` when no run ever started, in which case there is nothing to
    /// select a sweep on.
    pub(crate) pipeline_run_id: Option<Uuid>,
    pub(crate) pipeline_id: Option<Uuid>,
    pub(crate) pipeline_name: &'a str,
    /// The data items this run actually processed — the dataset's items minus
    /// the ones an earlier run had already marked complete.
    pub(crate) processed: Vec<Uuid>,
    pub(crate) config: &'a CognifyConfig,
}

impl RunContext<'_> {
    /// Run `scope` through a [`RunSweeper`], folding any failure of the sweep
    /// itself into a log line.
    async fn sweep(&self, scope: SweepScope) {
        let sweeper = RunSweeper::new(
            Arc::clone(&self.database),
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
        );
        let outcome = sweeper.sweep_logging_failure(&scope).await;
        info!(
            dataset_id = %self.dataset_id,
            pipeline_run_id = %scope.pipeline_run_id,
            graph_nodes_deleted = outcome.graph_nodes_deleted,
            vector_points_deleted = outcome.vector_points_deleted,
            provenance_nodes_deleted = outcome.provenance_nodes_deleted,
            provenance_edges_deleted = outcome.provenance_edges_deleted,
            data_items_unmarked = outcome.data_items_unmarked,
            "cognify: run sweep finished"
        );
        for warning in &outcome.warnings {
            warn!(pipeline_run_id = %scope.pipeline_run_id, "cognify: {warning}");
        }
    }

    /// The run id to sweep on, or `None` with an explanation logged.
    ///
    /// Both pipelines are swept the same way: the temporal persistence stage
    /// records ownership of everything it writes, so a temporal sweep selects
    /// real rows.
    fn sweepable_run_id(&self) -> Option<Uuid> {
        match self.pipeline_run_id {
            Some(run_id) => Some(run_id),
            None => {
                warn!(
                    dataset_id = %self.dataset_id,
                    "cognify: the run never started, so there is nothing to sweep"
                );
                None
            }
        }
    }

    /// Mark `data_ids` complete for this dataset's cognify pipeline.
    ///
    /// Gated on `incremental_loading` only. Both branches write under the one
    /// `cognify_pipeline` key the sweep's clearer and the delete path already
    /// use, matching Python, whose temporal cognify runs under that same
    /// pipeline name. The consequence is user-visible and deliberate: a dataset
    /// a standard run completed is a no-op for a later temporal run, and the
    /// other way round. Callers who want both graphs over one dataset set
    /// `with_incremental_loading(false)`.
    ///
    /// A write failure is a warning, never an error. The run completed; a
    /// missing marker only costs a redo, which is always the safe direction.
    async fn mark_complete(&self, data_ids: &[Uuid]) {
        if !self.config.incremental_loading || data_ids.is_empty() {
            return;
        }
        for data_id in data_ids {
            if let Err(e) = data::mark_cognify_pipeline_status_complete(
                &self.database,
                *data_id,
                self.dataset_id,
            )
            .await
            {
                warn!(
                    data_id = %data_id,
                    dataset_id = %self.dataset_id,
                    "cognify: failed to write the completion marker (the next run redoes this \
                     item): {e}"
                );
            }
        }
    }

    /// Append one row to this run's `pipeline_runs` trail.
    ///
    /// Best-effort for the same reason [`DbPipelineWatcher`]'s own writes are:
    /// the run is over either way, and a missing audit row must not become the
    /// caller's error.
    async fn append_run_row(&self, status: PipelineRunStatus, run_info: Value) {
        let (Some(pipeline_run_id), Some(pipeline_id)) = (self.pipeline_run_id, self.pipeline_id)
        else {
            return;
        };
        let label = format!("{status:?}");
        if let Err(e) = self
            .repo
            .log_pipeline_run(
                pipeline_run_id,
                pipeline_id,
                self.pipeline_name,
                Some(self.dataset_id),
                status,
                Some(run_info),
            )
            .await
        {
            warn!(
                pipeline_run_id = %pipeline_run_id,
                "cognify: failed to append the {label} run row (non-fatal): {e}"
            );
        }
    }
}

/// Sweep after a failed run, then make sure the run record says so.
///
/// Never returns an error: the caller's error is the one that matters.
///
/// `executor_recorded_the_error` distinguishes the two ways a run fails. When
/// the executor itself returned `Err` it has already written the `ERRORED`
/// row. When the post-pipeline teardown failed instead, the watcher has
/// already written `COMPLETED`, and without a further row a swept run would be
/// left looking complete — and would then be a pipeline-cache hit for a
/// dataset whose artifacts are gone.
pub(crate) async fn on_run_failed(
    ctx: &RunContext<'_>,
    error: &CognifyError,
    executor_recorded_the_error: bool,
) {
    let scope = ctx.config.rollback_scope;
    if decide_sweep(scope, &RunEnding::Failed) == SweepDecision::WholeRun
        && let Some(run_id) = ctx.sweepable_run_id()
    {
        info!(
            dataset_id = %ctx.dataset_id,
            pipeline_run_id = %run_id,
            ?scope,
            "cognify: run failed; sweeping everything it created"
        );
        ctx.sweep(SweepScope::whole_run(run_id, ctx.dataset_id))
            .await;
    }

    if !executor_recorded_the_error {
        ctx.append_run_row(
            PipelineRunStatus::Errored,
            run_info_for_errored(&ctx.processed, &error.to_string()),
        )
        .await;
    }
}

/// Sweep the failed items, mark the survivors, and attach the failure list to
/// the run record.
///
/// Never returns an error: the run completed.
pub(crate) async fn on_run_completed(ctx: &RunContext<'_>, report: &FailureReport) {
    let scope = ctx.config.rollback_scope;
    let outstanding = outstanding_items(report);

    if let SweepDecision::Items(items) = decide_sweep(scope, &RunEnding::Completed(report))
        && let Some(run_id) = ctx.sweepable_run_id()
    {
        info!(
            dataset_id = %ctx.dataset_id,
            pipeline_run_id = %run_id,
            item_count = items.len(),
            "cognify: run completed with failures; sweeping only the failed files"
        );
        ctx.sweep(SweepScope::for_data(run_id, ctx.dataset_id, items))
            .await;
    }

    // Survivors only — every processed item that neither failed nor went
    // unreached. An item dropped at classification is recorded as unreached,
    // so it is excluded here: marking it would claim a file was cognified that
    // never reached a single stage, and the marker would then skip it forever.
    let survivors: Vec<Uuid> = ctx
        .processed
        .iter()
        .filter(|id| !outstanding.contains(id))
        .copied()
        .collect();
    ctx.mark_complete(&survivors).await;

    // Only when there is something to attach. A clean run's `pipeline_runs`
    // trail stays byte-identical to what it was before this change, which is
    // what keeps the cross-SDK shape parity suite honest rather than merely
    // passing.
    if !outstanding.is_empty() {
        ctx.append_run_row(
            PipelineRunStatus::Completed,
            run_info_with_failures(&ctx.processed, report),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// Entry: completion markers are read
// ---------------------------------------------------------------------------

/// Drop the items an earlier run already marked complete for `dataset_id`,
/// returning the ones this run must actually process.
///
/// This is where `incremental_loading` finally means something. Python reaches
/// the same outcome per item inside `run_tasks_data_item_incremental`; Rust
/// reaches it for the dataset, before the pipeline is built, so a skipped item
/// is never classified, never chunked and never sent to an LLM.
pub(crate) async fn drop_already_complete(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    items: Vec<Data>,
) -> Result<Vec<Data>, CognifyError> {
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let completed = data::get_cognify_completed_data_ids(db, dataset_id, &ids)
        .await
        .map_err(|e| CognifyError::DatabaseError(e.to_string()))?;

    if completed.is_empty() {
        return Ok(items);
    }

    let remaining: Vec<Data> = items
        .into_iter()
        .filter(|item| !completed.contains(&item.id))
        .collect();
    info!(
        dataset_id = %dataset_id,
        skipped = completed.len(),
        remaining = remaining.len(),
        "cognify: skipping data items an earlier run already completed"
    );
    Ok(remaining)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee_core::pipeline::ExecutionError;

    use super::*;
    use crate::failure::{FailurePolicy, FailureStage, StageFailure};

    fn report_with(failed: &[Uuid], unreached: &[Uuid]) -> FailureReport {
        let mut report = FailureReport::default();
        report.note_totals(failed.len() + unreached.len(), 10);
        for id in failed {
            report.record(StageFailure {
                stage: FailureStage::GraphExtraction,
                data_id: *id,
                chunk_id: Some(Uuid::new_v4()),
                error: "boom".to_string(),
                fails_item: true,
            });
        }
        for id in unreached {
            report.mark_unreached(*id);
        }
        report
    }

    #[test]
    fn decide_sweep_over_the_whole_matrix() {
        let failed = Uuid::new_v4();
        let unreached = Uuid::new_v4();
        let report = report_with(&[failed], &[unreached]);

        assert_eq!(
            decide_sweep(RollbackScope::WholeRun, &RunEnding::Failed),
            SweepDecision::WholeRun
        );
        assert_eq!(
            decide_sweep(RollbackScope::WholeRun, &RunEnding::Completed(&report)),
            SweepDecision::Nothing,
            "a completed WholeRun run has no failed items to sweep"
        );
        assert_eq!(
            decide_sweep(RollbackScope::Nothing, &RunEnding::Failed),
            SweepDecision::Nothing
        );
        assert_eq!(
            decide_sweep(RollbackScope::Nothing, &RunEnding::Completed(&report)),
            SweepDecision::Nothing
        );
        assert_eq!(
            decide_sweep(RollbackScope::FailedItems, &RunEnding::Failed),
            SweepDecision::WholeRun,
            "the escalation: a FailedItems run that errored sweeps everything"
        );

        match decide_sweep(RollbackScope::FailedItems, &RunEnding::Completed(&report)) {
            SweepDecision::Items(items) => {
                assert_eq!(items.len(), 2);
                assert!(items.contains(&failed));
                assert!(
                    items.contains(&unreached),
                    "an unreached item is swept alongside a failed one"
                );
            }
            other => panic!("expected an item-scoped sweep, got {other:?}"),
        }
    }

    #[test]
    fn a_clean_completed_run_sweeps_nothing_under_every_scope() {
        let clean = FailureReport::default();
        for scope in [
            RollbackScope::WholeRun,
            RollbackScope::FailedItems,
            RollbackScope::Nothing,
        ] {
            assert_eq!(
                decide_sweep(scope, &RunEnding::Completed(&clean)),
                SweepDecision::Nothing,
                "scope={scope:?}"
            );
        }
    }

    /// Cancellation is swept — the deliberate divergence from Python, whose
    /// rollback handler catches `Exception` and so never fires on
    /// `CancelledError`.
    ///
    /// What makes it hold is that cancellation never reaches the policy as
    /// anything of its own. This test pins both halves of that: the executor's
    /// `Cancelled` is flattened by the real
    /// [`crate::tasks::unwrap_execution_error`] into the same shapeless
    /// [`CognifyError::Execute`] every other non-task failure gets — so a
    /// future error-shape check in `cognify()`'s `Err` arm would have nothing
    /// to match on — and every `Failed` ending under a sweeping scope sweeps
    /// the whole run regardless.
    #[test]
    fn a_cancelled_run_is_indistinguishable_from_any_other_failure() {
        let cancelled = crate::tasks::unwrap_execution_error(ExecutionError::Cancelled);
        assert!(
            matches!(&cancelled, CognifyError::Execute(msg) if msg.contains("cancelled")),
            "a dedicated variant here is the one thing that would give the \
             policy an error shape to branch on; got {cancelled:?}"
        );

        for scope in [RollbackScope::WholeRun, RollbackScope::FailedItems] {
            assert_eq!(
                decide_sweep(scope, &RunEnding::Failed),
                SweepDecision::WholeRun,
                "scope={scope:?}"
            );
        }
        assert_eq!(
            decide_sweep(RollbackScope::Nothing, &RunEnding::Failed),
            SweepDecision::Nothing,
            "…except the escape hatch, which is the point of the escape hatch"
        );
    }

    #[test]
    fn run_info_round_trips_through_the_cache_gate() {
        let failed = Uuid::new_v4();
        let unreached = Uuid::new_v4();
        let report = report_with(&[failed], &[unreached]);
        let processed = vec![failed, unreached, Uuid::new_v4()];

        let info = run_info_with_failures(&processed, &report);
        assert!(
            info.get("data").is_some(),
            "Python's shape requires `data`, and it must stay first"
        );
        assert_eq!(
            info.as_object().unwrap().keys().next().map(String::as_str),
            Some("data")
        );
        assert!(run_info_has_outstanding_failures(Some(&info)));

        let failures = info.get(RUN_INFO_FAILURES_KEY).unwrap();
        assert_eq!(failures.get("failure_count").unwrap(), 1);
        assert_eq!(
            failures.get("failed_data_ids").unwrap().as_array().unwrap(),
            &[Value::String(failed.to_string())]
        );
    }

    #[test]
    fn a_clean_run_info_is_not_an_outstanding_one() {
        assert!(!run_info_has_outstanding_failures(None));
        assert!(!run_info_has_outstanding_failures(Some(
            &serde_json::json!({
                "data": ["a", "b"]
            })
        )));
        assert!(
            !run_info_has_outstanding_failures(Some(&run_info_with_failures(
                &[Uuid::new_v4()],
                &FailureReport::default()
            ))),
            "an empty report leaves both id lists empty, so nothing is outstanding"
        );
    }

    /// The report cap bounds the entry list but never the id sets, so the
    /// sweep selection stays complete. Pinned here because `decide_sweep`
    /// reads exactly those sets.
    #[test]
    fn the_sweep_selection_survives_the_report_cap() {
        let policy = FailurePolicy {
            report_cap: 1,
            ..FailurePolicy::default()
        };
        let mut report = FailureReport::with_policy(&policy);
        report.note_totals(5, 50);
        for _ in 0..5 {
            report.record(StageFailure {
                stage: FailureStage::GraphExtraction,
                data_id: Uuid::new_v4(),
                chunk_id: Some(Uuid::new_v4()),
                error: "boom".to_string(),
                fails_item: true,
            });
        }

        match decide_sweep(RollbackScope::FailedItems, &RunEnding::Completed(&report)) {
            SweepDecision::Items(items) => assert_eq!(items.len(), 5),
            other => panic!("expected an item-scoped sweep, got {other:?}"),
        }
    }

    /// A run whose *teardown* fails is left errored, not complete.
    ///
    /// The watcher writes `COMPLETED` as the executor returns, so an error
    /// raised after that — the post-pipeline DLT teardown — must append its own
    /// `ERRORED` row. Without it a swept run would be left looking complete,
    /// and would then be a pipeline-cache hit for a dataset whose artifacts are
    /// gone.
    #[tokio::test]
    async fn a_failure_after_the_executor_returned_appends_an_errored_row() {
        let (db, dataset_id, ctx_parts) = run_context_fixture().await;
        let config = CognifyConfig::default();
        let processed = vec![Uuid::new_v4()];
        let ctx = RunContext {
            database: Arc::clone(&db),
            graph_db: ctx_parts.0,
            vector_db: ctx_parts.1,
            repo: Arc::clone(&ctx_parts.2),
            dataset_id,
            pipeline_run_id: Some(ctx_parts.3),
            pipeline_id: Some(ctx_parts.4),
            pipeline_name: "cognify_pipeline",
            processed: processed.clone(),
            config: &config,
        };

        // The watcher's own COMPLETED row, as it stands when the teardown runs.
        ctx_parts
            .2
            .log_pipeline_run(
                ctx_parts.3,
                ctx_parts.4,
                "cognify_pipeline",
                Some(dataset_id),
                PipelineRunStatus::Completed,
                Some(cognee_core::pipeline_run_registry::run_info_for_running(
                    &processed,
                )),
            )
            .await
            .expect("seed the COMPLETED row");

        let error = CognifyError::GraphStorageError("teardown blew up".to_string());
        on_run_failed(&ctx, &error, /* executor_recorded_the_error */ false).await;

        let rows = ctx_parts
            .2
            .list_recent(Some(dataset_id), 10)
            .await
            .expect("list runs");
        assert_eq!(rows[0].status, PipelineRunStatus::Errored);
        let info = rows[0]
            .run_info
            .as_ref()
            .expect("the ERRORED row carries run_info");
        assert_eq!(
            info.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["data", "error"],
            "Python's shape: `data` before `error`"
        );
        assert!(
            info["error"].as_str().unwrap().contains("teardown blew up"),
            "the row names the failure that actually happened: {info}"
        );
    }

    /// When the executor already wrote the `ERRORED` row, the policy layer
    /// appends nothing — one failure is one row.
    #[tokio::test]
    async fn an_executor_recorded_failure_appends_no_further_row() {
        let (db, dataset_id, ctx_parts) = run_context_fixture().await;
        let config = CognifyConfig::default();
        let ctx = RunContext {
            database: Arc::clone(&db),
            graph_db: ctx_parts.0,
            vector_db: ctx_parts.1,
            repo: Arc::clone(&ctx_parts.2),
            dataset_id,
            pipeline_run_id: Some(ctx_parts.3),
            pipeline_id: Some(ctx_parts.4),
            pipeline_name: "cognify_pipeline",
            processed: Vec::new(),
            config: &config,
        };

        on_run_failed(
            &ctx,
            &CognifyError::Execute("boom".to_string()),
            /* executor_recorded_the_error */ true,
        )
        .await;

        assert!(
            ctx_parts
                .2
                .list_recent(Some(dataset_id), 10)
                .await
                .expect("list runs")
                .is_empty(),
            "nothing was appended over the executor's own row"
        );
    }

    /// A temporal run is swept like any other. It writes its own ownership
    /// rows now, so there is no branch left that selects nothing while
    /// reporting success.
    #[tokio::test]
    async fn a_temporal_run_is_swept_like_a_standard_one() {
        let (db, dataset_id, ctx_parts) = run_context_fixture().await;
        let config = CognifyConfig::default();
        let run_id = ctx_parts.3;
        let ctx = RunContext {
            database: db,
            graph_db: ctx_parts.0,
            vector_db: ctx_parts.1,
            repo: ctx_parts.2,
            dataset_id,
            pipeline_run_id: Some(run_id),
            pipeline_id: Some(ctx_parts.4),
            pipeline_name: "temporal-cognify",
            processed: Vec::new(),
            config: &config,
        };

        assert_eq!(ctx.sweepable_run_id(), Some(run_id));
    }

    /// A run that never started has nothing to select a sweep on.
    #[tokio::test]
    async fn a_run_that_never_started_is_not_swept() {
        let (db, dataset_id, ctx_parts) = run_context_fixture().await;
        let config = CognifyConfig::default();
        let ctx = RunContext {
            database: db,
            graph_db: ctx_parts.0,
            vector_db: ctx_parts.1,
            repo: ctx_parts.2,
            dataset_id,
            pipeline_run_id: None,
            pipeline_id: None,
            pipeline_name: "cognify_pipeline",
            processed: Vec::new(),
            config: &config,
        };

        assert!(ctx.sweepable_run_id().is_none());
    }

    /// Everything a [`RunContext`] needs, over in-memory SQLite and mock
    /// stores.
    #[allow(clippy::type_complexity)]
    async fn run_context_fixture() -> (
        Arc<DatabaseConnection>,
        Uuid,
        (
            Arc<dyn GraphDBTrait>,
            Arc<dyn VectorDB>,
            Arc<dyn PipelineRunRepository>,
            Uuid,
            Uuid,
        ),
    ) {
        let conn = cognee_database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite");
        cognee_database::initialize(&conn)
            .await
            .expect("initialize");
        let db = Arc::new(conn);
        let dataset_id = Uuid::new_v4();
        let repo: Arc<dyn PipelineRunRepository> = Arc::new(
            cognee_database::SeaOrmPipelineRunRepository::new(Arc::clone(&db)),
        );
        (
            db,
            dataset_id,
            (
                Arc::new(cognee_test_utils::MockGraphDB::new()),
                Arc::new(cognee_test_utils::MockVectorDB::new()),
                repo,
                Uuid::new_v4(),
                Uuid::new_v4(),
            ),
        )
    }
}
