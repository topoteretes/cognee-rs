//! Rolling back one pipeline run's contribution to a dataset.
//!
//! The dataset-delete path answers "what does this dataset own"; this answers
//! "what did this run create", through the same artifact-deletion helpers. The
//! sweep is *told* a scope and executes it — every decision about which scope
//! to hand it belongs to run orchestration, not here.

use std::collections::HashSet;
use std::sync::Arc;

use cognee_database::ops::{data, graph_storage};
use cognee_database::{DatabaseConnection, GraphEdge, RunScope};
use cognee_graph::GraphDBTrait;
use cognee_vector::VectorDB;
use serde::{Deserialize, Serialize};
use tracing::{Span, error, instrument, warn};
use uuid::Uuid;

use crate::{
    DeleteError, delete_graph_artifacts, delete_vector_artifacts, edge_retrieval_text,
    edge_type_vector_id,
};

/// Which ownership rows a single sweep is responsible for.
///
/// Owned rather than borrowing, so a caller can compute a scope, log it, and
/// hand it on. [`RunScope`] is the borrowed shape the ledger queries take;
/// `as_run_scope` bridges the two without copying.
///
/// `pipeline_run_id` is the run *correlation* id
/// (`pipeline_runs.pipeline_run_id`), not a `pipeline_runs` row key — that
/// table holds one row per status transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepScope {
    pub pipeline_run_id: Uuid,
    pub dataset_id: Uuid,
    /// `None` sweeps everything the run created in the dataset; `Some(ids)`
    /// narrows to those data items, and `Some(vec![])` sweeps nothing.
    pub data_ids: Option<Vec<Uuid>>,
}

impl SweepScope {
    /// Everything the run created in `dataset_id`.
    pub fn whole_run(pipeline_run_id: Uuid, dataset_id: Uuid) -> Self {
        Self {
            pipeline_run_id,
            dataset_id,
            data_ids: None,
        }
    }

    /// Only what the run created for `data_ids` in `dataset_id`. Artifacts a
    /// data item *outside* this list also produced are kept — that is what
    /// stops an item-scoped sweep deleting an entity a surviving file shares.
    pub fn for_data(pipeline_run_id: Uuid, dataset_id: Uuid, data_ids: Vec<Uuid>) -> Self {
        Self {
            pipeline_run_id,
            dataset_id,
            data_ids: Some(data_ids),
        }
    }

    fn as_run_scope(&self) -> RunScope<'_> {
        RunScope {
            pipeline_run_id: self.pipeline_run_id,
            dataset_id: self.dataset_id,
            data_ids: self.data_ids.as_deref(),
        }
    }
}

/// What a sweep removed. Counts are per store, because a slug kept by an
/// outside claimant still loses its ownership row: `provenance_nodes_deleted`
/// is routinely larger than `graph_nodes_deleted`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepOutcome {
    pub graph_nodes_deleted: usize,
    pub vector_points_deleted: usize,
    pub provenance_nodes_deleted: usize,
    pub provenance_edges_deleted: usize,
    /// Data items whose cognify completion marker the sweep cleared. Counts
    /// the items it *asked* to clear and got an acknowledgement for: clearing
    /// an item that carried no marker is a no-op, and under the intended
    /// ordering — markers are written after the sweep decision — that is the
    /// common case. An item whose clear failed is reported in `warnings`
    /// instead of counted here.
    pub data_items_unmarked: usize,
    pub warnings: Vec<String>,
}

/// Removes one pipeline run's contribution to one dataset.
///
/// The graph and vector stores are **required**, not optional. A sweep that
/// cannot reach them cannot restore the no-orphan-artifacts invariant, and
/// deleting the ownership rows regardless would manufacture exactly the orphan
/// this machinery exists to remove. Refusing to construct is the honest
/// failure; every real caller has both stores.
///
/// Takes the concrete [`DatabaseConnection`] rather than the crate's
/// [`DeleteDb`](crate::DeleteDb) abstraction: none of the run-scoped ledger
/// queries a sweep needs exist on that trait, and the ledger code here is
/// exercised against real in-memory SQLite rather than a mock, so there is
/// nothing for the indirection to buy.
pub struct RunSweeper {
    database: Arc<DatabaseConnection>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
}

