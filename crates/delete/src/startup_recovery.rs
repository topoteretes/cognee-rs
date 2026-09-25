//! Rolling back the runs a killed process left in flight, at startup.
//!
//! [`RunSweeper`] rolls back *one* run that the process running it watched
//! fail. A run killed by SIGKILL, an OOM kill or an Android process kill is
//! never watched failing by anybody: the code that would have swept it
//! (`cognee_cognify::rollback::on_run_failed`) dies with it. What survives is
//! a `pipeline_runs` row still at `Initiated`/`Started`, whatever the run had
//! already written into the graph and vector stores, and — because a cognify
//! completion marker is written only on success — *no* marker for any of the
//! items it processed.
//!
//! So the next run re-processes every item and extracts the same entities
//! again, on top of the ones the corpse left behind. This module is the
//! missing half: given a repository, it asks which runs are still in flight
//! and rolls each one back through the very same [`RunSweeper`] the live
//! failure path uses, so recovery converges on the state a clean failure
//! would have left.
//!
//! Python does this too, and does it in the same order — `cognify_rollback_handler`
//! runs *before* the status reset in `modules/cognify/recovery.py`.
//!
//! # Only sound where one process owns the database
//!
//! "Still in flight" and "dead" are the same observation only when no peer
//! process could be running right now. Callers MUST gate this on
//! `Settings::resolved_single_process` / `COGNEE_SINGLE_PROCESS` — the same
//! assertion that gates
//! [`release_all_pipeline_run_claims`](cognee_database::PipelineRunRepository::release_all_pipeline_run_claims)
//! — and MUST call it before starting any run of their own. Without that,
//! this deletes a live peer's graph artifacts mid-run.
//!
//! # Ordering
//!
//! Sweep first, retire the status rows second. The status row is what makes
//! the dataset runnable again; retiring it first would open a window in which
//! a new run can start while the sweep is still deleting the dead run's
//! nodes.

use std::sync::Arc;

use cognee_database::{DatabaseConnection, PipelineRunRepository};
use cognee_graph::GraphDBTrait;
use cognee_vector::VectorDB;
use tracing::{info, warn};

use crate::{RunSweeper, SweepScope};

/// What [`sweep_orphaned_run_artifacts`] did.
///
/// Reported rather than returned as an error because every step is
/// best-effort: refusing to start the SDK over a recovery convenience would
/// turn a recoverable wedge into a hard startup failure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrphanArtifactSweep {
    /// Runs the repository reported as still in flight.
    pub runs_found: usize,
    /// Runs whose artifacts were swept without the sweeper reporting a
    /// problem.
    pub runs_swept: usize,
    /// Runs the sweep could not complete — the ledger rows survive, so a
    /// later recovery converges. Counted, not returned: the caller retires
    /// the status rows either way (see the note in the module docs).
    pub runs_failed: usize,
    /// Runs skipped because their `pipeline_runs` row carries no
    /// `dataset_id`, so no scope can be built for them. A run-scoped sweep
    /// needs both halves of the key; a dataset-less sweep would either select
    /// nothing or, if it ignored the dataset, reach outside the run.
    pub runs_without_dataset: usize,
    pub graph_nodes_deleted: usize,
    pub vector_points_deleted: usize,
    pub provenance_nodes_deleted: usize,
    pub provenance_edges_deleted: usize,
}

/// Roll back the graph/vector artifacts of every pipeline run the repository
/// still reports as in flight.
///
/// Call this **before**
/// [`reset_orphans`](cognee_database::PipelineRunRepository::reset_orphans),
/// and only under the single-process assertion — see the module docs for both
/// conditions.
///
/// Never fails. Each run is swept independently through
/// [`RunSweeper::sweep_logging_failure`], so one unreachable store or one
/// corrupt run does not abort recovery for the rest, and the caller still
/// retires every status row afterwards. That last point is the deliberate
/// half: a run whose sweep failed keeps its ownership-ledger rows, which are
/// the record of what still needs sweeping, but its status row is retired
/// anyway. Leaving the row would wedge the dataset *permanently* — that gate
/// never expires and this is the only path that clears it for an embedded
/// consumer — which is strictly worse than the duplicate entities a re-run
/// may now produce.
///
/// Scope is always [`SweepScope::whole_run`]: everything that one run created
/// in one dataset, and nothing else. Never a blanket delete — a dataset holds
/// artifacts from every run that ever touched it, and all but this one are
/// live.
pub async fn sweep_orphaned_run_artifacts(
    repo: &dyn PipelineRunRepository,
    database: Arc<DatabaseConnection>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
) -> OrphanArtifactSweep {
    let mut report = OrphanArtifactSweep::default();

    let orphans = match repo.list_orphan_runs().await {
        Ok(orphans) => orphans,
        Err(e) => {
            warn!(
                "startup rollback could not list the runs left in flight (non-fatal); a killed \
                 run's partial graph artifacts stay in the store and the next run will extract \
                 them again: {e}"
            );
            return report;
        }
    };

    report.runs_found = orphans.len();
    if orphans.is_empty() {
        return report;
    }

    let sweeper = RunSweeper::new(database, graph_db, vector_db);

    for orphan in orphans {
        let Some(dataset_id) = orphan.dataset_id else {
            report.runs_without_dataset += 1;
            warn!(
                pipeline_run_id = %orphan.pipeline_run_id,
                pipeline_name = %orphan.pipeline_name,
                "startup rollback skipped a run left in flight: its pipeline_runs row names no \
                 dataset, so there is no scope to sweep"
            );
            continue;
        };

        warn!(
            pipeline_run_id = %orphan.pipeline_run_id,
            dataset_id = %dataset_id,
            pipeline_name = %orphan.pipeline_name,
            started_at = %orphan.created_at,
            "rolling back a pipeline run a previous process left in flight"
        );

        let outcome = sweeper
            .sweep_logging_failure(&SweepScope::whole_run(orphan.pipeline_run_id, dataset_id))
            .await;

        report.graph_nodes_deleted += outcome.graph_nodes_deleted;
        report.vector_points_deleted += outcome.vector_points_deleted;
        report.provenance_nodes_deleted += outcome.provenance_nodes_deleted;
        report.provenance_edges_deleted += outcome.provenance_edges_deleted;

        if outcome.warnings.is_empty() {
            report.runs_swept += 1;
        } else {
            report.runs_failed += 1;
        }
    }

    info!(
        runs_found = report.runs_found,
        runs_swept = report.runs_swept,
        runs_failed = report.runs_failed,
        runs_without_dataset = report.runs_without_dataset,
        graph_nodes_deleted = report.graph_nodes_deleted,
        vector_points_deleted = report.vector_points_deleted,
        provenance_nodes_deleted = report.provenance_nodes_deleted,
        provenance_edges_deleted = report.provenance_edges_deleted,
        "startup rollback of runs left in flight by a previous process finished"
    );

    report
}
