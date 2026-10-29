//! Recovering the pipeline runs a killed process left in flight.
//!
//! Port of Python's `cognee/modules/cognify/recovery.py`
//! (`recover_stale_pipeline_runs_on_startup`).
//!
//! [`RunSweeper`] rolls back *one* run that the process running it watched
//! fail. A run killed by SIGKILL, an OOM kill or an Android process kill is
//! never watched failing by anybody: the code that would have swept it
//! (`cognee_cognify::rollback::on_run_failed`) dies with it. What survives is
//! a `pipeline_runs` row still at `STARTED`, whatever the run had already
//! written into the graph and vector stores, and — because a cognify
//! completion marker is written only on success — *no* marker for any of the
//! items it processed. The next run would extract the same entities again on
//! top of the ones the corpse left behind.
//!
//! # Proving a run abandoned
//!
//! A `STARTED` row is a *candidate*, not proof: a live run is in flight with
//! the same row. Recovery therefore requires all of:
//!
//! 1. the row is older than `min_age` (an eligibility floor, never the proof);
//! 2. nobody in this process holds the dataset's lock
//!    ([`DatasetLocks::try_lock`]), so no run of ours is working on it;
//! 3. the row's `run_info.recovery_lock` names an ownership marker on this
//!    filesystem whose OS lock can be taken without blocking
//!    ([`RunOwnership::claim`]). The OS drops that lock when its holder dies,
//!    so taking it proves the writer gone. A row with no marker (an older
//!    writer, a run started without a directory, another machine) is
//!    unverifiable and left untouched;
//! 4. the latest row of the run is still `STARTED` once the lock is held;
//! 5. the run's dataset still exists (otherwise the run is left as it is,
//!    marker included, as in Python).
//!
//! Then, in the order that matters: roll the run's artifacts back, and only
//! then close the run with an `ERRORED` row. A rollback that fails leaves the
//! run `STARTED`, so a later recovery retries it.
//!
//! No gate depends on the outcome: a run is never refused because a row says
//! `STARTED`, so an unrecovered row costs nothing but a stale status and the
//! dead run's partial artifacts.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cognee_core::pipeline_run_registry::recovery_lock_of;
use cognee_core::{DatasetLocks, RunOwnership};
use cognee_database::{DeleteDb, PipelineRunRepository, PipelineRunStatus, RelationalDb};
use cognee_graph::GraphDBTrait;
use cognee_vector::VectorDB;
use tracing::{debug, info, warn};

use crate::{RunSweeper, SweepScope};

/// The pipeline whose failed runs have artifacts to roll back. Any other
/// pipeline's abandoned run is only closed, as in Python's `ROLLBACK_HANDLERS`.
const COGNIFY_PIPELINE_NAME: &str = "cognify_pipeline";

/// The error recorded on a run recovery closes: Python's
/// `str(AbandonedPipelineRunError())`.
pub const ABANDONED_RUN_ERROR: &str = "AbandonedPipelineRunError: Pipeline run was abandoned: \
     its process stopped before the run finished. Startup recovery rolled the run back; re-run \
     the pipeline. (Status code: 500)";

/// Python's `STALE_RUN_MIN_AGE_SECONDS` default: one hour.
pub const DEFAULT_STALE_RUN_MIN_AGE: Duration = Duration::from_secs(3600);

/// What [`recover_stale_pipeline_runs`] did.
///
/// Reported rather than returned as an error because every step is
/// best-effort: refusing to start the SDK over a recovery convenience would be
/// worse than leaving a stale row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Runs the repository reported as still in flight.
    pub candidates: usize,
    /// Runs rolled back and closed as abandoned.
    pub recovered: usize,
    /// Runs younger than the age floor.
    pub skipped_young: usize,
    /// Runs whose dataset a run of this process holds the lock of.
    pub skipped_dataset_locked: usize,
    /// Runs whose ownership cannot be verified (no marker, or a live writer),
    /// or whose row names no dataset.
    pub skipped_unverifiable: usize,
    /// Runs whose latest row was already terminal once the lock was held.
    pub already_terminal: usize,
    /// Runs whose dataset row no longer exists. Left `STARTED` with their
    /// marker in place, as Python's recovery does (`recovery.py:108-111`).
    pub skipped_missing_dataset: usize,
    /// Runs the rollback or the close failed for; they stay `STARTED` and a
    /// later recovery retries them.
    pub failed: usize,
    pub graph_nodes_deleted: usize,
    pub vector_points_deleted: usize,
}

