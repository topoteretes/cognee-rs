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

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_core::pipeline::{PipelineRunInfo, PipelineStatus, PipelineWatcher, TaskStatus};
use cognee_core::pipeline_run_registry::{DbPipelineWatcher, run_info_for_errored};
use cognee_database::{PipelineRunRepository, PipelineRunStatus, RelationalDb};
use cognee_delete::{RunSweeper, SweepScope};
use cognee_graph::GraphDBTrait;
use cognee_models::Data;
use cognee_vector::VectorDB;
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::failure::{FailureReport, RollbackScope};

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
/// `on_pipeline_run_started`, the first event the executor emits, gets the
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

    /// Whether the run's `COMPLETED` / `ERRORED` row failed to write: the run
    /// is then still `STARTED`, and its ownership marker must be left for
    /// startup recovery ([`DbPipelineWatcher::terminal_write_failed`]).
    pub(crate) fn terminal_write_failed(&self) -> bool {
        self.inner.terminal_write_failed()
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

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        *self.ids() = Some((run.run_id, run.pipeline_id));
        self.inner.on_pipeline_run_started(run).await;
    }

    async fn on_pipeline_run_completed(
        &self,
        run: &PipelineRunInfo,
        output_count: usize,
    ) -> Result<(), cognee_core::WatcherError> {
        self.inner
            .on_pipeline_run_completed(run, output_count)
            .await
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
// Per-document completion
// ---------------------------------------------------------------------------

/// The documents a [`RollbackScope::FailedItems`] run has **completed** so
/// far, recorded the moment each one is (SDK-1694 O5b).
///
/// Python stamps a data item complete as soon as its own tasks have run
/// (`run_tasks_data_item`), and its rollback keeps exactly the items stamped
/// by the failed run (`rollback.py` `_kept_data_ids`). This is that stamp for
/// Rust's one-pipeline run: the document stage's writer completes a document
/// right after its group is saved, the code routes complete a code item right
/// after its graph is loaded, and the post-run step completes nothing — a DLT
/// document is completed at the end of the run, after its foreign-key edges.
///
/// "Completed" means saved, and — with `incremental_loading` — marked: the
/// marker is written first and the document is recorded only once it is in,
/// so a document whose save or marker write never finished (an error, or the
/// stage dropped mid-save) is not completed and the rollback treats it as
/// unsaved. A marker write that fails is a warning, as at the end of a run:
/// the document is then not completed, and a failed run sweeps it, as Python
/// does with an unstamped item.
///
/// Only a `FailedItems` run has one. A `WholeRun` run sweeps everything when
/// it fails, completion or not, so it marks its items at the end; a `Nothing`
/// run marks nothing it did not finish whole.
pub(crate) struct CompletionLog {
    database: Arc<dyn RelationalDb>,
    dataset_id: Uuid,
    write_markers: bool,
    completed: Mutex<BTreeSet<Uuid>>,
}

impl CompletionLog {
    /// A log for `dataset_id`'s run; `write_markers` is the run's
    /// `incremental_loading`.
    pub(crate) fn new(
        database: Arc<dyn RelationalDb>,
        dataset_id: Uuid,
        write_markers: bool,
    ) -> Self {
        Self {
            database,
            dataset_id,
            write_markers,
            completed: Mutex::new(BTreeSet::new()),
        }
    }

    #[allow(
        clippy::unwrap_used,
        reason = "lock poison is unrecoverable — the holder already panicked"
    )]
    fn guard(&self) -> std::sync::MutexGuard<'_, BTreeSet<Uuid>> {
        self.completed.lock().unwrap()
    }

    /// Record `data_id` as completed: write its marker (with
    /// `incremental_loading`), then remember it. `false` when the marker
    /// write failed, in which case the document is not completed.
    pub(crate) async fn complete(&self, data_id: Uuid) -> bool {
        if self.write_markers
            && let Err(e) = cognee_database::ProvenanceDb::mark_cognify_pipeline_status_complete(
                &*self.database,
                data_id,
                self.dataset_id,
            )
            .await
        {
            warn!(
                data_id = %data_id,
                dataset_id = %self.dataset_id,
                "cognify: failed to write the completion marker (the document is not kept if \
                 the run fails, and the next run redoes it): {e}"
            );
            return false;
        }
        self.guard().insert(data_id);
        true
    }

    /// Every document completed so far.
    pub(crate) fn completed(&self) -> BTreeSet<Uuid> {
        self.guard().clone()
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
    /// `completed` are the documents the run completed before it failed
    /// ([`CompletionLog`]); only a `FailedItems` run has any.
    ///
    /// Cancellation being in this list is a deliberate divergence: Python's
    /// rollback handler catches `Exception`, which excludes `CancelledError`,
    /// so a cancelled Python run keeps its partial graph.
    Failed { completed: &'a BTreeSet<Uuid> },

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
    /// Everything this run created in the dataset except what these items —
    /// the ones it completed — own. Never empty.
    WholeRunKeeping(Vec<Uuid>),
    /// The given items only. Never empty.
    Items(Vec<Uuid>),
}

/// Read the sweep axis and the run's ending, and say what goes.
///
/// | scope \ ending | `Failed` | `Completed` |
/// |---|---|---|
/// | `WholeRun` | `WholeRun` | `Nothing` |
/// | `FailedItems` | `WholeRunKeeping(completed)` (`WholeRun` when it completed nothing) | `Items(failed ∪ unreached)` |
/// | `Nothing` | `Nothing` | `Nothing` |
///
/// A `FailedItems` run fails whenever an item failed
/// ([`FailureReport::is_fatal`]), when a save outside one document failed, or
/// when it was cancelled. It reports the error — but the documents it
/// **completed** are kept, markers and all, as Python's per-item rollback
/// keeps the items its failed run stamped (`rollback.py` `_kept_data_ids`,
/// `Node.data_id.notin_(kept)`). Everything else the run wrote is swept: the
/// documents that failed, the ones it was still saving, and the run-level
/// rows no kept document stands beside (a DLT schema node). It is expressed
/// as "the whole run except", not as a list of the swept items, so the sweep
/// binds no list whatever the run's size
/// ([`cognee_delete::RunSweeper::sweep_run_keeping`]).
///
/// Before SDK-1694 O5b this swept the whole run, the completed documents
/// included, because documents were saved and marked only at the end. A
/// caller who would rather converge on the pre-run state selects `WholeRun`.
///
/// A completed `FailedItems` run failed no item; what it can leave behind are
/// items no document type matched, recorded as unreached. They wrote no rows,
/// so their item sweep is a no-op, kept so that it cannot under-sweep.
pub(crate) fn decide_sweep(scope: RollbackScope, ending: &RunEnding<'_>) -> SweepDecision {
    match (scope, ending) {
        (RollbackScope::Nothing, _) => SweepDecision::Nothing,
        (RollbackScope::WholeRun, RunEnding::Failed { .. }) => SweepDecision::WholeRun,
        // A completed `WholeRun` run has no failed items — `is_fatal` is true
        // for any of them — so there is nothing to remove.
        (RollbackScope::WholeRun, RunEnding::Completed(_)) => SweepDecision::Nothing,
        (RollbackScope::FailedItems, RunEnding::Failed { completed }) => {
            if completed.is_empty() {
                SweepDecision::WholeRun
            } else {
                SweepDecision::WholeRunKeeping(completed.iter().copied().collect())
            }
        }
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
// The two exit paths
// ---------------------------------------------------------------------------

/// Everything the policy layer needs about the run that just ended.
pub(crate) struct RunContext<'a> {
    pub(crate) database: Arc<dyn RelationalDb>,
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
    /// The documents the run completed as it went ([`CompletionLog`]): saved
    /// and, with `incremental_loading`, already marked. Empty for a run that
    /// has no log (every scope but `FailedItems`).
    pub(crate) completed: BTreeSet<Uuid>,
    pub(crate) config: &'a CognifyConfig,
}

impl RunContext<'_> {
    /// Run `scope` through a [`RunSweeper`], folding any failure of the sweep
    /// itself into a log line.
    async fn sweep(&self, scope: SweepScope) {
        let sweeper = self.sweeper();
        let outcome = sweeper.sweep_logging_failure(&scope).await;
        self.log_sweep(scope.pipeline_run_id, &outcome);
    }

    /// Sweep the whole run except what `keep` owns
    /// ([`RunSweeper::sweep_run_keeping`]), folding any failure into a log
    /// line.
    async fn sweep_keeping(&self, pipeline_run_id: Uuid, keep: &[Uuid]) {
        let outcome = self
            .sweeper()
            .sweep_run_keeping_logging_failure(pipeline_run_id, self.dataset_id, keep)
            .await;
        self.log_sweep(pipeline_run_id, &outcome);
    }

    fn sweeper(&self) -> RunSweeper {
        RunSweeper::new(
            Arc::clone(&self.database),
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
        )
    }

    fn log_sweep(&self, pipeline_run_id: Uuid, outcome: &cognee_delete::SweepOutcome) {
        info!(
            dataset_id = %self.dataset_id,
            pipeline_run_id = %pipeline_run_id,
            graph_nodes_deleted = outcome.graph_nodes_deleted,
            vector_points_deleted = outcome.vector_points_deleted,
            provenance_nodes_deleted = outcome.provenance_nodes_deleted,
            provenance_edges_deleted = outcome.provenance_edges_deleted,
            data_items_unmarked = outcome.data_items_unmarked,
            "cognify: run sweep finished"
        );
        for warning in &outcome.warnings {
            warn!(pipeline_run_id = %pipeline_run_id, "cognify: {warning}");
        }
    }

    /// The run id to sweep on, or `None` with an explanation logged.
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
    /// Gated on `incremental_loading` only. Writes under the one
    /// `cognify_pipeline` key the sweep's clearer and the delete path already
    /// use, matching Python.
    ///
    /// A write failure is a warning, never an error. The run completed; a
    /// missing marker only costs a redo, which is always the safe direction.
    async fn mark_complete(&self, data_ids: &[Uuid]) {
        if !self.config.incremental_loading || data_ids.is_empty() {
            return;
        }
        for data_id in data_ids {
            if let Err(e) = cognee_database::ProvenanceDb::mark_cognify_pipeline_status_complete(
                &*self.database,
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

    /// Keep the completed documents' edge evidence readable after the run
    /// errored (Python `keep_completed_items_evidence`, called by its
    /// rollback in the transaction that resets the statuses).
    ///
    /// The rows carry the evidence run id — the host's when it set
    /// [`CognifyConfig::evidence_pipeline_run_id`], else the executor's.
    /// A failure is a warning: the documents stay kept either way, and their
    /// evidence is only hidden from `include_references` until they are
    /// cognified again. Python fails its rollback instead, which leaves the
    /// statuses unreset; Rust's sweep has already run by now.
    async fn keep_completed_items_evidence(&self) {
        let Some(run_id) = self
            .config
            .evidence_pipeline_run_id
            .or(self.pipeline_run_id)
        else {
            return;
        };
        let data_ids: Vec<Uuid> = self.completed.iter().copied().collect();
        match cognee_database::ProvenanceDb::keep_completed_items_evidence(
            &*self.database,
            self.dataset_id,
            run_id,
            &data_ids,
        )
        .await
        {
            Ok(rows) => info!(
                dataset_id = %self.dataset_id,
                pipeline_run_id = %run_id,
                rows,
                "cognify: kept the completed documents' edge evidence"
            ),
            Err(e) => warn!(
                dataset_id = %self.dataset_id,
                pipeline_run_id = %run_id,
                "cognify: failed to keep the completed documents' edge evidence readable: {e}"
            ),
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
/// left looking complete although its artifacts are gone.
pub(crate) async fn on_run_failed(
    ctx: &RunContext<'_>,
    error: &CognifyError,
    executor_recorded_the_error: bool,
) {
    let scope = ctx.config.failure_policy().scope;
    let ending = RunEnding::Failed {
        completed: &ctx.completed,
    };
    match decide_sweep(scope, &ending) {
        SweepDecision::Nothing => {}
        SweepDecision::WholeRun => {
            if let Some(run_id) = ctx.sweepable_run_id() {
                info!(
                    dataset_id = %ctx.dataset_id,
                    pipeline_run_id = %run_id,
                    ?scope,
                    "cognify: run failed; sweeping everything it created"
                );
                ctx.sweep(SweepScope::whole_run(run_id, ctx.dataset_id))
                    .await;
            }
        }
        SweepDecision::WholeRunKeeping(keep) => {
            if let Some(run_id) = ctx.sweepable_run_id() {
                info!(
                    dataset_id = %ctx.dataset_id,
                    pipeline_run_id = %run_id,
                    kept = keep.len(),
                    "cognify: run failed; keeping the documents it completed and sweeping the rest"
                );
                ctx.sweep_keeping(run_id, &keep).await;
            }
        }
        // A failed run's ending never selects items.
        SweepDecision::Items(items) => {
            if let Some(run_id) = ctx.sweepable_run_id() {
                ctx.sweep(SweepScope::for_data(run_id, ctx.dataset_id, items))
                    .await;
            }
        }
    }
    // The kept documents' edge evidence would be hidden with the errored run,
    // and the next run skips them, so nothing would record it again.
    if scope == RollbackScope::FailedItems && !ctx.completed.is_empty() {
        ctx.keep_completed_items_evidence().await;
    }

    if !executor_recorded_the_error {
        ctx.append_run_row(
            PipelineRunStatus::Errored,
            run_info_for_errored(&error.to_string()),
        )
        .await;
    }
}

/// Sweep the failed items and mark the survivors complete.
///
/// Never returns an error: the run completed.
pub(crate) async fn on_run_completed(ctx: &RunContext<'_>, report: &FailureReport) {
    let scope = ctx.config.failure_policy().scope;
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
    // A document the run already completed as it went is marked already; what
    // is left is a DLT document, completed only now that its foreign-key
    // edges are written, or every survivor of a run without a log.
    let survivors: Vec<Uuid> = ctx
        .processed
        .iter()
        .filter(|id| !outstanding.contains(id) && !ctx.completed.contains(id))
        .copied()
        .collect();
    ctx.mark_complete(&survivors).await;
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
    db: &dyn RelationalDb,
    dataset_id: Uuid,
    items: Vec<Data>,
) -> Result<Vec<Data>, CognifyError> {
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let completed =
        cognee_database::ProvenanceDb::get_cognify_completed_data_ids(db, dataset_id, &ids)
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

/// Whether the items `cognify()` will actually process for `dataset_id` need
/// the LLM: [`routes_need_llm`](crate::routing::routes_need_llm) over `items`
/// minus what an earlier run completed (the completion markers `cognify()`
/// skips by, applied only with `incremental_loading`, as `cognify()` applies
/// them).
///
/// Callers that resolve the graph extractor (and the LLM it may need) only
/// when a standard item will reach it ask this rather than
/// `routes_need_llm(items)`: a dataset whose standard items are already
/// cognified and that gains a code repository must not need an LLM.
///
/// The markers are read without the dataset lock `cognify()` takes, so a
/// marker cleared in between (a concurrent delete and re-add of a completed
/// standard item) is not seen: that item then runs without the extractor
/// the caller left unresolved. The window is a concurrent rewrite of the
/// same item, and the run it affects is the one that races it.
///
/// # Errors
///
/// [`CognifyError::DatabaseError`] when the completion markers cannot be read.
pub async fn pending_routes_need_llm(
    db: &dyn RelationalDb,
    dataset_id: Uuid,
    items: &[Data],
    incremental_loading: bool,
) -> Result<bool, CognifyError> {
    if !crate::routing::routes_need_llm(items) {
        return Ok(false);
    }
    if !incremental_loading {
        return Ok(true);
    }
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let completed =
        cognee_database::ProvenanceDb::get_cognify_completed_data_ids(db, dataset_id, &ids)
            .await
            .map_err(|e| CognifyError::DatabaseError(e.to_string()))?;
    let pending: Vec<Data> = items
        .iter()
        .filter(|item| !completed.contains(&item.id))
        .cloned()
        .collect();
    Ok(crate::routing::routes_need_llm(&pending))
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
    use cognee_database::ops::data;

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
        let saved = Uuid::new_v4();
        let nothing_completed = BTreeSet::new();
        let saved_completed = BTreeSet::from([saved]);
        let failed_with = |completed| RunEnding::Failed { completed };

        for completed in [&nothing_completed, &saved_completed] {
            assert_eq!(
                decide_sweep(RollbackScope::WholeRun, &failed_with(completed)),
                SweepDecision::WholeRun,
                "WholeRun sweeps the whole run whatever it completed"
            );
            assert_eq!(
                decide_sweep(RollbackScope::Nothing, &failed_with(completed)),
                SweepDecision::Nothing
            );
        }
        assert_eq!(
            decide_sweep(RollbackScope::WholeRun, &RunEnding::Completed(&report)),
            SweepDecision::Nothing,
            "a completed WholeRun run has no failed items to sweep"
        );
        assert_eq!(
            decide_sweep(RollbackScope::Nothing, &RunEnding::Completed(&report)),
            SweepDecision::Nothing
        );
        assert_eq!(
            decide_sweep(RollbackScope::FailedItems, &failed_with(&nothing_completed)),
            SweepDecision::WholeRun,
            "the escalation with nothing completed sweeps the whole run"
        );
        assert_eq!(
            decide_sweep(RollbackScope::FailedItems, &failed_with(&saved_completed)),
            SweepDecision::WholeRunKeeping(vec![saved]),
            "the escalation keeps the document the run completed (SDK-1694 O5b)"
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
                decide_sweep(
                    scope,
                    &RunEnding::Failed {
                        completed: &BTreeSet::new()
                    }
                ),
                SweepDecision::WholeRun,
                "scope={scope:?}"
            );
        }
        assert_eq!(
            decide_sweep(
                RollbackScope::Nothing,
                &RunEnding::Failed {
                    completed: &BTreeSet::new()
                }
            ),
            SweepDecision::Nothing,
            "…except the escape hatch, which is the point of the escape hatch"
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
    /// `ERRORED` row. Without it a swept run would be left looking complete
    /// although its artifacts are gone.
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
            completed: BTreeSet::new(),
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
                Some(cognee_core::pipeline_run_registry::run_info_for_completed()),
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
            vec!["error"],
            "Python's ERRORED shape is the error alone (COG-6089)"
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
            completed: BTreeSet::new(),
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

    /// Since SDK-1694 O5b a `FailedItems` run with a failed file errors, and
    /// keeps the file it **completed** while sweeping the one that failed —
    /// Python's per-item rollback, which keeps the items its failed run
    /// stamped.
    ///
    /// Both halves of the path are pinned here. [`FailureReport::is_fatal`]
    /// turns the failed file into a `Failed` ending — with a survivor present
    /// and whatever the ratio threshold — and [`decide_sweep`] turns that
    /// ending into a sweep of the whole run except what it completed. The
    /// assertions are on the end state a caller actually inherits: the
    /// survivor's graph node, its ownership row and its completion marker
    /// stay, so the next run skips it; the failed file's go. (Before O5b the
    /// run swept the whole run, the survivor included.)
    #[tokio::test]
    async fn a_failed_run_keeps_the_files_it_completed() {
        use cognee_database::ops::graph_storage;
        use cognee_database::{GraphNode, RunScope};

        let conn = cognee_database::connect("sqlite::memory:")
            .await
            .expect("connect sqlite");
        cognee_database::initialize(&conn)
            .await
            .expect("initialize");
        let db = Arc::new(conn);
        let graph = Arc::new(cognee_test_utils::MockGraphDB::new());
        let vector = Arc::new(cognee_test_utils::MockVectorDB::new());
        let repo: Arc<dyn PipelineRunRepository> = Arc::clone(&db).pipeline_runs();

        let owner = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        cognee_database::ops::datasets::create_dataset(
            &db,
            cognee_models::Dataset::new("escalation".to_string(), owner, None, dataset_id),
        )
        .await
        .expect("create dataset");

        // One file the run finished and marked complete, one it failed on.
        let survivor = seed_data_item(&db, owner, "survivor.txt").await;
        let failed = seed_data_item(&db, owner, "failed.txt").await;
        let slug_of = |data_id: Uuid| Uuid::new_v5(&data_id, b"node");

        // The ownership row and the graph node each file's work produced.
        for data_id in [survivor, failed] {
            let slug = slug_of(data_id);
            graph_storage::upsert_nodes(
                &db,
                &[GraphNode {
                    id: Uuid::new_v4(),
                    slug,
                    user_id: owner,
                    data_id,
                    dataset_id,
                    pipeline_run_id: Some(run_id),
                    label: Some(format!("node-{slug}")),
                    node_type: "Entity".into(),
                    indexed_fields: serde_json::json!([]),
                    attributes: None,
                    created_at: chrono::Utc::now(),
                }],
            )
            .await
            .expect("seed the ownership row");
            graph
                .add_node_raw(serde_json::json!({ "id": slug.to_string(), "name": "n" }))
                .await
                .expect("seed the graph node");
        }

        // The survivor completed, so the writer saved and marked it as it went
        // (`CompletionLog::complete`). This is the state the escalation keeps.
        cognee_database::ProvenanceDb::mark_cognify_pipeline_status_complete(
            &*db, survivor, dataset_id,
        )
        .await
        .expect("mark the survivor complete");

        // Two of ten chunks failed, both of them the failed file's: a 0.2 ratio
        // against the default 0.05 threshold.
        let config = CognifyConfig::default().with_rollback_scope(RollbackScope::FailedItems);
        let policy = config.failure_policy();
        let mut report = FailureReport::with_policy(&policy);
        report.note_totals(2, 10);
        for _ in 0..2 {
            report.record(StageFailure {
                stage: FailureStage::GraphExtraction,
                data_id: failed,
                chunk_id: Some(Uuid::new_v4()),
                error: "boom".to_string(),
                fails_item: true,
            });
        }

        assert_eq!(
            report.total_items() - (report.failed_items().len() + report.unreached_items().len()),
            1,
            "a file survived, so the no-survivor backstop is not what makes this fatal"
        );
        assert!(
            report.is_fatal(&policy),
            "a file failed, so this FailedItems run failed"
        );
        let tolerant = FailurePolicy {
            chunk_failure_ratio_threshold: 1.0,
            ..policy
        };
        assert!(
            report.is_fatal(&tolerant),
            "whatever the ratio threshold: Python errors whenever an item failed"
        );

        let ctx = RunContext {
            database: db.clone(),
            graph_db: Arc::clone(&graph) as Arc<dyn GraphDBTrait>,
            vector_db: vector as Arc<dyn VectorDB>,
            repo,
            dataset_id,
            pipeline_run_id: Some(run_id),
            pipeline_id: Some(Uuid::new_v4()),
            pipeline_name: "cognify_pipeline",
            processed: vec![survivor, failed],
            completed: BTreeSet::from([survivor]),
            config: &config,
        };
        on_run_failed(
            &ctx,
            &CognifyError::Execute(report.to_string()),
            /* executor_recorded_the_error */ true,
        )
        .await;

        let scope = RunScope {
            pipeline_run_id: run_id,
            dataset_id,
            data_ids: None,
        };
        let rows: Vec<Uuid> = graph_storage::get_nodes_for_run(&db, &scope)
            .await
            .expect("read the ownership rows")
            .into_iter()
            .map(|row| row.data_id)
            .collect();
        assert_eq!(
            rows,
            vec![survivor],
            "the survivor's ownership row stays, the failed file's goes"
        );
        assert_eq!(
            graph.node_count(),
            1,
            "only the failed file's graph node is deleted"
        );
        assert!(
            graph
                .get_node(&slug_of(survivor).to_string())
                .await
                .expect("read the graph")
                .is_some(),
            "the node left is the survivor's"
        );
        assert_eq!(
            cognee_database::ProvenanceDb::get_cognify_completed_data_ids(
                &*db,
                dataset_id,
                &[survivor, failed]
            )
            .await
            .expect("read the completion markers"),
            BTreeSet::from([survivor]),
            "the survivor stays marked, so the next run redoes only the failed file"
        );
    }

    /// A `Data` row the completion-marker ops can find.
    async fn seed_data_item(
        db: &cognee_database::DatabaseConnection,
        owner: Uuid,
        name: &str,
    ) -> Uuid {
        let data_id = Uuid::new_v4();
        data::create_data(
            db,
            Data::builder(
                data_id,
                name,
                format!("/tmp/{name}"),
                format!("file://{name}"),
                "txt",
                "text/plain",
                "hash_placeholder",
                owner,
            )
            .build(),
        )
        .await
        .expect("create data");
        data_id
    }

    /// A started run is swept on its own run id.
    #[tokio::test]
    async fn a_started_run_is_swept_on_its_run_id() {
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
            pipeline_name: "cognify_pipeline",
            processed: Vec::new(),
            completed: BTreeSet::new(),
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
            completed: BTreeSet::new(),
            config: &config,
        };

        assert!(ctx.sweepable_run_id().is_none());
    }

    /// Everything a [`RunContext`] needs, over in-memory SQLite and mock
    /// stores.
    #[allow(clippy::type_complexity)]
    async fn run_context_fixture() -> (
        Arc<dyn RelationalDb>,
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
        let repo: Arc<dyn PipelineRunRepository> = Arc::clone(&db).pipeline_runs();
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