impl RunSweeper {
    pub fn new(
        database: Arc<DatabaseConnection>,
        graph_db: Arc<dyn GraphDBTrait>,
        vector_db: Arc<dyn VectorDB>,
    ) -> Self {
        Self {
            database,
            graph_db,
            vector_db,
        }
    }

    /// Remove the run's artifacts, ownership rows and completion markers
    /// within `scope`.
    ///
    /// Idempotent: a second sweep of the same scope selects nothing, deletes
    /// nothing and returns a zeroed [`SweepOutcome`].
    ///
    /// The order — artifacts, then ownership rows, then markers — is the whole
    /// point. If artifact deletion fails partway this returns `Err` with the
    /// ownership rows untouched, so they survive as the record of what still
    /// needs sweeping and re-running converges. The reverse order loses that.
    /// Marker clearing is the one phase a re-run *cannot* redo — the rows that
    /// name the items are gone by then — so it is best-effort: every item is
    /// attempted and the failures are reported together.
    ///
    /// **Residual.** Artifacts are deleted only when the ledger shows nothing
    /// outside the scope claims them, under both identities an edge has (its
    /// slug and its `EdgeType` retrieval text). The one gap is an outside edge
    /// whose retrieval text lives only in its attributes and differs from its
    /// relationship name — see
    /// `graph_storage::get_relationship_names_claimed_outside_run`. Separately,
    /// an edge whose endpoints survive stays in the graph while its vectors go;
    /// that residual is Python's too.
    #[instrument(
        name = "cognee.rollback.sweep",
        level = "info",
        skip_all,
        fields(
            cognee.rollback.pipeline_run_id = %scope.pipeline_run_id,
            cognee.rollback.dataset_id = %scope.dataset_id,
            cognee.rollback.data_item_count = tracing::field::Empty,
        ),
        err,
    )]
    pub async fn sweep(&self, scope: &SweepScope) -> Result<SweepOutcome, DeleteError> {
        let mut outcome = SweepOutcome::default();
        self.run(scope, &mut outcome).await?;
        Ok(outcome)
    }

    /// [`RunSweeper::sweep`], but reporting its own failure instead of
    /// returning it.
    ///
    /// The sweep runs on an error path. Its failure must never replace the
    /// pipeline error that triggered it, so it is logged and folded into
    /// [`SweepOutcome::warnings`] and whatever the sweep did manage is
    /// returned.
    pub async fn sweep_logging_failure(&self, scope: &SweepScope) -> SweepOutcome {
        let mut outcome = SweepOutcome::default();
        if let Err(e) = self.run(scope, &mut outcome).await {
            error!(
                pipeline_run_id = %scope.pipeline_run_id,
                dataset_id = %scope.dataset_id,
                "Run sweep failed; the store may still hold this run's artifacts: {e}"
            );
            outcome.warnings.push(format!("Run sweep failed: {e}"));
        }
        outcome
    }

    async fn run(&self, scope: &SweepScope, outcome: &mut SweepOutcome) -> Result<(), DeleteError> {
        let run_scope = scope.as_run_scope();
        let run_id = scope.pipeline_run_id;

        // 1. The data items to unmark, read *before* step 4 deletes the rows
        //    that carry the answer. Derived from the ledger alone, never from
        //    the caller's narrowing list: an item this run wrote no row for was
        //    not rolled back here, and any marker it carries was written by an
        //    earlier run that did complete it — which the contract keeps.
        let affected = graph_storage::get_data_ids_for_run(&self.database, &run_scope)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to get provenance data ids for run {run_id}: {e}"
                ))
            })?;
        Span::current().record("cognee.rollback.data_item_count", affected.len() as i64);

        // 2. The rows whose slug no row outside the scope claims — another
        //    run, a pre-ownership NULL-run row, another dataset, or a data
        //    item this sweep is keeping. Only those artifacts may go.
        let nodes = graph_storage::get_unique_nodes_for_run(&self.database, &run_scope)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to get exclusively-owned provenance nodes for run {run_id}: {e}"
                ))
            })?;
        let edges = graph_storage::get_unique_edges_for_run(&self.database, &run_scope)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to get exclusively-owned provenance edges for run {run_id}: {e}"
                ))
            })?;

        // 2b. The same question for the edges' *other* identity. An
        //     `EdgeType_relationship_name` point is keyed on the edge's
        //     retrieval text, which is many-to-one over edges — a failed file's
        //     `is_a` edge and a surviving file's `is_a` edge resolve to one
        //     point — so slug exclusivity does not license deleting it. Only
        //     the texts no surviving row claims may go.
        let edge_type_ids = self.deletable_edge_type_ids(&run_scope, &edges).await?;

        // 3. Artifacts first. Graph *edges* are never deleted directly — they
        //    cascade with their endpoints, matching Python and the dataset
        //    delete path.
        let (graph_nodes, graph_warnings) =
            delete_graph_artifacts(Some(&self.graph_db), &nodes).await?;
        outcome.graph_nodes_deleted = graph_nodes;
        outcome.warnings.extend(graph_warnings);

        let (vector_points, vector_warnings) =
            delete_vector_artifacts(Some(&self.vector_db), &nodes, &edges, &edge_type_ids).await?;
        outcome.vector_points_deleted = vector_points;
        outcome.warnings.extend(vector_warnings);

        // 4. Ownership rows second — *all* of the selection, not just the
        //    exclusive subset: the row records that this run wrote the
        //    artifact, which stays true whether or not the artifact survived.
        outcome.provenance_edges_deleted =
            graph_storage::delete_edges_for_run(&self.database, &run_scope)
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!(
                        "Failed to delete provenance edges for run {run_id}: {e}"
                    ))
                })? as usize;
        outcome.provenance_nodes_deleted =
            graph_storage::delete_nodes_for_run(&self.database, &run_scope)
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!(
                        "Failed to delete provenance nodes for run {run_id}: {e}"
                    ))
                })? as usize;

        // 5. Completion markers last, for every item the run touched —
        //    exclusive or not. An item whose every artifact is shared still had
        //    its work rolled back, so it must not look complete afterwards.
        //
        //    Best-effort, unlike every phase above it: the ownership rows are
        //    already gone, so a re-run reads an empty `affected` and nothing
        //    will ever clear what this loop skips. Stopping at the first
        //    failure would strand every later item marked complete with its
        //    artifacts deleted — precisely the all-or-nothing violation the
        //    sweep exists to repair, and one that incremental loading then
        //    skips forever. So each item is attempted, and the failures are
        //    raised together at the end.
        let mut failures = 0usize;
        for data_id in &affected {
            match data::clear_cognify_pipeline_status_for_data(
                &self.database,
                *data_id,
                scope.dataset_id,
            )
            .await
            {
                Ok(()) => outcome.data_items_unmarked += 1,
                Err(e) => {
                    failures += 1;
                    let message = format!(
                        "Failed to clear the cognify completion marker for data {data_id}: {e}"
                    );
                    warn!("{message}");
                    outcome.warnings.push(message);
                }
            }
        }
        if failures > 0 {
            return Err(DeleteError::Runtime(format!(
                "Failed to clear {failures} of {} cognify completion markers for run {run_id}; \
                 those data items are still marked complete but their artifacts are gone",
                affected.len()
            )));
        }

        Ok(())
    }

    /// The `EdgeType` point ids `edges` may take with them: those whose
    /// retrieval text no surviving row still claims.
    ///
    /// The claimant query is skipped when there are no edges to filter: it is
    /// a scan, and for an empty scope it would honestly answer "everything is
    /// claimed".
    async fn deletable_edge_type_ids(
        &self,
        run_scope: &RunScope<'_>,
        edges: &[GraphEdge],
    ) -> Result<Vec<Uuid>, DeleteError> {
        if edges.is_empty() {
            return Ok(Vec::new());
        }

        let claimed: HashSet<String> =
            graph_storage::get_relationship_names_claimed_outside_run(&self.database, run_scope)
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!(
                        "Failed to get the relationship names claimed outside run {}: {e}",
                        run_scope.pipeline_run_id
                    ))
                })?
                .into_iter()
                .collect();

        Ok(edges
            .iter()
            .filter(|edge| !claimed.contains(&edge_retrieval_text(edge)))
            .filter_map(edge_type_vector_id)
            .collect())
    }
}