/// Roll back and close every pipeline run whose writer is provably gone.
///
/// Never fails; see the module docs for the conditions and their order.
/// `ownership_dir` is the directory run-ownership markers live in
/// (`CognifyConfig::run_ownership_dir`).
pub async fn recover_stale_pipeline_runs(
    repo: &dyn PipelineRunRepository,
    database: Arc<dyn RelationalDb>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    locks: &DatasetLocks,
    ownership_dir: &Path,
    min_age: Duration,
) -> RecoveryReport {
    let mut report = RecoveryReport::default();

    let candidates = match repo.list_unterminated_runs().await {
        Ok(candidates) => candidates,
        Err(e) => {
            warn!("startup recovery could not list the runs left in flight (non-fatal): {e}");
            return report;
        }
    };
    report.candidates = candidates.len();
    if candidates.is_empty() {
        return report;
    }

    let sweeper = RunSweeper::new(Arc::clone(&database), graph_db, vector_db);
    // An age too large to represent means nothing is old enough.
    let cutoff = chrono::Duration::from_std(min_age)
        .ok()
        .and_then(|age| chrono::Utc::now().checked_sub_signed(age))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);

    for candidate in candidates {
        if candidate.created_at > cutoff {
            debug!(
                pipeline_run_id = %candidate.pipeline_run_id,
                "skipping recovery: the run started less than {}s ago, treating it as live",
                min_age.as_secs()
            );
            report.skipped_young += 1;
            continue;
        }
        let Some(dataset_id) = candidate.dataset_id else {
            report.skipped_unverifiable += 1;
            continue;
        };

        let Some(_dataset_lock) = locks.try_lock(dataset_id) else {
            report.skipped_dataset_locked += 1;
            continue;
        };

        let Some(token) = recovery_lock_of(candidate.run_info.as_ref()) else {
            report.skipped_unverifiable += 1;
            continue;
        };
        let Some(mut ownership) = RunOwnership::claim(ownership_dir, token) else {
            debug!(
                pipeline_run_id = %candidate.pipeline_run_id,
                "run is active or its ownership cannot be verified; skipping recovery"
            );
            report.skipped_unverifiable += 1;
            continue;
        };

        // A writer or another recoverer may have finished since the candidate
        // was listed.
        let current = match repo.get_pipeline_run(candidate.pipeline_run_id).await {
            Ok(current) => current,
            Err(e) => {
                warn!(pipeline_run_id = %candidate.pipeline_run_id, "recovery failed, will retry: {e}");
                report.failed += 1;
                continue;
            }
        };
        let Some(current) = current.filter(|c| c.status == PipelineRunStatus::Started) else {
            ownership.mark_closed();
            report.already_terminal += 1;
            continue;
        };

        // Python reads the `Dataset` before rolling anything back and leaves
        // the run alone, marker included, when it is gone
        // (`recovery.py:108-111`).
        match DeleteDb::get_dataset_by_id(database.as_ref(), dataset_id).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                warn!(dataset_id = %dataset_id, "recovery dataset no longer exists");
                report.skipped_missing_dataset += 1;
                continue;
            }
            Err(e) => {
                warn!(pipeline_run_id = %current.pipeline_run_id, "recovery failed, will retry: {e}");
                report.failed += 1;
                continue;
            }
        }

        warn!(
            pipeline_run_id = %current.pipeline_run_id,
            dataset_id = %dataset_id,
            pipeline_name = %current.pipeline_name,
            started_at = %current.created_at,
            "recovering a pipeline run its process abandoned"
        );

        if current.pipeline_name == COGNIFY_PIPELINE_NAME {
            let outcome = sweeper
                .sweep_logging_failure(&SweepScope::whole_run(current.pipeline_run_id, dataset_id))
                .await;
            report.graph_nodes_deleted += outcome.graph_nodes_deleted;
            report.vector_points_deleted += outcome.vector_points_deleted;
            if !outcome.warnings.is_empty() {
                // Python lets the handler's exception abort this run's
                // recovery: the row stays STARTED and the next pass retries.
                report.failed += 1;
                continue;
            }
        }

        match repo
            .reset_orphan_run(
                dataset_id,
                &current.pipeline_name,
                current.id,
                ABANDONED_RUN_ERROR,
            )
            .await
        {
            // The latest row is no longer the one observed (a newer run, or
            // someone else closed it first): nothing left for us to close.
            Ok(false) => {
                ownership.mark_closed();
                report.already_terminal += 1;
            }
            Ok(true) => {
                ownership.mark_closed();
                report.recovered += 1;
                info!(pipeline_run_id = %current.pipeline_run_id, "recovery closed run as abandoned");
            }
            Err(e) => {
                warn!(pipeline_run_id = %current.pipeline_run_id, "recovery failed, will retry: {e}");
                report.failed += 1;
            }
        }
    }

    info!(
        candidates = report.candidates,
        recovered = report.recovered,
        skipped_young = report.skipped_young,
        skipped_dataset_locked = report.skipped_dataset_locked,
        skipped_unverifiable = report.skipped_unverifiable,
        already_terminal = report.already_terminal,
        skipped_missing_dataset = report.skipped_missing_dataset,
        failed = report.failed,
        "startup recovery of pipeline runs finished"
    );
    report
}
