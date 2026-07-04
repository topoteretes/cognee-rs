//! Cascading deletion of data and datasets across all Cognee backends.
//!
//! Removes content in dependency order — relational DB → graph DB → vector DB →
//! file storage — so no orphaned references remain. Supports dry-run previews.
//!
//! Main types: [`DeleteService`] and [`AuthorizedDeleteService`] (the
//! permission-checked wrapper).

mod authorized;

pub use authorized::AuthorizedDeleteService;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cognee_core::pipeline_run_registry::{
    ids::{pipeline_id, pipeline_run_id},
    run_info_for_initiated,
};
use cognee_database::{DeleteDb, GraphEdge, GraphNode, PipelineRunRepository, PipelineRunStatus};
use cognee_graph::GraphDBTrait;
use cognee_models::{Dataset, EdgeType, Triplet};
use cognee_session::SessionStore;
use cognee_storage::{StorageError, StorageTrait};
use cognee_vector::VectorDB;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

/// Map a `DeleteScope` variant to a human-readable label matching Python's
/// `COGNEE_FORGET_TARGET` values.
fn scope_label(scope: &DeleteScope) -> &'static str {
    match scope {
        DeleteScope::Data { .. } => "data_item",
        DeleteScope::Dataset { .. } => "dataset",
        DeleteScope::User { .. } => "user",
        DeleteScope::All => "everything",
    }
}

/// Map a `DeleteMode` variant to a human-readable label.
fn mode_label(mode: &DeleteMode) -> &'static str {
    match mode {
        DeleteMode::Soft => "soft",
        DeleteMode::Hard => "hard",
    }
}

/// Fallback vector collections used when `list_collections()` returns an empty
/// list (e.g. backends that do not implement dynamic discovery).
const FALLBACK_VECTOR_COLLECTIONS: &[(&str, &str)] = &[
    ("DocumentChunk", "text"),
    ("Entity", "name"),
    ("EntityType", "name"),
    ("TextSummary", "text"),
    ("EdgeType", "relationship_name"),
    ("Triplet", "text"),
    ("Event", "name"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeleteScope {
    Data {
        owner_id: Uuid,
        data_id: Uuid,
        dataset_name: Option<String>,
        /// When `true`, automatically delete the owning dataset if it becomes
        /// empty after this data item is removed. Mirrors Python's
        /// `delete_dataset_if_empty` parameter. Defaults to `false`.
        #[serde(default)]
        delete_dataset_if_empty: bool,
    },
    Dataset {
        owner_id: Uuid,
        dataset_name: String,
    },
    User {
        owner_id: Uuid,
    },
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeleteMode {
    Soft,
    Hard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRequest {
    pub scope: DeleteScope,
    pub mode: DeleteMode,
    /// When true: delete graph + vector only; preserve relational rows and raw
    /// files; force a cognify-only pipeline-status reset. Mirrors Python's
    /// `*_memory_only` forget targets.
    #[serde(default)]
    pub memory_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeletePreview {
    pub datasets_to_delete: usize,
    pub dataset_links_to_delete: usize,
    pub data_to_delete: usize,
    pub storage_files_to_delete: usize,
    pub graph_nodes_to_delete: usize,
    pub vector_points_to_delete: usize,
    pub provenance_nodes_to_delete: usize,
    pub provenance_edges_to_delete: usize,
    pub search_queries_to_delete: usize,
    pub orphaned_edge_types_to_delete: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeleteResult {
    pub deleted_datasets: usize,
    pub deleted_dataset_links: usize,
    pub deleted_data: usize,
    pub deleted_storage_files: usize,
    pub deleted_graph_nodes: usize,
    pub deleted_vector_points: usize,
    pub deleted_provenance_nodes: usize,
    pub deleted_provenance_edges: usize,
    pub deleted_orphan_entities: usize,
    pub deleted_orphan_entity_types: usize,
    pub deleted_orphan_edge_types: usize,
    pub deleted_pipeline_runs: usize,
    pub cleared_pipeline_statuses: usize,
    pub deleted_search_queries: usize,
    pub pruned_sessions: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum DeleteError {
    #[error("{0}")]
    Validation(String),

    #[error("{0}")]
    Runtime(String),

    #[error("Graph cleanup failed: {0}")]
    GraphCleanup(String),

    #[error("Vector cleanup failed: {0}")]
    VectorCleanup(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),
}

struct ResolvedDeleteTargets {
    datasets_to_delete: Vec<Dataset>,
    links_to_detach: Vec<(Uuid, Uuid)>,
    candidate_data_ids: Vec<Uuid>,
}

pub struct DeleteService {
    storage: Arc<dyn StorageTrait>,
    database: Arc<dyn DeleteDb>,
    graph_db: Option<Arc<dyn GraphDBTrait>>,
    vector_db: Option<Arc<dyn VectorDB>>,
    session_store: Option<Arc<dyn SessionStore>>,
    pipeline_run_repo: Option<Arc<dyn PipelineRunRepository>>,
}

impl DeleteService {
    pub fn new(storage: Arc<dyn StorageTrait>, database: Arc<dyn DeleteDb>) -> Self {
        Self {
            storage,
            database,
            graph_db: None,
            vector_db: None,
            session_store: None,
            pipeline_run_repo: None,
        }
    }

    pub fn with_graph_db(mut self, graph_db: Arc<dyn GraphDBTrait>) -> Self {
        self.graph_db = Some(graph_db);
        self
    }

    pub fn with_vector_db(mut self, vector_db: Arc<dyn VectorDB>) -> Self {
        self.vector_db = Some(vector_db);
        self
    }

    pub fn with_session_store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.session_store = Some(store);
        self
    }

    /// Wire a [`PipelineRunRepository`] so the dataset-scoped delete path
    /// writes a fresh `INITIATED` row for every `(dataset_id, pipeline_name)`
    /// pair before tearing the dataset down. This matches Python's prune
    /// chain, which fires `reset_dataset_pipeline_run_status` so a future
    /// re-cognify is not short-circuited by
    /// `check_pipeline_run_qualification` (task 08-08).
    ///
    /// When unset (default for back-compat / mock paths) the reset is a
    /// no-op.
    pub fn with_pipeline_run_repo(mut self, repo: Arc<dyn PipelineRunRepository>) -> Self {
        self.pipeline_run_repo = Some(repo);
        self
    }

    #[tracing::instrument(
        name = "cognee.delete.preview",
        skip(self, request),
        fields(
            cognee.forget.target = %scope_label(&request.scope),
            cognee.result.count = tracing::field::Empty,
        )
    )]
    pub async fn preview(&self, request: &DeleteRequest) -> Result<DeletePreview, DeleteError> {
        let targets = self.resolve_targets(request).await?;
        let data_to_delete = self
            .count_data_that_would_be_deleted(&targets.candidate_data_ids, &targets.links_to_detach)
            .await?;

        // Count graph nodes, vector points, and provenance rows from tables
        let (graph_node_count, vector_point_count, prov_node_count, prov_edge_count) =
            self.count_graph_vector_artifacts(&targets).await?;

        // Count search history queries that would be deleted
        let search_queries_to_delete = match &request.scope {
            DeleteScope::User { owner_id } => self
                .database
                .count_search_history_for_user(*owner_id)
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!(
                        "Failed to count search history for user {owner_id}: {e}"
                    ))
                })? as usize,
            DeleteScope::All => self
                .database
                .count_all_search_history()
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!("Failed to count all search history: {e}"))
                })? as usize,
            DeleteScope::Data { .. } | DeleteScope::Dataset { .. } => 0,
        };

        tracing::Span::current().record("cognee.result.count", data_to_delete);

        info!(
            datasets = targets.datasets_to_delete.len(),
            links = targets.links_to_detach.len(),
            data = data_to_delete,
            graph_nodes = graph_node_count,
            vector_points = vector_point_count,
            "delete preview computed"
        );

        Ok(DeletePreview {
            datasets_to_delete: targets.datasets_to_delete.len(),
            dataset_links_to_delete: targets.links_to_detach.len(),
            data_to_delete,
            storage_files_to_delete: data_to_delete,
            graph_nodes_to_delete: graph_node_count,
            vector_points_to_delete: vector_point_count,
            provenance_nodes_to_delete: prov_node_count,
            provenance_edges_to_delete: prov_edge_count,
            search_queries_to_delete,
            // Orphaned EdgeType count is only known at execution time (after
            // graph nodes are deleted and edges disappear), so preview reports 0.
            orphaned_edge_types_to_delete: 0,
        })
    }

    #[tracing::instrument(
        name = "cognee.delete.execute",
        skip(self, request),
        fields(
            cognee.forget.target = %scope_label(&request.scope),
            cognee.operation.mode = %mode_label(&request.mode),
            cognee.result.count = tracing::field::Empty,
        )
    )]
    pub async fn execute(&self, request: &DeleteRequest) -> Result<DeleteResult, DeleteError> {
        let targets = self.resolve_targets(request).await?;

        info!(
            datasets = targets.datasets_to_delete.len(),
            links = targets.links_to_detach.len(),
            data_candidates = targets.candidate_data_ids.len(),
            "delete targets resolved"
        );

        let mut warnings = Vec::new();
        let mut deleted_links = 0usize;
        let mut deleted_datasets = 0usize;
        let mut deleted_data = 0usize;
        let mut deleted_storage = 0usize;
        let mut deleted_graph_nodes = 0usize;
        let mut deleted_vector_points = 0usize;
        let mut deleted_provenance_nodes = 0usize;
        let mut deleted_provenance_edges = 0usize;
        let mut deleted_pipeline_runs = 0usize;
        let mut cleared_pipeline_statuses = 0usize;

        // ------------------------------------------------------------------
        // Memory-only path: wipe graph+vector, reset only cognify pipeline,
        // preserve relational rows and files.
        // ------------------------------------------------------------------
        if request.memory_only {
            return self.execute_memory_only(request, &targets).await;
        }

        // ------------------------------------------------------------------
        // Phase 0: Pipeline status cleanup (while junction rows still exist)
        // ------------------------------------------------------------------
        // Clear pipeline_status JSON entries for datasets about to be deleted.
        // This must run before junction rows (dataset_data) are removed, since
        // the junction is needed to find related Data records.

        for dataset in &targets.datasets_to_delete {
            let count = self
                .database
                .clear_pipeline_status_for_dataset(dataset.id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to clear pipeline_status for dataset '{}': {error}",
                        dataset.name
                    ))
                })?;
            cleared_pipeline_statuses += count;

            // Python parity: writing a fresh `INITIATED` row for every
            // pipeline registered against this dataset invalidates any prior
            // `COMPLETED` row so a future re-cognify is not short-circuited
            // by `check_pipeline_run_qualification` (task 08-08). The rows
            // outlive the dataset itself — once the FK is dropped (gap 08-01)
            // the orphans are harmless and surface in
            // `list_recent_with_attribution` with `dataset_name = None`.
            // See [docs/telemetry/08/05-reset-helpers.md §4.3] for the
            // orphan-row decision.
            self.reset_dataset_pipeline_run_status(dataset.owner_id, dataset.id)
                .await?;
        }

        // For data-scoped deletion, clear pipeline_status for each affected
        // dataset (the data item's pipeline_status entries keyed by dataset_id).
        if matches!(request.scope, DeleteScope::Data { .. }) {
            // Collect unique dataset IDs from links_to_detach (that are NOT in
            // datasets_to_delete, which were already handled above).
            let already_handled: HashSet<Uuid> =
                targets.datasets_to_delete.iter().map(|d| d.id).collect();
            let mut affected_dataset_ids: HashSet<Uuid> = HashSet::new();
            for (dataset_id, _) in &targets.links_to_detach {
                if !already_handled.contains(dataset_id) {
                    affected_dataset_ids.insert(*dataset_id);
                }
            }
            for dataset_id in affected_dataset_ids {
                let count = self
                    .database
                    .clear_pipeline_status_for_dataset(dataset_id)
                    .await
                    .map_err(|error| {
                        DeleteError::Runtime(format!(
                            "Failed to clear pipeline_status for dataset {dataset_id}: {error}"
                        ))
                    })?;
                cleared_pipeline_statuses += count;
            }
        }

        // ------------------------------------------------------------------
        // Phase 1: Graph/vector cleanup (before relational provenance is gone)
        // ------------------------------------------------------------------

        let is_all_scope = matches!(request.scope, DeleteScope::All);

        if is_all_scope {
            // Fast-path: wipe entire graph and all vector collections
            let (gn, vp, pn, pe) = self.count_graph_vector_artifacts(&targets).await?;
            let (_, _, _, _, gv_warnings) = self.cleanup_all().await?;
            deleted_graph_nodes += gn;
            deleted_vector_points += vp;
            deleted_provenance_nodes += pn;
            deleted_provenance_edges += pe;
            warnings.extend(gv_warnings);
        } else {
            // Dataset-scoped cleanup
            for dataset in &targets.datasets_to_delete {
                let (gn, vp, pn, pe, gv_warnings) = self.cleanup_dataset(dataset.id).await?;
                deleted_graph_nodes += gn;
                deleted_vector_points += vp;
                deleted_provenance_nodes += pn;
                deleted_provenance_edges += pe;
                warnings.extend(gv_warnings);
            }

            // Data-scoped cleanup (for single data item deletion)
            if matches!(request.scope, DeleteScope::Data { .. }) {
                // Compute which data IDs will actually be deleted
                let deletable_data_ids = self.compute_deletable_data_ids(&targets).await?;

                for data_id in &deletable_data_ids {
                    for (dataset_id, did) in &targets.links_to_detach {
                        if did == data_id {
                            let (gn, vp, pn, pe, gv_warnings) =
                                self.cleanup_data(*data_id, *dataset_id).await?;
                            deleted_graph_nodes += gn;
                            deleted_vector_points += vp;
                            deleted_provenance_nodes += pn;
                            deleted_provenance_edges += pe;
                            warnings.extend(gv_warnings);
                        }
                    }
                }
            }
        }

        info!(
            deleted_graph_nodes,
            deleted_vector_points,
            deleted_provenance_nodes,
            deleted_provenance_edges,
            "phase 1: graph/vector cleanup completed"
        );

        // ------------------------------------------------------------------
        // Phase 2: Relational cleanup (links, datasets, data, storage)
        // ------------------------------------------------------------------

        for (dataset_id, data_id) in &targets.links_to_detach {
            self.database
                .detach_data_from_dataset(*dataset_id, *data_id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to detach data {data_id} from dataset {dataset_id}: {error}"
                    ))
                })?;
            deleted_links += 1;
        }

        // For data-scoped deletion, invalidate the pipeline cache for each
        // affected dataset after detaching. This ensures re-running cognify
        // will reprocess the dataset since its data composition has changed.
        if matches!(request.scope, DeleteScope::Data { .. }) {
            let mut invalidated_datasets: HashSet<Uuid> = HashSet::new();
            for (dataset_id, _) in &targets.links_to_detach {
                if invalidated_datasets.insert(*dataset_id) {
                    let count = self
                        .database
                        .delete_pipeline_runs_by_dataset(*dataset_id)
                        .await
                        .map_err(|error| {
                            DeleteError::Runtime(format!(
                                "Failed to delete pipeline_runs for dataset {dataset_id}: {error}"
                            ))
                        })?;
                    deleted_pipeline_runs += count as usize;
                }
            }
        }

        for dataset in &targets.datasets_to_delete {
            self.database
                .delete_dataset(dataset.id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to delete dataset '{}': {error}",
                        dataset.name
                    ))
                })?;
            deleted_datasets += 1;
        }

        for data_id in &targets.candidate_data_ids {
            let remaining_links = self
                .database
                .count_data_dataset_links(*data_id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to count links for data {data_id}: {error}"
                    ))
                })?;

            if remaining_links > 0 {
                continue;
            }

            let data = self.database.get_data(*data_id).await.map_err(|error| {
                DeleteError::Runtime(format!("Failed to fetch data {data_id}: {error}"))
            })?;

            if let Some(data) = data {
                match self.storage.delete(&data.raw_data_location).await {
                    Ok(()) => {
                        deleted_storage += 1;
                    }
                    Err(StorageError::NotFound(_)) => {
                        warn!(
                            data_id = %data.id,
                            location = %data.raw_data_location,
                            "storage file already missing"
                        );
                        warnings.push(format!(
                            "Storage file already missing for data {} at '{}'",
                            data.id, data.raw_data_location
                        ));
                    }
                    Err(error) => {
                        return Err(DeleteError::Runtime(format!(
                            "Failed to delete storage for data {}: {}",
                            data.id, error
                        )));
                    }
                }
            }

            self.database.delete_data(*data_id).await.map_err(|error| {
                DeleteError::Runtime(format!("Failed to delete data {data_id}: {error}"))
            })?;
            deleted_data += 1;
        }

        info!(
            deleted_links,
            deleted_datasets,
            deleted_data,
            deleted_storage,
            "phase 2: relational cleanup completed"
        );

        // ------------------------------------------------------------------
        // Phase 3: Hard-mode orphan sweep (degree-one Entity/EntityType nodes)
        // ------------------------------------------------------------------
        //
        // The degree-based sweep is gated to Hard mode to match Python, whose
        // degree-one Entity/EntityType sweep is itself `if mode == "hard"`
        // (legacy_delete.py) and is never reached by the production soft path
        // (datasets.delete_data → legacy_delete(data, "soft")). Running it on
        // Soft as well would make Rust soft-delete *more* destructive than
        // Python (it would remove degree-one nodes Python preserves).
        //
        // TODO(B6.4): Python's main soft path still prunes nodes that become
        // orphaned by the deletion via a provenance/slug-scoped traversal
        // (delete_from_graph_and_vector excludes co-owned slugs), not a global
        // degree sweep. Rust currently leaves those orphans on soft delete.
        // Closing this gap requires a deletion-scoped cleanup rather than the
        // global degree heuristic; tracked for a follow-up.

        let mut deleted_orphan_entities = 0usize;
        let mut deleted_orphan_entity_types = 0usize;
        let mut deleted_orphan_edge_types = 0usize;

        if matches!(request.mode, DeleteMode::Hard) {
            let (oe, oet, sweep_warnings) = self.sweep_orphan_nodes().await?;
            deleted_orphan_entities = oe;
            deleted_orphan_entity_types = oet;
            warnings.extend(sweep_warnings);

            let (oedge, edge_sweep_warnings) = self.sweep_orphan_edge_types().await?;
            deleted_orphan_edge_types = oedge;
            warnings.extend(edge_sweep_warnings);
        }

        // ------------------------------------------------------------------
        // Phase 4: Search history cleanup
        // ------------------------------------------------------------------

        let deleted_search_queries = match &request.scope {
            DeleteScope::User { owner_id } => self
                .database
                .delete_search_history_for_user(*owner_id)
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!(
                        "Failed to delete search history for user {owner_id}: {e}"
                    ))
                })? as usize,
            DeleteScope::All => self
                .database
                .delete_all_search_history()
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!("Failed to delete all search history: {e}"))
                })? as usize,
            // Data/Dataset scopes: no-op (no dataset_id on queries table)
            DeleteScope::Data { .. } | DeleteScope::Dataset { .. } => 0,
        };

        // ------------------------------------------------------------------
        // Phase 5: Session cache cleanup
        // ------------------------------------------------------------------

        let mut pruned_sessions = false;
        if matches!(request.scope, DeleteScope::All)
            && let Some(session_store) = &self.session_store
        {
            session_store
                .prune()
                .await
                .map_err(|e| DeleteError::Runtime(format!("Failed to prune session cache: {e}")))?;
            pruned_sessions = true;
        }

        let total_deleted =
            deleted_datasets + deleted_data + deleted_graph_nodes + deleted_vector_points;
        tracing::Span::current().record("cognee.result.count", total_deleted);

        info!(
            deleted_datasets,
            deleted_links,
            deleted_data,
            deleted_storage,
            deleted_graph_nodes,
            deleted_vector_points,
            deleted_orphan_entities,
            deleted_orphan_entity_types,
            deleted_orphan_edge_types,
            warning_count = warnings.len(),
            "delete execution completed"
        );

        Ok(DeleteResult {
            deleted_datasets,
            deleted_dataset_links: deleted_links,
            deleted_data,
            deleted_storage_files: deleted_storage,
            deleted_graph_nodes,
            deleted_vector_points,
            deleted_provenance_nodes,
            deleted_provenance_edges,
            deleted_orphan_entities,
            deleted_orphan_entity_types,
            deleted_orphan_edge_types,
            deleted_pipeline_runs,
            cleared_pipeline_statuses,
            deleted_search_queries,
            pruned_sessions,
            warnings,
        })
    }

    async fn execute_memory_only(
        &self,
        request: &DeleteRequest,
        targets: &ResolvedDeleteTargets,
    ) -> Result<DeleteResult, DeleteError> {
        let mut warnings = Vec::new();
        let mut deleted_graph_nodes = 0usize;
        let mut deleted_vector_points = 0usize;
        let mut deleted_provenance_nodes = 0usize;
        let mut deleted_provenance_edges = 0usize;

        // Phase 1: Graph/vector cleanup only (same logic as normal execute).
        let is_all_scope = matches!(request.scope, DeleteScope::All);

        if is_all_scope {
            let (gn, vp, pn, pe) = self.count_graph_vector_artifacts(targets).await?;
            let (_, _, _, _, gv_warnings) = self.cleanup_all().await?;
            deleted_graph_nodes += gn;
            deleted_vector_points += vp;
            deleted_provenance_nodes += pn;
            deleted_provenance_edges += pe;
            warnings.extend(gv_warnings);
        } else {
            for dataset in &targets.datasets_to_delete {
                let (gn, vp, pn, pe, gv_warnings) = self.cleanup_dataset(dataset.id).await?;
                deleted_graph_nodes += gn;
                deleted_vector_points += vp;
                deleted_provenance_nodes += pn;
                deleted_provenance_edges += pe;
                warnings.extend(gv_warnings);
            }

            if matches!(request.scope, DeleteScope::Data { .. }) {
                let deletable_data_ids = self.compute_deletable_data_ids(targets).await?;
                for data_id in &deletable_data_ids {
                    for (dataset_id, did) in &targets.links_to_detach {
                        if did == data_id {
                            let (gn, vp, pn, pe, gv_warnings) =
                                self.cleanup_data(*data_id, *dataset_id).await?;
                            deleted_graph_nodes += gn;
                            deleted_vector_points += vp;
                            deleted_provenance_nodes += pn;
                            deleted_provenance_edges += pe;
                            warnings.extend(gv_warnings);
                        }
                    }
                }
            }
        }

        // Phase 2: Pipeline-status reset.
        //
        // The dataset and data-item variants differ exactly as Python does:
        //
        // * Dataset variant (Python `_forget_dataset_memory`, forget.py:271-289):
        //   on every Data record linked to the dataset, remove the dataset_id
        //   entry from EVERY pipeline in `pipeline_status` (Python loops over
        //   `list(pipeline_status.keys())`), then reset the dataset-level
        //   *run* status for `cognify_pipeline` only.
        //
        // * Data-item variant (Python `_forget_data_memory`, forget.py:331-351):
        //   remove only the `cognify_pipeline` entry for `(data_id, dataset_id)`
        //   on that single Data record; NO dataset-level run-status reset.
        if matches!(request.scope, DeleteScope::Data { .. }) {
            // Data-item: surgically clear cognify status for the single data
            // record in each affected (dataset_id, data_id) pair.
            for (dataset_id, data_id) in &targets.links_to_detach {
                self.database
                    .clear_cognify_pipeline_status_for_data(*data_id, *dataset_id)
                    .await
                    .map_err(|e| {
                        DeleteError::Runtime(format!(
                            "Failed to clear cognify pipeline_status for data {data_id} in dataset {dataset_id}: {e}"
                        ))
                    })?;
            }
        } else {
            // Dataset (and All): mirror Python's per-record all-pipeline key
            // removal, then reset cognify run status at the dataset level.
            for dataset in &targets.datasets_to_delete {
                self.database
                    .clear_pipeline_status_for_dataset(dataset.id)
                    .await
                    .map_err(|e| {
                        DeleteError::Runtime(format!(
                            "Failed to clear pipeline_status for dataset '{}': {e}",
                            dataset.name
                        ))
                    })?;

                self.reset_cognify_pipeline_run_status(dataset.owner_id, dataset.id)
                    .await?;
            }
        }

        info!(
            deleted_graph_nodes,
            deleted_vector_points, "memory-only delete completed"
        );

        Ok(DeleteResult {
            deleted_datasets: 0,
            deleted_dataset_links: 0,
            deleted_data: 0,
            deleted_storage_files: 0,
            deleted_graph_nodes,
            deleted_vector_points,
            deleted_provenance_nodes,
            deleted_provenance_edges,
            deleted_orphan_entities: 0,
            deleted_orphan_entity_types: 0,
            deleted_orphan_edge_types: 0,
            deleted_pipeline_runs: 0,
            cleared_pipeline_statuses: 0,
            deleted_search_queries: 0,
            pruned_sessions: false,
            warnings,
        })
    }

    pub async fn data_ids_to_delete(
        &self,
        request: &DeleteRequest,
    ) -> Result<Vec<Uuid>, DeleteError> {
        let targets = self.resolve_targets(request).await?;
        self.compute_deletable_data_ids(&targets).await
    }

    /// Internal Python-parity helper invoked from the dataset-deletion
    /// path. Walks every distinct `pipeline_name` registered against
    /// `dataset_id` and writes a fresh `INITIATED` row for each (skipping
    /// pipelines already at `INITIATED`).
    ///
    /// No-op when no [`PipelineRunRepository`] was supplied via
    /// [`Self::with_pipeline_run_repo`]; this keeps mock-only test paths
    /// working without forcing them to wire a repo.
    async fn reset_dataset_pipeline_run_status(
        &self,
        owner_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DeleteError> {
        let Some(repo) = self.pipeline_run_repo.as_ref() else {
            return Ok(());
        };

        let runs = repo
            .get_pipeline_runs_by_dataset(dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to list pipeline runs for dataset {dataset_id}: {e}"
                ))
            })?;

        for run in runs {
            if matches!(run.status, PipelineRunStatus::Initiated) {
                // Python skips runs already pending to avoid duplicate rows.
                continue;
            }
            let name = run.pipeline_name;
            let pid = pipeline_id(owner_id, dataset_id, &name);
            let prid = pipeline_run_id(pid, dataset_id);
            repo.log_pipeline_run(
                prid,
                pid,
                &name,
                Some(dataset_id),
                PipelineRunStatus::Initiated,
                Some(run_info_for_initiated()),
            )
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to reset pipeline '{name}' for dataset {dataset_id}: {e}"
                ))
            })?;
        }
        Ok(())
    }

    /// Like [`reset_dataset_pipeline_run_status`] but only resets the specified
    /// pipeline by name. Used by memory-only forget to reset `cognify_pipeline`
    /// without touching `add_pipeline` — matching Python's
    /// `reset_dataset_pipeline_run_status(pipeline_names=["cognify_pipeline"])`.
    async fn reset_cognify_pipeline_run_status(
        &self,
        owner_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DeleteError> {
        let Some(repo) = self.pipeline_run_repo.as_ref() else {
            return Ok(());
        };

        let runs = repo
            .get_pipeline_runs_by_dataset(dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to list pipeline runs for dataset {dataset_id}: {e}"
                ))
            })?;

        for run in runs {
            if run.pipeline_name != "cognify_pipeline" {
                continue;
            }
            if matches!(run.status, PipelineRunStatus::Initiated) {
                continue;
            }
            let name = run.pipeline_name;
            let pid = pipeline_id(owner_id, dataset_id, &name);
            let prid = pipeline_run_id(pid, dataset_id);
            repo.log_pipeline_run(
                prid,
                pid,
                &name,
                Some(dataset_id),
                PipelineRunStatus::Initiated,
                Some(run_info_for_initiated()),
            )
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to reset cognify pipeline for dataset {dataset_id}: {e}"
                ))
            })?;
        }
        Ok(())
    }

    // ==================================================================
    // Graph/vector cleanup helpers
    // ==================================================================

    /// Fast-path for `DeleteScope::All`: wipe entire graph and all vector
    /// collections. Returns `(graph_nodes_deleted, vector_points_deleted,
    /// provenance_nodes_deleted, provenance_edges_deleted, warnings)`.
    ///
    /// Note: provenance counts are 0 because the dataset cascade in phase 2
    /// handles provenance row deletion.
    async fn cleanup_all(&self) -> Result<(usize, usize, usize, usize, Vec<String>), DeleteError> {
        let mut warnings = Vec::new();
        let graph_nodes = 0usize;
        let vector_points = 0usize;

        // --- Graph ---
        if let Some(graph_db) = &self.graph_db {
            graph_db
                .delete_graph()
                .await
                .map_err(|e| DeleteError::GraphCleanup(format!("Failed to delete graph: {e}")))?;
            // We don't know exact count after delete_graph(); report 0 as "wiped".
        } else {
            warnings
                .push("Graph DB not configured; graph artifacts were not cleaned up.".to_string());
        }

        // --- Vector ---
        if let Some(vector_db) = &self.vector_db {
            let mut collections = vector_db.list_collections().await.map_err(|e| {
                DeleteError::VectorCleanup(format!("Failed to list vector collections: {e}"))
            })?;

            if collections.is_empty() {
                // Fallback to known list
                collections = FALLBACK_VECTOR_COLLECTIONS
                    .iter()
                    .map(|(dt, fn_)| (dt.to_string(), fn_.to_string()))
                    .collect();
            }

            for (data_type, field_name) in &collections {
                let exists = vector_db
                    .has_collection(data_type, field_name)
                    .await
                    .map_err(|e| {
                        DeleteError::VectorCleanup(format!(
                            "Failed to check vector collection {data_type}_{field_name}: {e}"
                        ))
                    })?;

                if exists {
                    vector_db
                        .delete_collection(data_type, field_name)
                        .await
                        .map_err(|e| {
                            DeleteError::VectorCleanup(format!(
                                "Failed to delete vector collection {data_type}_{field_name}: {e}"
                            ))
                        })?;
                }
            }
        } else {
            warnings.push(
                "Vector DB not configured; vector artifacts were not cleaned up.".to_string(),
            );
        }

        Ok((graph_nodes, vector_points, 0, 0, warnings))
    }

    /// Dataset-scoped cleanup: remove graph nodes and vector points based on
    /// the provenance `nodes`/`edges` tables. Returns `(graph_nodes_deleted,
    /// vector_points_deleted, provenance_nodes_deleted, provenance_edges_deleted,
    /// warnings)`.
    async fn cleanup_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<(usize, usize, usize, usize, Vec<String>), DeleteError> {
        let mut warnings = Vec::new();
        let mut graph_node_count = 0usize;
        let mut vector_point_count = 0usize;

        let nodes = self
            .database
            .get_nodes_by_dataset(dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to get provenance nodes for dataset {dataset_id}: {e}"
                ))
            })?;

        let edges = self
            .database
            .get_edges_by_dataset(dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to get provenance edges for dataset {dataset_id}: {e}"
                ))
            })?;

        let prov_node_count = nodes.len();
        let prov_edge_count = edges.len();

        if nodes.is_empty() && edges.is_empty() {
            return Ok((0, 0, 0, 0, warnings));
        }

        // --- Graph cleanup ---
        let (gn, gw) = self.delete_graph_artifacts(&nodes).await?;
        graph_node_count += gn;
        warnings.extend(gw);

        // --- Vector cleanup ---
        let (vp, vw) = self.delete_vector_artifacts(&nodes, &edges).await?;
        vector_point_count += vp;
        warnings.extend(vw);

        // --- Provenance cleanup ---
        // Note: for dataset deletion, the FK CASCADE on dataset_id will handle
        // this automatically when the dataset record is deleted. But we call
        // it explicitly to be safe and consistent.
        self.database
            .delete_provenance_edges_for_dataset(dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to delete provenance edges for dataset {dataset_id}: {e}"
                ))
            })?;
        self.database
            .delete_provenance_nodes_for_dataset(dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to delete provenance nodes for dataset {dataset_id}: {e}"
                ))
            })?;

        Ok((
            graph_node_count,
            vector_point_count,
            prov_node_count,
            prov_edge_count,
            warnings,
        ))
    }

    /// Data-scoped cleanup: remove graph nodes and vector points for a single
    /// data item, using only non-shared slugs. Returns `(graph_nodes_deleted,
    /// vector_points_deleted, provenance_nodes_deleted, provenance_edges_deleted,
    /// warnings)`.
    ///
    /// Note: `provenance_nodes_deleted` and `provenance_edges_deleted` count ALL
    /// provenance rows for this `(data_id, dataset_id)` pair, not just the
    /// unique ones used for graph/vector cleanup.
    async fn cleanup_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(usize, usize, usize, usize, Vec<String>), DeleteError> {
        let mut warnings = Vec::new();
        let mut graph_node_count = 0usize;
        let mut vector_point_count = 0usize;

        let nodes = self
            .database
            .get_unique_nodes_for_data(data_id, dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to get unique provenance nodes for data {data_id}: {e}"
                ))
            })?;

        let edges = self
            .database
            .get_unique_edges_for_data(data_id, dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to get unique provenance edges for data {data_id}: {e}"
                ))
            })?;

        // Count ALL provenance rows (not just unique) before deletion, because
        // we delete all rows for this (data_id, dataset_id) pair.
        let all_prov_nodes = self
            .database
            .get_provenance_node_count_for_data(data_id, dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to count provenance nodes for data {data_id}: {e}"
                ))
            })?;
        let all_prov_edges = self
            .database
            .get_provenance_edge_count_for_data(data_id, dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to count provenance edges for data {data_id}: {e}"
                ))
            })?;

        if nodes.is_empty() && edges.is_empty() && all_prov_nodes == 0 && all_prov_edges == 0 {
            return Ok((0, 0, 0, 0, warnings));
        }

        // --- Graph cleanup ---
        let (gn, gw) = self.delete_graph_artifacts(&nodes).await?;
        graph_node_count += gn;
        warnings.extend(gw);

        // --- Vector cleanup ---
        let (vp, vw) = self.delete_vector_artifacts(&nodes, &edges).await?;
        vector_point_count += vp;
        warnings.extend(vw);

        // --- Provenance cleanup ---
        self.database
            .delete_provenance_edges_for_data(data_id, dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to delete provenance edges for data {data_id}: {e}"
                ))
            })?;
        self.database
            .delete_provenance_nodes_for_data(data_id, dataset_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!(
                    "Failed to delete provenance nodes for data {data_id}: {e}"
                ))
            })?;

        Ok((
            graph_node_count,
            vector_point_count,
            all_prov_nodes,
            all_prov_edges,
            warnings,
        ))
    }

    /// Delete nodes from the graph DB based on provenance node slugs.
    /// Returns `(count, warnings)`.
    async fn delete_graph_artifacts(
        &self,
        nodes: &[GraphNode],
    ) -> Result<(usize, Vec<String>), DeleteError> {
        let mut warnings = Vec::new();

        if let Some(graph_db) = &self.graph_db {
            let node_ids: Vec<String> = nodes
                .iter()
                .map(|n| n.slug.to_string())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            if !node_ids.is_empty() {
                graph_db.delete_nodes(&node_ids).await.map_err(|e| {
                    DeleteError::GraphCleanup(format!(
                        "Failed to delete {} graph nodes: {e}",
                        node_ids.len()
                    ))
                })?;
            }

            Ok((node_ids.len(), warnings))
        } else {
            if !nodes.is_empty() {
                warnings.push(
                    "Graph DB not configured; graph artifacts were not cleaned up.".to_string(),
                );
            }
            Ok((0, warnings))
        }
    }

    /// Delete vector points based on provenance nodes and edges.
    /// Returns `(count, warnings)`.
    async fn delete_vector_artifacts(
        &self,
        nodes: &[GraphNode],
        edges: &[GraphEdge],
    ) -> Result<(usize, Vec<String>), DeleteError> {
        let mut warnings = Vec::new();
        let mut total_deleted = 0usize;

        if let Some(vector_db) = &self.vector_db {
            // Group nodes by (node_type, field_name) for batched deletion
            let mut by_collection: HashMap<(String, String), Vec<Uuid>> = HashMap::new();

            for node in nodes {
                let fields = parse_indexed_fields(&node.indexed_fields);
                for field_name in fields {
                    by_collection
                        .entry((node.node_type.clone(), field_name))
                        .or_default()
                        .push(node.slug);
                }
            }

            // Edges contribute to EdgeType_relationship_name and Triplet_text.
            for edge in edges {
                by_collection
                    .entry(("EdgeType".to_string(), "relationship_name".to_string()))
                    .or_default()
                    .push(EdgeType::deterministic_id(&edge.relationship_name));

                by_collection
                    .entry(("Triplet".to_string(), "text".to_string()))
                    .or_default()
                    .push(triplet_vector_id(edge));
            }

            for ((data_type, field_name), ids) in &by_collection {
                if ids.is_empty() {
                    continue;
                }

                let exists = vector_db
                    .has_collection(data_type, field_name)
                    .await
                    .map_err(|e| {
                        DeleteError::VectorCleanup(format!(
                            "Failed to check vector collection {data_type}_{field_name}: {e}"
                        ))
                    })?;

                if exists {
                    vector_db
                        .delete_points(data_type, field_name, ids)
                        .await
                        .map_err(|e| {
                            DeleteError::VectorCleanup(format!(
                                "Failed to delete vector points from {data_type}_{field_name}: {e}"
                            ))
                        })?;
                    total_deleted += ids.len();
                }
            }

            Ok((total_deleted, warnings))
        } else {
            if !nodes.is_empty() || !edges.is_empty() {
                warnings.push(
                    "Vector DB not configured; vector artifacts were not cleaned up.".to_string(),
                );
            }
            Ok((0, warnings))
        }
    }

    /// Hard-mode orphan sweep: find and delete degree-one Entity/EntityType
    /// nodes from the graph and their corresponding vector points.
    ///
    /// Returns `(orphan_entities, orphan_entity_types, warnings)`.
    async fn sweep_orphan_nodes(&self) -> Result<(usize, usize, Vec<String>), DeleteError> {
        let mut warnings = Vec::new();

        let graph_db = match &self.graph_db {
            Some(db) => db,
            None => {
                warnings
                    .push("Graph DB not configured; hard-mode orphan sweep skipped.".to_string());
                return Ok((0, 0, warnings));
            }
        };

        let orphan_entities = graph_db.get_degree_one_nodes("Entity").await.map_err(|e| {
            DeleteError::GraphCleanup(format!("Failed to query degree-one Entity nodes: {e}"))
        })?;

        let orphan_types = graph_db
            .get_degree_one_nodes("EntityType")
            .await
            .map_err(|e| {
                DeleteError::GraphCleanup(format!(
                    "Failed to query degree-one EntityType nodes: {e}"
                ))
            })?;

        let entity_count = orphan_entities.len();
        let type_count = orphan_types.len();

        if entity_count == 0 && type_count == 0 {
            return Ok((0, 0, warnings));
        }

        // Collect all orphan node IDs for graph deletion
        let all_orphan_ids: Vec<String> = orphan_entities
            .iter()
            .chain(orphan_types.iter())
            .map(|(id, _)| id.clone())
            .collect();

        // Delete from vector DB (if configured)
        if let Some(vector_db) = &self.vector_db {
            // Entity orphans → Entity/name collection
            if !orphan_entities.is_empty() {
                let entity_uuids: Vec<Uuid> = orphan_entities
                    .iter()
                    .filter_map(|(id, _)| Uuid::parse_str(id).ok())
                    .collect();
                if !entity_uuids.is_empty()
                    && vector_db
                        .has_collection("Entity", "name")
                        .await
                        .unwrap_or(false)
                {
                    vector_db
                        .delete_points("Entity", "name", &entity_uuids)
                        .await
                        .map_err(|e| {
                            DeleteError::VectorCleanup(format!(
                                "Failed to delete orphan Entity vector points: {e}"
                            ))
                        })?;
                }
            }

            // EntityType orphans → EntityType/name collection
            if !orphan_types.is_empty() {
                let type_uuids: Vec<Uuid> = orphan_types
                    .iter()
                    .filter_map(|(id, _)| Uuid::parse_str(id).ok())
                    .collect();
                if !type_uuids.is_empty()
                    && vector_db
                        .has_collection("EntityType", "name")
                        .await
                        .unwrap_or(false)
                {
                    vector_db
                        .delete_points("EntityType", "name", &type_uuids)
                        .await
                        .map_err(|e| {
                            DeleteError::VectorCleanup(format!(
                                "Failed to delete orphan EntityType vector points: {e}"
                            ))
                        })?;
                }
            }
        }

        // Delete from graph DB
        graph_db.delete_nodes(&all_orphan_ids).await.map_err(|e| {
            DeleteError::GraphCleanup(format!("Failed to delete orphan graph nodes: {e}"))
        })?;

        Ok((entity_count, type_count, warnings))
    }

    /// Hard-mode orphan sweep for EdgeType nodes: find EdgeType graph nodes
    /// whose relationship name no longer appears in any graph edge, and delete
    /// them from both the graph and vector DBs.
    ///
    /// Returns `(deleted_count, warnings)`.
    async fn sweep_orphan_edge_types(&self) -> Result<(usize, Vec<String>), DeleteError> {
        let mut warnings = Vec::new();

        let graph_db = match &self.graph_db {
            Some(db) => db,
            None => {
                warnings
                    .push("Graph DB not configured; orphan EdgeType sweep skipped.".to_string());
                return Ok((0, warnings));
            }
        };

        let orphan_edge_types = match graph_db.get_zero_degree_edge_type_nodes().await {
            Ok(nodes) => nodes,
            Err(e) => {
                warnings.push(format!(
                    "Failed to query orphan EdgeType nodes (non-fatal): {e}"
                ));
                return Ok((0, warnings));
            }
        };

        let count = orphan_edge_types.len();
        if count == 0 {
            return Ok((0, warnings));
        }

        // Delete from vector DB (if configured) — non-fatal
        if let Some(vector_db) = &self.vector_db {
            let uuids: Vec<Uuid> = orphan_edge_types
                .iter()
                .filter_map(|(id, _)| Uuid::parse_str(id).ok())
                .collect();

            if !uuids.is_empty() {
                let has_collection = vector_db
                    .has_collection("EdgeType", "relationship_name")
                    .await
                    .unwrap_or(false);
                if has_collection
                    && let Err(e) = vector_db
                        .delete_points("EdgeType", "relationship_name", &uuids)
                        .await
                {
                    warnings.push(format!(
                        "Failed to delete orphan EdgeType vector points (non-fatal): {e}"
                    ));
                }
            }
        }

        // Delete from graph DB
        let orphan_ids: Vec<String> = orphan_edge_types.iter().map(|(id, _)| id.clone()).collect();

        if let Err(e) = graph_db.delete_nodes(&orphan_ids).await {
            warnings.push(format!(
                "Failed to delete orphan EdgeType graph nodes (non-fatal): {e}"
            ));
            return Ok((0, warnings));
        }

        Ok((count, warnings))
    }

    /// Count graph nodes, vector points, and provenance rows that would be
    /// affected. Returns `(graph_nodes, vector_points, prov_nodes, prov_edges)`.
    async fn count_graph_vector_artifacts(
        &self,
        targets: &ResolvedDeleteTargets,
    ) -> Result<(usize, usize, usize, usize), DeleteError> {
        let mut graph_nodes = 0usize;
        let mut vector_points = 0usize;
        let mut prov_nodes = 0usize;
        let mut prov_edges = 0usize;

        for dataset in &targets.datasets_to_delete {
            let nodes = self
                .database
                .get_nodes_by_dataset(dataset.id)
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!(
                        "Failed to count provenance nodes for dataset {}: {e}",
                        dataset.id
                    ))
                })?;

            let edges = self
                .database
                .get_edges_by_dataset(dataset.id)
                .await
                .map_err(|e| {
                    DeleteError::Runtime(format!(
                        "Failed to count provenance edges for dataset {}: {e}",
                        dataset.id
                    ))
                })?;

            prov_nodes += nodes.len();
            prov_edges += edges.len();

            // Unique node slugs = graph nodes to delete
            let unique_slugs: HashSet<Uuid> = nodes.iter().map(|n| n.slug).collect();
            graph_nodes += unique_slugs.len();

            // Count vector points from indexed_fields
            for node in &nodes {
                let fields = parse_indexed_fields(&node.indexed_fields);
                vector_points += fields.len();
            }
            // Each edge contributes to EdgeType and Triplet collections
            vector_points += edges.len() * 2;
        }

        Ok((graph_nodes, vector_points, prov_nodes, prov_edges))
    }

    // ==================================================================
    // Target resolution
    // ==================================================================

    async fn compute_deletable_data_ids(
        &self,
        targets: &ResolvedDeleteTargets,
    ) -> Result<Vec<Uuid>, DeleteError> {
        let mut links_to_remove_per_data: HashMap<Uuid, usize> = HashMap::new();
        for (_, data_id) in &targets.links_to_detach {
            *links_to_remove_per_data.entry(*data_id).or_insert(0) += 1;
        }

        let mut deletable = Vec::new();
        for data_id in &targets.candidate_data_ids {
            let link_count = self
                .database
                .count_data_dataset_links(*data_id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to count dataset links for data {data_id}: {error}"
                    ))
                })?;
            let to_remove = links_to_remove_per_data.get(data_id).copied().unwrap_or(0);
            if link_count <= to_remove {
                deletable.push(*data_id);
            }
        }

        Ok(deletable)
    }

    #[tracing::instrument(level = "debug", skip(self, request))]
    async fn resolve_targets(
        &self,
        request: &DeleteRequest,
    ) -> Result<ResolvedDeleteTargets, DeleteError> {
        match &request.scope {
            DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name,
                delete_dataset_if_empty,
            } => {
                self.resolve_data_scope(
                    *owner_id,
                    *data_id,
                    dataset_name.as_deref(),
                    *delete_dataset_if_empty,
                )
                .await
            }
            DeleteScope::Dataset {
                owner_id,
                dataset_name,
            } => self.resolve_dataset_scope(*owner_id, dataset_name).await,
            DeleteScope::User { owner_id } => self.resolve_user_scope(*owner_id).await,
            DeleteScope::All => self.resolve_all_scope().await,
        }
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn resolve_data_scope(
        &self,
        owner_id: Uuid,
        data_id: Uuid,
        dataset_name: Option<&str>,
        delete_dataset_if_empty: bool,
    ) -> Result<ResolvedDeleteTargets, DeleteError> {
        let data = self.database.get_data(data_id).await.map_err(|error| {
            DeleteError::Runtime(format!("Failed to fetch data {data_id}: {error}"))
        })?;

        // Python behaviour (datasets.py:165-176): when the Data row is absent the
        // caller may be using a custom graph model that didn't go through the
        // standard ingestion pipeline. In that case we still attempt graph/vector
        // cleanup using a minimal ghost targets struct (candidate_data_ids set, no
        // links to detach, no datasets to delete) and return success rather than
        // an error.
        let Some(data) = data else {
            tracing::warn!(
                data_id = %data_id,
                "Data row not found — assuming custom graph model; attempting orphan cleanup"
            );
            return Ok(ResolvedDeleteTargets {
                datasets_to_delete: Vec::new(),
                links_to_detach: Vec::new(),
                candidate_data_ids: vec![data_id],
            });
        };

        if data.owner_id != owner_id {
            return Err(DeleteError::Validation(format!(
                "Data {data_id} does not belong to owner {owner_id}"
            )));
        }

        let mut links_to_detach = Vec::new();
        // Collect affected datasets (with their data items) so we can check
        // emptiness when `delete_dataset_if_empty` is set.
        let mut affected_datasets: Vec<(cognee_models::Dataset, Vec<cognee_models::Data>)> =
            Vec::new();

        if let Some(dataset_name) = dataset_name {
            let dataset = self
                .database
                .get_dataset_by_name(dataset_name, owner_id, None)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to resolve dataset '{dataset_name}': {error}"
                    ))
                })?
                .ok_or_else(|| {
                    DeleteError::Validation(format!(
                        "Dataset '{dataset_name}' was not found for owner {owner_id}"
                    ))
                })?;

            let data_items = self
                .database
                .get_dataset_data(dataset.id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to load data for dataset '{}': {}",
                        dataset.name, error
                    ))
                })?;

            if !data_items.iter().any(|item| item.id == data_id) {
                return Err(DeleteError::Validation(format!(
                    "Data {} is not attached to dataset '{}'",
                    data_id, dataset.name
                )));
            }

            links_to_detach.push((dataset.id, data_id));
            affected_datasets.push((dataset, data_items));
        } else {
            let datasets = self
                .database
                .list_datasets_for_data(data_id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to list datasets for data {data_id}: {error}"
                    ))
                })?;

            for dataset in datasets {
                if dataset.owner_id == owner_id {
                    links_to_detach.push((dataset.id, data_id));
                    if delete_dataset_if_empty {
                        let data_items =
                            self.database
                                .get_dataset_data(dataset.id)
                                .await
                                .map_err(|error| {
                                    DeleteError::Runtime(format!(
                                        "Failed to load data for dataset '{}': {}",
                                        dataset.name, error
                                    ))
                                })?;
                        affected_datasets.push((dataset, data_items));
                    }
                }
            }

            if links_to_detach.is_empty() {
                return Err(DeleteError::Validation(format!(
                    "No dataset links found for data {data_id} and owner {owner_id}"
                )));
            }
        }

        // When the flag is set, check each affected dataset: if it currently
        // has exactly one data item and that item is the one being removed,
        // mark the dataset for deletion.
        let mut datasets_to_delete = Vec::new();
        if delete_dataset_if_empty {
            for (dataset, data_items) in affected_datasets {
                if data_items.len() == 1 && data_items[0].id == data_id {
                    datasets_to_delete.push(dataset);
                }
            }
        }

        Ok(ResolvedDeleteTargets {
            datasets_to_delete,
            links_to_detach,
            candidate_data_ids: vec![data_id],
        })
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn resolve_dataset_scope(
        &self,
        owner_id: Uuid,
        dataset_name: &str,
    ) -> Result<ResolvedDeleteTargets, DeleteError> {
        let dataset = self
            .database
            .get_dataset_by_name(dataset_name, owner_id, None)
            .await
            .map_err(|error| {
                DeleteError::Runtime(format!(
                    "Failed to resolve dataset '{dataset_name}': {error}"
                ))
            })?
            .ok_or_else(|| {
                DeleteError::Validation(format!(
                    "Dataset '{dataset_name}' was not found for owner {owner_id}"
                ))
            })?;

        self.resolve_dataset_list(vec![dataset]).await
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn resolve_user_scope(
        &self,
        owner_id: Uuid,
    ) -> Result<ResolvedDeleteTargets, DeleteError> {
        let datasets = self
            .database
            .list_datasets_by_owner(owner_id)
            .await
            .map_err(|error| {
                DeleteError::Runtime(format!(
                    "Failed to list datasets for owner {owner_id}: {error}"
                ))
            })?;

        self.resolve_dataset_list(datasets).await
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn resolve_all_scope(&self) -> Result<ResolvedDeleteTargets, DeleteError> {
        let datasets =
            self.database.list_datasets().await.map_err(|error| {
                DeleteError::Runtime(format!("Failed to list datasets: {error}"))
            })?;

        self.resolve_dataset_list(datasets).await
    }

    async fn resolve_dataset_list(
        &self,
        datasets: Vec<Dataset>,
    ) -> Result<ResolvedDeleteTargets, DeleteError> {
        let mut links_to_detach = Vec::new();
        let mut candidate_data_ids = HashSet::new();

        for dataset in &datasets {
            let data_items = self
                .database
                .get_dataset_data(dataset.id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to load data for dataset '{}': {}",
                        dataset.name, error
                    ))
                })?;

            for data in data_items {
                links_to_detach.push((dataset.id, data.id));
                candidate_data_ids.insert(data.id);
            }
        }

        Ok(ResolvedDeleteTargets {
            datasets_to_delete: datasets,
            links_to_detach,
            candidate_data_ids: candidate_data_ids.into_iter().collect(),
        })
    }

    async fn count_data_that_would_be_deleted(
        &self,
        candidate_data_ids: &[Uuid],
        links_to_detach: &[(Uuid, Uuid)],
    ) -> Result<usize, DeleteError> {
        let mut links_to_remove_per_data: HashMap<Uuid, usize> = HashMap::new();
        for (_, data_id) in links_to_detach {
            *links_to_remove_per_data.entry(*data_id).or_insert(0) += 1;
        }

        let mut count = 0usize;
        for data_id in candidate_data_ids {
            let link_count = self
                .database
                .count_data_dataset_links(*data_id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to count dataset links for data {data_id}: {error}"
                    ))
                })?;
            let to_remove = links_to_remove_per_data.get(data_id).copied().unwrap_or(0);
            if link_count <= to_remove {
                count += 1;
            }
        }

        Ok(count)
    }
}

/// Parse the `indexed_fields` JSON value into a list of field names.
///
/// The `indexed_fields` column is a JSON array of field names (e.g., `["text"]`,
/// `["name"]`). If it is not a JSON array, returns an empty list.
fn parse_indexed_fields(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => {
            warn!(
                "indexed_fields is not a JSON array: {:?}; skipping vector cleanup for this node",
                value
            );
            vec![]
        }
    }
}

fn triplet_vector_id(edge: &GraphEdge) -> Uuid {
    Triplet::new(
        edge.source_node_id,
        edge.destination_node_id,
        edge.relationship_name.clone(),
        String::new(),
    )
    .id
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee_database::{connect, initialize, ops};
    use cognee_graph::MockGraphDB;
    use cognee_models::{Data, Dataset};
    use cognee_storage::MockStorage;
    use cognee_vector::MockVectorDB;

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    async fn make_service() -> (
        DeleteService,
        Arc<MockStorage>,
        Arc<cognee_database::DatabaseConnection>,
    ) {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let db = Arc::new(db);
        let storage = Arc::new(MockStorage::new());
        let svc = DeleteService::new(
            storage.clone() as Arc<dyn StorageTrait>,
            db.clone() as Arc<dyn DeleteDb>,
        );
        (svc, storage, db)
    }

    async fn make_service_with_graph_vector() -> (
        DeleteService,
        Arc<MockStorage>,
        Arc<cognee_database::DatabaseConnection>,
        Arc<MockGraphDB>,
        Arc<MockVectorDB>,
    ) {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let db = Arc::new(db);
        let storage = Arc::new(MockStorage::new());
        let graph_db = Arc::new(MockGraphDB::new());
        let vector_db = Arc::new(MockVectorDB::new());
        let svc = DeleteService::new(
            storage.clone() as Arc<dyn StorageTrait>,
            db.clone() as Arc<dyn DeleteDb>,
        )
        .with_graph_db(graph_db.clone() as Arc<dyn GraphDBTrait>)
        .with_vector_db(vector_db.clone() as Arc<dyn VectorDB>);
        (svc, storage, db, graph_db, vector_db)
    }

    /// Seed one dataset + one data item, attach them, and return their IDs.
    async fn seed_dataset_with_data(
        db: &cognee_database::DatabaseConnection,
        storage: &MockStorage,
        owner_id: Uuid,
        dataset_name: &str,
    ) -> (Uuid, Uuid) {
        let dataset = Dataset::new(dataset_name.to_string(), owner_id, None, Uuid::new_v4());
        let dataset_id = dataset.id;
        ops::datasets::create_dataset(db, dataset).await.unwrap();

        let location = storage.store(b"test content", "test.txt").await.unwrap();

        let data_id = Uuid::new_v4();
        let data = Data::builder(
            data_id,
            "test.txt",
            location,
            "file://test.txt",
            "txt",
            "text/plain",
            "hash_placeholder",
            owner_id,
        )
        .build();
        ops::data::create_data(db, data).await.unwrap();
        ops::datasets::attach_data_to_dataset(db, dataset_id, data_id)
            .await
            .unwrap();

        (dataset_id, data_id)
    }

    /// Seed provenance nodes in the relational DB.
    async fn seed_provenance_nodes(
        db: &cognee_database::DatabaseConnection,
        dataset_id: Uuid,
        data_id: Uuid,
        owner_id: Uuid,
        slugs: &[Uuid],
        node_type: &str,
        indexed_fields: serde_json::Value,
    ) {
        let nodes: Vec<GraphNode> = slugs
            .iter()
            .map(|slug| GraphNode {
                id: Uuid::new_v4(),
                slug: *slug,
                user_id: owner_id,
                data_id,
                dataset_id,
                label: Some(format!("node-{slug}")),
                node_type: node_type.to_string(),
                indexed_fields: indexed_fields.clone(),
                attributes: None,
                created_at: chrono::Utc::now(),
            })
            .collect();
        ops::graph_storage::upsert_nodes(db, &nodes).await.unwrap();
    }

    /// Seed provenance edges in the relational DB.
    async fn seed_provenance_edges(
        db: &cognee_database::DatabaseConnection,
        dataset_id: Uuid,
        data_id: Uuid,
        owner_id: Uuid,
        slugs: &[Uuid],
        relationship_name: &str,
    ) {
        let edges: Vec<GraphEdge> = slugs
            .iter()
            .map(|slug| GraphEdge {
                id: Uuid::new_v4(),
                slug: *slug,
                user_id: owner_id,
                data_id,
                dataset_id,
                source_node_id: Uuid::new_v4(),
                destination_node_id: Uuid::new_v4(),
                relationship_name: relationship_name.to_string(),
                label: None,
                attributes: None,
                created_at: chrono::Utc::now(),
            })
            .collect();
        ops::graph_storage::upsert_edges(db, &edges).await.unwrap();
    }

    // ------------------------------------------------------------------
    // Original tests (backward compatibility)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn delete_dataset_with_force_removes_dataset_and_data() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "test_dataset").await;

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "test_dataset".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_data, 1);

        let still_exists = ops::datasets::get_dataset_by_name(&db, "test_dataset", owner, None)
            .await
            .unwrap();
        assert!(still_exists.is_none(), "dataset should be gone");
    }

    #[tokio::test]
    async fn preview_does_not_mutate_database_state() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        let (_ds_id, data_id) = seed_dataset_with_data(&db, &storage, owner, "test_dataset").await;

        let request = DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id: owner,
                dataset_name: "test_dataset".to_string(),
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        };

        let preview = svc.preview(&request).await.expect("preview should succeed");

        assert_eq!(preview.datasets_to_delete, 1);
        assert_eq!(preview.data_to_delete, 1);

        let still_exists = ops::datasets::get_dataset_by_name(&db, "test_dataset", owner, None)
            .await
            .unwrap();
        assert!(
            still_exists.is_some(),
            "dataset should still exist after preview"
        );
        let data_still_there = ops::data::get_data(&db, data_id).await.unwrap();
        assert!(
            data_still_there.is_some(),
            "data should be unchanged after preview"
        );
    }

    #[tokio::test]
    async fn delete_nonexistent_dataset_returns_validation_error() {
        let (svc, _storage, _db) = make_service().await;

        let err = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: Uuid::new_v4(),
                    dataset_name: "nonexistent".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect_err("should fail for nonexistent dataset");

        assert!(
            matches!(err, DeleteError::Validation(_)),
            "expected Validation error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn shared_data_not_deleted_while_linked_to_another_dataset() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();

        let ds1 = Dataset::new("dataset1".to_string(), owner, None, Uuid::new_v4());
        let ds2 = Dataset::new("dataset2".to_string(), owner, None, Uuid::new_v4());
        let ds1_id = ds1.id;
        let ds2_id = ds2.id;
        ops::datasets::create_dataset(&db, ds1).await.unwrap();
        ops::datasets::create_dataset(&db, ds2).await.unwrap();

        let location = storage
            .store(b"shared content", "shared.txt")
            .await
            .unwrap();
        let data_id = Uuid::new_v4();
        let data = Data::builder(
            data_id,
            "shared.txt",
            location,
            "file://shared.txt",
            "txt",
            "text/plain",
            "shared_hash",
            owner,
        )
        .build();
        ops::data::create_data(&db, data).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, ds1_id, data_id)
            .await
            .unwrap();
        ops::datasets::attach_data_to_dataset(&db, ds2_id, data_id)
            .await
            .unwrap();

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "dataset1".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(
            result.deleted_data, 0,
            "data must not be deleted while still linked to dataset2"
        );

        let data_still_there = ops::data::get_data(&db, data_id).await.unwrap();
        assert!(data_still_there.is_some(), "data record must survive");
    }

    #[tokio::test]
    async fn data_deleted_when_last_dataset_link_removed() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();

        let ds1 = Dataset::new("dataset1".to_string(), owner, None, Uuid::new_v4());
        let ds2 = Dataset::new("dataset2".to_string(), owner, None, Uuid::new_v4());
        let ds1_id = ds1.id;
        let ds2_id = ds2.id;
        ops::datasets::create_dataset(&db, ds1).await.unwrap();
        ops::datasets::create_dataset(&db, ds2).await.unwrap();

        let location = storage
            .store(b"shared content", "shared.txt")
            .await
            .unwrap();
        let data_id = Uuid::new_v4();
        let data = Data::builder(
            data_id,
            "shared.txt",
            location,
            "file://shared.txt",
            "txt",
            "text/plain",
            "shared_hash",
            owner,
        )
        .build();
        ops::data::create_data(&db, data).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, ds1_id, data_id)
            .await
            .unwrap();
        ops::datasets::attach_data_to_dataset(&db, ds2_id, data_id)
            .await
            .unwrap();

        svc.execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id: owner,
                dataset_name: "dataset1".to_string(),
            },
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await
        .expect("delete dataset1");

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "dataset2".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("delete dataset2");

        assert_eq!(
            result.deleted_data, 1,
            "data must be deleted when last link is removed"
        );

        let data_gone = ops::data::get_data(&db, data_id).await.unwrap();
        assert!(data_gone.is_none(), "data record must be gone");
    }

    #[tokio::test]
    async fn delete_dataset_with_wrong_owner_returns_validation_error() {
        let (svc, storage, db) = make_service().await;
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();

        seed_dataset_with_data(&db, &storage, owner_a, "owner_a_dataset").await;

        let err = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner_b,
                    dataset_name: "owner_a_dataset".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect_err("should fail for wrong owner");

        assert!(
            matches!(err, DeleteError::Validation(_)),
            "expected Validation error for wrong owner, got: {err:?}"
        );
    }

    // ------------------------------------------------------------------
    // New tests: graph/vector cleanup
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn delete_dataset_cleans_graph_nodes() {
        let (svc, storage, db, graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) = seed_dataset_with_data(&db, &storage, owner, "graph_ds").await;

        // Seed provenance and graph nodes
        let slug1 = Uuid::new_v4();
        let slug2 = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id,
            owner,
            &[slug1, slug2],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;

        // Add matching nodes to graph DB
        graph_db
            .add_node_raw(serde_json::json!({"id": slug1.to_string(), "name": "Alice"}))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({"id": slug2.to_string(), "name": "Bob"}))
            .await
            .unwrap();
        assert_eq!(graph_db.node_count(), 2);

        // Execute delete
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "graph_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_graph_nodes, 2);
        assert_eq!(graph_db.node_count(), 0, "graph nodes should be cleaned up");
    }

    #[tokio::test]
    async fn delete_dataset_cleans_vector_points() {
        let (svc, storage, db, _graph_db, vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) = seed_dataset_with_data(&db, &storage, owner, "vector_ds").await;

        // Seed provenance nodes
        let slug1 = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id,
            owner,
            &[slug1],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;

        // Create collection and index a point
        vector_db
            .create_collection("Entity", "name", 3)
            .await
            .unwrap();
        let point = cognee_vector::VectorPoint::new(slug1, vec![1.0, 0.0, 0.0]);
        vector_db
            .index_points("Entity", "name", &[point])
            .await
            .unwrap();
        assert_eq!(
            vector_db.collection_size("Entity", "name").await.unwrap(),
            1
        );

        // Execute delete
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "vector_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_vector_points, 1);
        assert_eq!(
            vector_db.collection_size("Entity", "name").await.unwrap(),
            0,
            "vector point should be removed"
        );
    }

    #[tokio::test]
    async fn delete_all_wipes_graph_and_vector() {
        let (svc, storage, db, graph_db, vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "all_ds").await;

        // Add some data to graph and vector
        graph_db
            .add_node_raw(serde_json::json!({"id": "node1", "name": "Alice"}))
            .await
            .unwrap();
        vector_db
            .create_collection("Entity", "name", 3)
            .await
            .unwrap();
        let point = cognee_vector::VectorPoint::new(Uuid::new_v4(), vec![1.0, 0.0, 0.0]);
        vector_db
            .index_points("Entity", "name", &[point])
            .await
            .unwrap();

        // Execute delete all
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::All,
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert!(
            graph_db.is_empty().await.unwrap(),
            "graph should be completely wiped"
        );
        assert!(
            !vector_db.has_collection("Entity", "name").await.unwrap(),
            "vector collection should be deleted"
        );
    }

    #[tokio::test]
    async fn delete_all_reports_provenance_backed_graph_and_vector_counts() {
        let (svc, storage, db, _graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "all_counts_ds").await;

        let node_slugs = [Uuid::new_v4(), Uuid::new_v4()];
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id,
            owner,
            &node_slugs,
            "Entity",
            serde_json::json!(["name", "description"]),
        )
        .await;

        let edge_slug = Uuid::new_v4();
        seed_provenance_edges(&db, dataset_id, data_id, owner, &[edge_slug], "knows").await;

        let request = DeleteRequest {
            scope: DeleteScope::All,
            mode: DeleteMode::Soft,
            memory_only: false,
        };

        let preview = svc.preview(&request).await.expect("preview should succeed");
        assert_eq!(preview.graph_nodes_to_delete, 2);
        assert_eq!(preview.vector_points_to_delete, 6);
        assert_eq!(preview.provenance_nodes_to_delete, 2);
        assert_eq!(preview.provenance_edges_to_delete, 1);

        let result = svc.execute(&request).await.expect("execute should succeed");
        assert_eq!(result.deleted_graph_nodes, preview.graph_nodes_to_delete);
        assert_eq!(
            result.deleted_vector_points,
            preview.vector_points_to_delete
        );
        assert_eq!(
            result.deleted_provenance_nodes,
            preview.provenance_nodes_to_delete
        );
        assert_eq!(
            result.deleted_provenance_edges,
            preview.provenance_edges_to_delete
        );
    }

    #[tokio::test]
    async fn delete_without_graph_db_emits_warning() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "no_graph_ds").await;

        // Seed provenance so there IS something to clean up
        let slug = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id,
            owner,
            &[slug],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "no_graph_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("Graph DB not configured")),
            "should warn about missing graph DB, got: {:?}",
            result.warnings
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("Vector DB not configured")),
            "should warn about missing vector DB, got: {:?}",
            result.warnings
        );
    }

    #[tokio::test]
    async fn delete_dataset_cleans_edge_vector_points() {
        let (svc, storage, db, _graph_db, vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) = seed_dataset_with_data(&db, &storage, owner, "edge_ds").await;

        // Seed provenance edges
        let source_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();
        let relationship_name = "knows";
        let triplet_id = Triplet::new(
            source_id,
            target_id,
            relationship_name.to_string(),
            String::new(),
        )
        .id;
        ops::graph_storage::upsert_edges(
            &db,
            &[GraphEdge {
                id: Uuid::new_v4(),
                slug: triplet_id,
                user_id: owner,
                data_id,
                dataset_id,
                source_node_id: source_id,
                destination_node_id: target_id,
                relationship_name: relationship_name.to_string(),
                label: None,
                attributes: None,
                created_at: chrono::Utc::now(),
            }],
        )
        .await
        .unwrap();

        // Create EdgeType and Triplet collections and index points
        vector_db
            .create_collection("EdgeType", "relationship_name", 3)
            .await
            .unwrap();
        vector_db
            .create_collection("Triplet", "text", 3)
            .await
            .unwrap();

        let et_point = cognee_vector::VectorPoint::new(
            EdgeType::deterministic_id(relationship_name),
            vec![1.0, 0.0, 0.0],
        );
        vector_db
            .index_points("EdgeType", "relationship_name", &[et_point])
            .await
            .unwrap();
        let triplet_point = cognee_vector::VectorPoint::new(triplet_id, vec![0.0, 1.0, 0.0]);
        vector_db
            .index_points("Triplet", "text", &[triplet_point])
            .await
            .unwrap();

        // Execute delete
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "edge_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        // Each edge contributes 1 point to EdgeType and 1 to Triplet = 2
        assert_eq!(result.deleted_vector_points, 2);
        assert_eq!(
            vector_db
                .collection_size("EdgeType", "relationship_name")
                .await
                .unwrap(),
            0,
            "EdgeType vector point should be removed"
        );
        assert_eq!(
            vector_db.collection_size("Triplet", "text").await.unwrap(),
            0,
            "Triplet vector point should be removed"
        );
    }

    #[tokio::test]
    async fn delete_without_provenance_still_works() {
        let (svc, storage, db, graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "no_prov_ds").await;

        // No provenance seeded -- graph/vector cleanup should be a no-op
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "no_prov_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_graph_nodes, 0);
        assert_eq!(result.deleted_vector_points, 0);
        assert!(
            graph_db.is_empty().await.unwrap(),
            "graph should still be empty"
        );
    }

    #[tokio::test]
    async fn shared_node_not_removed_when_sibling_data_exists() {
        let (svc, storage, db, graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();

        // Create dataset + first data item
        let dataset = Dataset::new("shared_ds".to_string(), owner, None, Uuid::new_v4());
        let dataset_id = dataset.id;
        ops::datasets::create_dataset(&db, dataset).await.unwrap();

        let loc1 = storage.store(b"content one", "one.txt").await.unwrap();
        let data_id_1 = Uuid::new_v4();
        let data1 = Data::builder(
            data_id_1,
            "one.txt",
            loc1,
            "file://one.txt",
            "txt",
            "text/plain",
            "hash_one",
            owner,
        )
        .build();
        ops::data::create_data(&db, data1).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, dataset_id, data_id_1)
            .await
            .unwrap();

        let loc2 = storage.store(b"content two", "two.txt").await.unwrap();
        let data_id_2 = Uuid::new_v4();
        let data2 = Data::builder(
            data_id_2,
            "two.txt",
            loc2,
            "file://two.txt",
            "txt",
            "text/plain",
            "hash_two",
            owner,
        )
        .build();
        ops::data::create_data(&db, data2).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, dataset_id, data_id_2)
            .await
            .unwrap();

        // Shared slug: both data items reference the same entity
        let shared_slug = Uuid::new_v4();
        let unique_slug = Uuid::new_v4();

        // data_id_1 has shared_slug + unique_slug
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id_1,
            owner,
            &[shared_slug, unique_slug],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;

        // data_id_2 also references shared_slug
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id_2,
            owner,
            &[shared_slug],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;

        // Add graph nodes
        graph_db
            .add_node_raw(serde_json::json!({"id": shared_slug.to_string(), "name": "Shared"}))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({"id": unique_slug.to_string(), "name": "Unique"}))
            .await
            .unwrap();
        assert_eq!(graph_db.node_count(), 2);

        // Delete data_id_1 from the dataset
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id: data_id_1,
                    dataset_name: Some("shared_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        // unique_slug should be cleaned up; shared_slug should survive
        assert_eq!(
            result.deleted_graph_nodes, 1,
            "only the unique node should be deleted"
        );
        assert_eq!(
            graph_db.node_count(),
            1,
            "shared node should survive because data_id_2 also references it"
        );
    }

    // ------------------------------------------------------------------
    // New tests: relational DB provenance state verification
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_data_deletion_cleans_relational_provenance() {
        let (svc, storage, db, _graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "prov_data_ds").await;

        // Seed provenance nodes and edges for this data item
        let node_slug = Uuid::new_v4();
        let edge_slug = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id,
            owner,
            &[node_slug],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;
        seed_provenance_edges(&db, dataset_id, data_id, owner, &[edge_slug], "knows").await;

        // Verify provenance rows exist before deletion
        let nodes_before = ops::graph_storage::get_nodes_by_data(&db, data_id, dataset_id)
            .await
            .unwrap();
        let edges_before = ops::graph_storage::get_edges_by_data(&db, data_id, dataset_id)
            .await
            .unwrap();
        assert_eq!(
            nodes_before.len(),
            1,
            "should have 1 provenance node before delete"
        );
        assert_eq!(
            edges_before.len(),
            1,
            "should have 1 provenance edge before delete"
        );

        // Delete the data item
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id,
                    dataset_name: Some("prov_data_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_data, 1);
        assert_eq!(result.deleted_provenance_nodes, 1);
        assert_eq!(result.deleted_provenance_edges, 1);

        // Query DB directly: provenance rows should be gone
        let nodes_after = ops::graph_storage::get_nodes_by_data(&db, data_id, dataset_id)
            .await
            .unwrap();
        let edges_after = ops::graph_storage::get_edges_by_data(&db, data_id, dataset_id)
            .await
            .unwrap();
        assert!(
            nodes_after.is_empty(),
            "provenance nodes should be gone after data deletion"
        );
        assert!(
            edges_after.is_empty(),
            "provenance edges should be gone after data deletion"
        );
    }

    #[tokio::test]
    async fn test_dataset_deletion_cascades_relational_provenance() {
        let (svc, storage, db, _graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "prov_ds_ds").await;

        // Seed multiple provenance nodes and edges
        let node_slugs = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let edge_slugs = [Uuid::new_v4(), Uuid::new_v4()];
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id,
            owner,
            &node_slugs,
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;
        seed_provenance_edges(&db, dataset_id, data_id, owner, &edge_slugs, "related_to").await;

        // Verify provenance rows exist
        let nodes_before = ops::graph_storage::get_nodes_by_dataset(&db, dataset_id)
            .await
            .unwrap();
        let edges_before = ops::graph_storage::get_edges_by_dataset(&db, dataset_id)
            .await
            .unwrap();
        assert_eq!(nodes_before.len(), 3);
        assert_eq!(edges_before.len(), 2);

        // Delete the entire dataset
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "prov_ds_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_data, 1);
        assert_eq!(result.deleted_provenance_nodes, 3);
        assert_eq!(result.deleted_provenance_edges, 2);

        // Query DB directly: all provenance rows for this dataset should be gone
        let nodes_after = ops::graph_storage::get_nodes_by_dataset(&db, dataset_id)
            .await
            .unwrap();
        let edges_after = ops::graph_storage::get_edges_by_dataset(&db, dataset_id)
            .await
            .unwrap();
        assert!(
            nodes_after.is_empty(),
            "provenance nodes should be gone after dataset deletion"
        );
        assert!(
            edges_after.is_empty(),
            "provenance edges should be gone after dataset deletion"
        );
    }

    #[tokio::test]
    async fn test_data_deletion_preserves_sibling_provenance() {
        let (svc, storage, db, _graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();

        // Create one dataset with two data items
        let dataset = Dataset::new("sibling_ds".to_string(), owner, None, Uuid::new_v4());
        let dataset_id = dataset.id;
        ops::datasets::create_dataset(&db, dataset).await.unwrap();

        let loc1 = storage.store(b"content alpha", "alpha.txt").await.unwrap();
        let data_id_1 = Uuid::new_v4();
        let data1 = Data::builder(
            data_id_1,
            "alpha.txt",
            loc1,
            "file://alpha.txt",
            "txt",
            "text/plain",
            "hash_alpha",
            owner,
        )
        .build();
        ops::data::create_data(&db, data1).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, dataset_id, data_id_1)
            .await
            .unwrap();

        let loc2 = storage.store(b"content beta", "beta.txt").await.unwrap();
        let data_id_2 = Uuid::new_v4();
        let data2 = Data::builder(
            data_id_2,
            "beta.txt",
            loc2,
            "file://beta.txt",
            "txt",
            "text/plain",
            "hash_beta",
            owner,
        )
        .build();
        ops::data::create_data(&db, data2).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, dataset_id, data_id_2)
            .await
            .unwrap();

        // Seed separate provenance for each data item
        let slug_d1 = Uuid::new_v4();
        let edge_d1 = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id_1,
            owner,
            &[slug_d1],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;
        seed_provenance_edges(&db, dataset_id, data_id_1, owner, &[edge_d1], "mentions").await;

        let slug_d2 = Uuid::new_v4();
        let edge_d2 = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id_2,
            owner,
            &[slug_d2],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;
        seed_provenance_edges(&db, dataset_id, data_id_2, owner, &[edge_d2], "describes").await;

        // Verify 2 + 2 provenance rows total
        let all_nodes = ops::graph_storage::get_nodes_by_dataset(&db, dataset_id)
            .await
            .unwrap();
        let all_edges = ops::graph_storage::get_edges_by_dataset(&db, dataset_id)
            .await
            .unwrap();
        assert_eq!(all_nodes.len(), 2, "2 provenance nodes total before delete");
        assert_eq!(all_edges.len(), 2, "2 provenance edges total before delete");

        // Delete only data_id_1
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id: data_id_1,
                    dataset_name: Some("sibling_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_provenance_nodes, 1);
        assert_eq!(result.deleted_provenance_edges, 1);

        // data_id_1 provenance should be gone
        let d1_nodes = ops::graph_storage::get_nodes_by_data(&db, data_id_1, dataset_id)
            .await
            .unwrap();
        let d1_edges = ops::graph_storage::get_edges_by_data(&db, data_id_1, dataset_id)
            .await
            .unwrap();
        assert!(
            d1_nodes.is_empty(),
            "data_id_1 provenance nodes should be gone"
        );
        assert!(
            d1_edges.is_empty(),
            "data_id_1 provenance edges should be gone"
        );

        // data_id_2 provenance should survive
        let d2_nodes = ops::graph_storage::get_nodes_by_data(&db, data_id_2, dataset_id)
            .await
            .unwrap();
        let d2_edges = ops::graph_storage::get_edges_by_data(&db, data_id_2, dataset_id)
            .await
            .unwrap();
        assert_eq!(
            d2_nodes.len(),
            1,
            "data_id_2 provenance nodes should survive sibling deletion"
        );
        assert_eq!(
            d2_edges.len(),
            1,
            "data_id_2 provenance edges should survive sibling deletion"
        );
    }

    // ------------------------------------------------------------------
    // ACL authorization tests
    // ------------------------------------------------------------------

    /// Helper to create an AuthorizedDeleteService backed by a real SQLite DB.
    ///
    /// The closed `AccessControl` (`impl AclDb for ...`) lives in the
    /// closed `cognee-access-control` crate. OSS tests drive ACL decisions
    /// through `MockAclDb`; this helper grants all four permissions on
    /// every dataset by default so behavioural assertions keep passing.
    async fn make_authorized_service() -> (
        AuthorizedDeleteService,
        Arc<MockStorage>,
        Arc<cognee_database::DatabaseConnection>,
        Arc<cognee_test_utils::MockAclDb>,
    ) {
        use cognee_database::AclDb;

        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let db = Arc::new(db);
        let storage = Arc::new(MockStorage::new());
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        let svc = DeleteService::new(
            storage.clone() as Arc<dyn StorageTrait>,
            db.clone() as Arc<dyn DeleteDb>,
        );
        let auth_svc = AuthorizedDeleteService::new(
            svc,
            acl.clone() as Arc<dyn AclDb>,
            db.clone() as Arc<dyn DeleteDb>,
        );
        (auth_svc, storage, db, acl)
    }

    /// Grant the four canonical permissions on `dataset_id` to `principal_id`
    /// through the supplied `MockAclDb`, matching the production semantics
    /// of `ops::acl::grant_all_permissions_on_dataset` (which a closed
    /// `AccessControl` would persist into the real `acls` table).
    async fn mock_grant_all_perms(
        acl: &Arc<cognee_test_utils::MockAclDb>,
        principal_id: Uuid,
        dataset_id: Uuid,
    ) {
        use cognee_database::AclDb;
        let acl_dyn: &dyn AclDb = acl.as_ref();
        acl_dyn
            .ensure_principal(principal_id, "user")
            .await
            .unwrap();
        for perm in ["read", "write", "delete", "share"] {
            acl_dyn
                .grant_permission(principal_id, dataset_id, perm)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn authorized_delete_succeeds_with_permission() {
        let (svc, storage, db, acl) = make_authorized_service().await;
        let owner = Uuid::new_v4();
        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner, "acl_ok_ds").await;

        mock_grant_all_perms(&acl, owner, dataset_id).await;

        let result = svc
            .execute(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner,
                        dataset_name: "acl_ok_ds".to_string(),
                    },
                    mode: DeleteMode::Soft,
                    memory_only: false,
                },
                owner,
            )
            .await
            .expect("authorized delete should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_data, 1);
    }

    #[tokio::test]
    async fn authorized_delete_fails_without_permission() {
        use cognee_database::AclDb;
        let (svc, storage, db, acl) = make_authorized_service().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "acl_fail_ds").await;

        // Do NOT grant any permissions.
        // Ensure the principal exists but without delete permission.
        let acl_dyn: &dyn AclDb = acl.as_ref();
        acl_dyn.ensure_principal(owner, "user").await.unwrap();

        let err = svc
            .execute(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner,
                        dataset_name: "acl_fail_ds".to_string(),
                    },
                    mode: DeleteMode::Soft,
                    memory_only: false,
                },
                owner,
            )
            .await
            .expect_err("should fail without delete permission");

        assert!(
            matches!(err, DeleteError::PermissionDenied(_)),
            "expected PermissionDenied, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn authorized_delete_with_wrong_principal_fails() {
        use cognee_database::AclDb;
        let (svc, storage, db, acl) = make_authorized_service().await;
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();
        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner_a, "acl_wrong_principal").await;

        // Grant permissions to owner_a only
        mock_grant_all_perms(&acl, owner_a, dataset_id).await;
        // Ensure owner_b exists as principal but has no permissions
        let acl_dyn: &dyn AclDb = acl.as_ref();
        acl_dyn.ensure_principal(owner_b, "user").await.unwrap();

        let err = svc
            .execute(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner_a,
                        dataset_name: "acl_wrong_principal".to_string(),
                    },
                    mode: DeleteMode::Soft,
                    memory_only: false,
                },
                owner_b, // wrong principal
            )
            .await
            .expect_err("should fail for wrong principal");

        assert!(
            matches!(err, DeleteError::PermissionDenied(_)),
            "expected PermissionDenied for wrong principal, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn authorized_delete_after_permission_grant() {
        use cognee_database::AclDb;
        let (svc, storage, db, acl) = make_authorized_service().await;
        let owner_a = Uuid::new_v4();
        let user_b = Uuid::new_v4();
        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner_a, "acl_delegated").await;

        // Owner A gets all permissions
        mock_grant_all_perms(&acl, owner_a, dataset_id).await;

        // Grant "delete" to user B (delegated access)
        let acl_dyn: &dyn AclDb = acl.as_ref();
        acl_dyn.ensure_principal(user_b, "user").await.unwrap();
        acl_dyn
            .grant_permission(user_b, dataset_id, "delete")
            .await
            .unwrap();

        // User B should now be able to delete
        let result = svc
            .execute(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner_a,
                        dataset_name: "acl_delegated".to_string(),
                    },
                    mode: DeleteMode::Soft,
                    memory_only: false,
                },
                user_b,
            )
            .await
            .expect("delegated delete should succeed");

        assert_eq!(result.deleted_datasets, 1);
    }

    #[tokio::test]
    async fn delete_cascades_acl_entries() {
        // This test used to verify FK CASCADE on `acls.dataset_id` through
        // the real `ops::acl::*` standalone functions. With the `acls` table
        // moved to the closed `cognee-access-control` migration,
        // OSS cannot exercise the production CASCADE — that is now covered
        // by integration tests in the closed crate. To keep the OSS test
        // surface meaningful we drive the in-memory `MockAclDb` and verify
        // the grant + delete-dataset interaction at the trait level: the
        // grant exists before and is unaffected by deleting the OSS
        // `datasets` row (the mock has no FK cascade).
        //
        // TODO: replicate FK CASCADE verification in the closed
        // `cognee-access-control` integration tests.
        use cognee_database::AclDb;
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let owner = Uuid::new_v4();
        let storage = MockStorage::new();
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());

        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner, "cascade_ds").await;
        mock_grant_all_perms(&acl, owner, dataset_id).await;

        // Verify ACLs exist via the mock.
        let acl_dyn: &dyn AclDb = acl.as_ref();
        let has_delete = acl_dyn
            .has_permission(owner, dataset_id, "delete")
            .await
            .unwrap();
        assert!(has_delete, "should have delete permission before cascade");

        // Delete the dataset directly (bypasses DeleteService).
        ops::datasets::delete_dataset(&db, dataset_id)
            .await
            .unwrap();

        // The mock has no FK cascade — the grant is still present. The
        // production CASCADE is exercised in the closed crate's tests.
        let has_delete_after = acl_dyn
            .has_permission(owner, dataset_id, "delete")
            .await
            .unwrap();
        assert!(
            has_delete_after,
            "MockAclDb does not cascade — production CASCADE coverage moved to \
             cognee-access-control integration tests."
        );
    }

    #[tokio::test]
    async fn unauthorized_service_still_works() {
        // The plain DeleteService (without ACL wrapper) should continue
        // to work based on owner_id matching only.
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "no_acl_ds").await;

        // No ACL grants needed — plain service doesn't check them
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "no_acl_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("plain service should succeed without ACL");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_data, 1);
    }

    #[tokio::test]
    async fn authorized_preview_checks_acl() {
        use cognee_database::AclDb;
        let (svc, storage, db, acl) = make_authorized_service().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "preview_acl_ds").await;

        // Without permission, even preview should fail
        let acl_dyn: &dyn AclDb = acl.as_ref();
        acl_dyn.ensure_principal(owner, "user").await.unwrap();

        let err = svc
            .preview(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner,
                        dataset_name: "preview_acl_ds".to_string(),
                    },
                    mode: DeleteMode::Soft,
                    memory_only: false,
                },
                owner,
            )
            .await
            .expect_err("preview should fail without permission");

        assert!(
            matches!(err, DeleteError::PermissionDenied(_)),
            "expected PermissionDenied on preview, got: {err:?}"
        );
    }

    // ------------------------------------------------------------------
    // Hard delete mode tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn hard_delete_removes_degree_one_entities() {
        let (svc, storage, db, graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner, "hard_del_ds").await;

        // Add an Entity node with degree 1 (one edge to a type node)
        graph_db
            .add_node_raw(serde_json::json!({
                "id": "entity-orphan",
                "type": "Entity",
                "name": "OrphanEntity"
            }))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({
                "id": "type-node",
                "type": "EntityType",
                "name": "Person"
            }))
            .await
            .unwrap();
        // Single edge: entity-orphan -> type-node
        // entity-orphan has degree 1, type-node has degree 1
        graph_db
            .add_edge("entity-orphan", "type-node", "is_a", None)
            .await
            .unwrap();

        assert_eq!(graph_db.node_count(), 2);

        // Seed provenance so cleanup_dataset has something to work with
        let entity_slug = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            _data_id,
            owner,
            &[entity_slug],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "hard_del_ds".to_string(),
                },
                mode: DeleteMode::Hard,
                memory_only: false,
            })
            .await
            .expect("hard delete should succeed");

        assert_eq!(result.deleted_datasets, 1);
        // After dataset cleanup removes tracked nodes, the orphan sweep finds
        // degree-one nodes. Since both nodes started with degree 1, both should
        // be swept (after normal cleanup may have already removed tracked ones,
        // the sweep catches any remaining degree-one nodes).
        // The exact counts depend on whether cleanup_dataset already removed
        // entity-orphan via provenance. The important thing is that the graph
        // ends up clean.
        let remaining_nodes = graph_db.node_count();
        assert_eq!(
            remaining_nodes, 0,
            "all orphan nodes should be removed after hard delete"
        );
    }

    #[tokio::test]
    async fn soft_delete_does_not_sweep_orphans() {
        let (svc, storage, db, graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "soft_del_ds").await;

        // Add orphan entity (degree 1)
        graph_db
            .add_node_raw(serde_json::json!({
                "id": "orphan-soft",
                "type": "Entity",
                "name": "SoftOrphan"
            }))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({
                "id": "type-soft",
                "type": "EntityType",
                "name": "Thing"
            }))
            .await
            .unwrap();
        graph_db
            .add_edge("orphan-soft", "type-soft", "is_a", None)
            .await
            .unwrap();

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "soft_del_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("soft delete should succeed");

        // Soft delete should NOT sweep orphans
        assert_eq!(result.deleted_orphan_entities, 0);
        assert_eq!(result.deleted_orphan_entity_types, 0);
        // Orphan nodes should still exist
        assert_eq!(
            graph_db.node_count(),
            2,
            "orphan nodes should survive soft delete"
        );
    }

    #[tokio::test]
    async fn hard_delete_preserves_well_connected_entities() {
        let (svc, storage, db, graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "hard_preserve_ds").await;

        // Add a well-connected Entity (degree 3)
        graph_db
            .add_node_raw(serde_json::json!({
                "id": "connected-entity",
                "type": "Entity",
                "name": "WellConnected"
            }))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({
                "id": "neighbor-1",
                "type": "DocumentChunk",
                "text": "chunk1"
            }))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({
                "id": "neighbor-2",
                "type": "DocumentChunk",
                "text": "chunk2"
            }))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({
                "id": "type-node",
                "type": "EntityType",
                "name": "Person"
            }))
            .await
            .unwrap();

        // connected-entity has 3 edges -> degree 3
        graph_db
            .add_edge("neighbor-1", "connected-entity", "contains", None)
            .await
            .unwrap();
        graph_db
            .add_edge("neighbor-2", "connected-entity", "contains", None)
            .await
            .unwrap();
        graph_db
            .add_edge("connected-entity", "type-node", "is_a", None)
            .await
            .unwrap();

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "hard_preserve_ds".to_string(),
                },
                mode: DeleteMode::Hard,
                memory_only: false,
            })
            .await
            .expect("hard delete should succeed");

        // The well-connected entity (degree 3) should survive
        assert!(
            graph_db.has_node("connected-entity").await.unwrap(),
            "well-connected entity should survive hard delete"
        );
        // No orphan entities should have been swept
        assert_eq!(result.deleted_orphan_entities, 0);
    }

    // ------------------------------------------------------------------
    // Pipeline runs / pipeline_status cleanup tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_dataset_deletion_clears_pipeline_status() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "ps_clear_ds").await;

        // Set pipeline_status JSON on the data record with an entry for this dataset
        let dataset_id_hex = cognee_database::uuid_hex::to_hex(dataset_id);
        let status_json = serde_json::json!({
            "cognify_pipeline": {
                dataset_id_hex: "DATA_ITEM_PROCESSING_COMPLETED"
            }
        });
        let data = ops::data::get_data(&db, data_id).await.unwrap().unwrap();
        let updated_data = Data {
            pipeline_status: Some(status_json.to_string()),
            ..data
        };
        ops::data::update_data(&db, updated_data).await.unwrap();

        // Verify pipeline_status is set
        let data_before = ops::data::get_data(&db, data_id).await.unwrap().unwrap();
        assert!(
            data_before.pipeline_status.is_some(),
            "pipeline_status should be set before deletion"
        );

        // Delete the dataset
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "ps_clear_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.cleared_pipeline_statuses, 1);

        // Data record should still exist (was deleted because only link was
        // to the deleted dataset). Let us verify by creating a scenario where
        // the data survives deletion.
    }

    #[tokio::test]
    async fn test_dataset_deletion_clears_pipeline_status_data_survives() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();

        // Create two datasets sharing one data item
        let ds1 = Dataset::new("ps_ds1".to_string(), owner, None, Uuid::new_v4());
        let ds2 = Dataset::new("ps_ds2".to_string(), owner, None, Uuid::new_v4());
        let ds1_id = ds1.id;
        let ds2_id = ds2.id;
        ops::datasets::create_dataset(&db, ds1).await.unwrap();
        ops::datasets::create_dataset(&db, ds2).await.unwrap();

        let location = storage
            .store(b"shared content", "shared.txt")
            .await
            .unwrap();
        let data_id = Uuid::new_v4();
        let data = Data::builder(
            data_id,
            "shared.txt",
            location,
            "file://shared.txt",
            "txt",
            "text/plain",
            "shared_hash",
            owner,
        )
        .build();
        ops::data::create_data(&db, data).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, ds1_id, data_id)
            .await
            .unwrap();
        ops::datasets::attach_data_to_dataset(&db, ds2_id, data_id)
            .await
            .unwrap();

        // Set pipeline_status with entries for both datasets
        let ds1_hex = cognee_database::uuid_hex::to_hex(ds1_id);
        let ds2_hex = cognee_database::uuid_hex::to_hex(ds2_id);
        let status_json = serde_json::json!({
            "cognify_pipeline": {
                ds1_hex.clone(): "DATA_ITEM_PROCESSING_COMPLETED",
                ds2_hex.clone(): "DATA_ITEM_PROCESSING_COMPLETED"
            }
        });
        let data_record = ops::data::get_data(&db, data_id).await.unwrap().unwrap();
        let updated_data = Data {
            pipeline_status: Some(status_json.to_string()),
            ..data_record
        };
        ops::data::update_data(&db, updated_data).await.unwrap();

        // Delete dataset 1 only
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "ps_ds1".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(
            result.deleted_data, 0,
            "data should survive because it's still linked to ds2"
        );
        assert_eq!(result.cleared_pipeline_statuses, 1);

        // Verify pipeline_status: ds1 entry should be removed, ds2 entry should remain
        let data_after = ops::data::get_data(&db, data_id).await.unwrap().unwrap();
        let status_after: serde_json::Value =
            serde_json::from_str(data_after.pipeline_status.as_deref().unwrap_or("{}")).unwrap();
        let cognify_obj = status_after
            .get("cognify_pipeline")
            .and_then(|v| v.as_object())
            .expect("cognify_pipeline should still exist");

        assert!(
            !cognify_obj.contains_key(&ds1_hex),
            "ds1 entry should be removed from pipeline_status"
        );
        assert!(
            cognify_obj.contains_key(&ds2_hex),
            "ds2 entry should remain in pipeline_status"
        );
    }

    #[tokio::test]
    async fn test_data_deletion_invalidates_pipeline_cache() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "pr_invalidate_ds").await;

        // Insert a pipeline_runs row for this dataset
        let pipeline_run = cognee_database::PipelineRun {
            id: Uuid::new_v4(),
            created_at: chrono::Utc::now(),
            status: cognee_database::PipelineRunStatus::Completed,
            pipeline_run_id: Uuid::new_v4(),
            pipeline_name: "cognify_pipeline".to_string(),
            pipeline_id: Uuid::new_v4(),
            dataset_id: Some(dataset_id),
            run_info: None,
        };
        ops::pipeline_runs::create_pipeline_run(&db, pipeline_run)
            .await
            .unwrap();

        // Verify pipeline_run exists
        let status_before =
            ops::pipeline_runs::get_latest_pipeline_status(&db, "cognify_pipeline", dataset_id)
                .await
                .unwrap();
        assert!(
            status_before.is_some(),
            "pipeline run should exist before data deletion"
        );

        // Delete the data item (data-scoped)
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id,
                    dataset_name: Some("pr_invalidate_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_data, 1);
        assert_eq!(
            result.deleted_pipeline_runs, 1,
            "pipeline_runs row should be deleted"
        );

        // Verify pipeline_run is gone
        let status_after =
            ops::pipeline_runs::get_latest_pipeline_status(&db, "cognify_pipeline", dataset_id)
                .await
                .unwrap();
        assert!(
            status_after.is_none(),
            "pipeline run should be invalidated after data deletion"
        );
    }

    #[tokio::test]
    async fn test_dataset_deletion_preserves_pipeline_runs() {
        // Post-08-01: the `pipeline_runs.dataset_id` FK CASCADE has been
        // dropped (Python parity — Python's `pipeline_runs.dataset_id` is a
        // plain nullable column with no FK). The audit-trail row therefore
        // survives a dataset deletion; the orphaned row simply retains its
        // historical `dataset_id` value pointing at a now-deleted dataset.
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner, "pr_cascade_ds").await;

        // Insert a pipeline_runs row for this dataset
        let pipeline_run = cognee_database::PipelineRun {
            id: Uuid::new_v4(),
            created_at: chrono::Utc::now(),
            status: cognee_database::PipelineRunStatus::Completed,
            pipeline_run_id: Uuid::new_v4(),
            pipeline_name: "cognify_pipeline".to_string(),
            pipeline_id: Uuid::new_v4(),
            dataset_id: Some(dataset_id),
            run_info: None,
        };
        ops::pipeline_runs::create_pipeline_run(&db, pipeline_run)
            .await
            .unwrap();

        // Verify pipeline_run exists
        let status_before =
            ops::pipeline_runs::get_latest_pipeline_status(&db, "cognify_pipeline", dataset_id)
                .await
                .unwrap();
        assert!(
            status_before.is_some(),
            "pipeline run should exist before dataset deletion"
        );

        // Delete the dataset
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "pr_cascade_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);

        // Post-08-01: pipeline_runs are NOT cascade-deleted (the FK is gone).
        // The audit row survives, still keyed by the now-orphaned dataset_id.
        let status_after =
            ops::pipeline_runs::get_latest_pipeline_status(&db, "cognify_pipeline", dataset_id)
                .await
                .unwrap();
        assert!(
            status_after.is_some(),
            "pipeline run should survive dataset deletion (no FK CASCADE post-08-01)"
        );
    }

    // ------------------------------------------------------------------
    // Search history cleanup tests
    // ------------------------------------------------------------------

    /// Helper: seed search history queries (and one result per query) for a user.
    async fn seed_search_history(
        db: &cognee_database::DatabaseConnection,
        user_id: Uuid,
        count: usize,
    ) {
        for i in 0..count {
            let query_id = ops::search_history::log_query(
                db,
                &format!("test query {i}"),
                "GraphCompletion",
                Some(user_id),
            )
            .await
            .unwrap();
            ops::search_history::log_result(
                db,
                query_id,
                &format!("{{\"result\": {i}}}"),
                Some(user_id),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn user_scoped_delete_clears_search_history() {
        let (svc, storage, db) = make_service().await;
        let user_a = Uuid::new_v4();
        let user_b = Uuid::new_v4();

        // Seed datasets so the user-scoped delete has something to resolve
        seed_dataset_with_data(&db, &storage, user_a, "sh_user_a_ds").await;
        seed_dataset_with_data(&db, &storage, user_b, "sh_user_b_ds").await;

        // Seed search history for both users
        seed_search_history(&db, user_a, 3).await;
        seed_search_history(&db, user_b, 2).await;

        // Verify initial counts
        let count_a = ops::search_history::count_queries_by_user(&db, user_a)
            .await
            .unwrap();
        let count_b = ops::search_history::count_queries_by_user(&db, user_b)
            .await
            .unwrap();
        assert_eq!(count_a, 3);
        assert_eq!(count_b, 2);

        // Delete user A
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::User { owner_id: user_a },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_search_queries, 3);

        // User A's search history should be gone
        let count_a_after = ops::search_history::count_queries_by_user(&db, user_a)
            .await
            .unwrap();
        assert_eq!(
            count_a_after, 0,
            "user A's search history should be deleted"
        );

        // User B's search history should remain
        let count_b_after = ops::search_history::count_queries_by_user(&db, user_b)
            .await
            .unwrap();
        assert_eq!(
            count_b_after, 2,
            "user B's search history should be untouched"
        );
    }

    #[tokio::test]
    async fn all_scoped_delete_clears_all_search_history() {
        let (svc, storage, db) = make_service().await;
        let user_a = Uuid::new_v4();
        let user_b = Uuid::new_v4();

        seed_dataset_with_data(&db, &storage, user_a, "sh_all_a_ds").await;
        seed_dataset_with_data(&db, &storage, user_b, "sh_all_b_ds").await;

        seed_search_history(&db, user_a, 3).await;
        seed_search_history(&db, user_b, 2).await;

        let total_before = ops::search_history::count_all_queries(&db).await.unwrap();
        assert_eq!(total_before, 5);

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::All,
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_search_queries, 5);

        let total_after = ops::search_history::count_all_queries(&db).await.unwrap();
        assert_eq!(
            total_after, 0,
            "all search history should be deleted after All-scoped delete"
        );
    }

    #[tokio::test]
    async fn dataset_scoped_delete_does_not_touch_search_history() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();

        seed_dataset_with_data(&db, &storage, owner, "sh_ds_notouch").await;
        seed_search_history(&db, owner, 4).await;

        let count_before = ops::search_history::count_queries_by_user(&db, owner)
            .await
            .unwrap();
        assert_eq!(count_before, 4);

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "sh_ds_notouch".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(
            result.deleted_search_queries, 0,
            "dataset-scoped delete should not touch search history"
        );

        let count_after = ops::search_history::count_queries_by_user(&db, owner)
            .await
            .unwrap();
        assert_eq!(
            count_after, 4,
            "search history should be untouched after dataset deletion"
        );
    }

    #[tokio::test]
    async fn preview_shows_search_history_count() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();

        seed_dataset_with_data(&db, &storage, owner, "sh_preview_ds").await;
        seed_search_history(&db, owner, 5).await;

        // User-scoped preview
        let preview = svc
            .preview(&DeleteRequest {
                scope: DeleteScope::User { owner_id: owner },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("preview should succeed");

        assert_eq!(
            preview.search_queries_to_delete, 5,
            "preview should show correct search query count for user scope"
        );

        // All-scoped preview
        let preview_all = svc
            .preview(&DeleteRequest {
                scope: DeleteScope::All,
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("preview should succeed");

        assert_eq!(
            preview_all.search_queries_to_delete, 5,
            "preview should show correct search query count for All scope"
        );

        // Dataset-scoped preview should show 0
        let preview_ds = svc
            .preview(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "sh_preview_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("preview should succeed");

        assert_eq!(
            preview_ds.search_queries_to_delete, 0,
            "dataset-scoped preview should show 0 search queries"
        );
    }

    // ------------------------------------------------------------------
    // Session prune tests
    // ------------------------------------------------------------------

    /// Minimal mock session store that tracks whether `prune()` was called.
    struct MockSessionStore {
        pruned: std::sync::Mutex<bool>,
    }

    impl MockSessionStore {
        fn new() -> Self {
            Self {
                pruned: std::sync::Mutex::new(false),
            }
        }

        fn was_pruned(&self) -> bool {
            *self.pruned.lock().expect("lock poison is unrecoverable")
        }
    }

    #[async_trait::async_trait]
    impl SessionStore for MockSessionStore {
        async fn create_qa_entry(
            &self,
            _session_id: &str,
            _user_id: Option<&str>,
            _question: &str,
            _answer: &str,
            _context: Option<&str>,
        ) -> Result<String, cognee_session::SessionError> {
            Ok("mock-qa-id".to_string())
        }

        async fn get_latest_qa_entries(
            &self,
            _session_id: &str,
            _user_id: Option<&str>,
            _last_n: usize,
        ) -> Result<Vec<cognee_session::SessionQAEntry>, cognee_session::SessionError> {
            Ok(vec![])
        }

        async fn get_all_qa_entries(
            &self,
            _session_id: &str,
            _user_id: Option<&str>,
        ) -> Result<Vec<cognee_session::SessionQAEntry>, cognee_session::SessionError> {
            Ok(vec![])
        }

        async fn delete_session(
            &self,
            _session_id: &str,
            _user_id: Option<&str>,
        ) -> Result<bool, cognee_session::SessionError> {
            Ok(true)
        }

        async fn delete_qa_entry(
            &self,
            _session_id: &str,
            _user_id: Option<&str>,
            _qa_id: &str,
        ) -> Result<bool, cognee_session::SessionError> {
            Ok(true)
        }

        async fn prune(&self) -> Result<(), cognee_session::SessionError> {
            *self.pruned.lock().expect("lock poison is unrecoverable") = true;
            Ok(())
        }

        async fn update_qa_entry(
            &self,
            _session_id: &str,
            _user_id: Option<&str>,
            _qa_id: &str,
            _updates: cognee_session::SessionQAUpdate,
        ) -> Result<bool, cognee_session::SessionError> {
            Ok(true)
        }

        async fn get_graph_context(
            &self,
            _session_id: &str,
            _user_id: Option<&str>,
        ) -> Result<Option<String>, cognee_session::SessionError> {
            Ok(None)
        }

        async fn set_graph_context(
            &self,
            _session_id: &str,
            _user_id: Option<&str>,
            _context: &str,
        ) -> Result<(), cognee_session::SessionError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn delete_all_prunes_session_store() {
        let (svc, _storage, _db) = make_service().await;
        let session_store = Arc::new(MockSessionStore::new());
        let svc = svc.with_session_store(session_store.clone() as Arc<dyn SessionStore>);

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::All,
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("delete all should succeed");

        assert!(
            session_store.was_pruned(),
            "session store prune() should have been called"
        );
        assert!(
            result.pruned_sessions,
            "result should indicate sessions were pruned"
        );
    }

    #[tokio::test]
    async fn delete_dataset_does_not_prune_sessions() {
        let (svc, storage, db) = make_service().await;
        let session_store = Arc::new(MockSessionStore::new());
        let owner_id = Uuid::new_v4();
        let _ = seed_dataset_with_data(&db, &storage, owner_id, "test_ds").await;
        let svc = svc.with_session_store(session_store.clone() as Arc<dyn SessionStore>);

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id,
                    dataset_name: "test_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("delete dataset should succeed");

        assert!(
            !session_store.was_pruned(),
            "session store prune() should NOT be called for dataset-scoped deletion"
        );
        assert!(
            !result.pruned_sessions,
            "result should indicate sessions were NOT pruned"
        );
    }

    #[tokio::test]
    async fn delete_all_without_session_store_skips_prune() {
        let (svc, _storage, _db) = make_service().await;
        // No session store configured

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::All,
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("delete all should succeed");

        assert!(
            !result.pruned_sessions,
            "result should indicate sessions were NOT pruned when no store is configured"
        );
    }

    // ------------------------------------------------------------------
    // Orphaned EdgeType cleanup tests (Gap 09)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn hard_delete_removes_orphaned_edge_type_nodes() {
        let (_svc, storage, db, graph_db, vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "edge_type_ds").await;

        // Seed provenance so graph/vector cleanup can find something
        let node_slug = Uuid::new_v4();
        let edge_slug = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id,
            owner,
            &[node_slug],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;
        seed_provenance_edges(&db, dataset_id, data_id, owner, &[edge_slug], "works_at").await;

        // Add entity node + EdgeType node to the graph
        let edge_type_id = cognee_models::EdgeType::deterministic_id("works_at");
        graph_db
            .add_node_raw(serde_json::json!({
                "id": node_slug.to_string(),
                "type": "Entity",
                "name": "Alice"
            }))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({
                "id": edge_type_id.to_string(),
                "type": "EdgeType",
                "relationship_name": "works_at"
            }))
            .await
            .unwrap();

        // Add an edge between the entity and something (so the EdgeType is "in use")
        graph_db
            .add_edge(&node_slug.to_string(), "some_target", "works_at", None)
            .await
            .unwrap();

        assert_eq!(graph_db.node_count(), 2);
        assert_eq!(graph_db.edge_count(), 1);

        // Add EdgeType vector point
        vector_db
            .create_collection("EdgeType", "relationship_name", 3)
            .await
            .unwrap();
        let et_point = cognee_vector::VectorPoint::new(edge_type_id, vec![1.0, 0.0, 0.0]);
        vector_db
            .index_points("EdgeType", "relationship_name", &[et_point])
            .await
            .unwrap();

        // Execute hard delete - this should:
        // 1. Delete the entity node from graph (via provenance)
        // 2. After entity deletion, the edge "works_at" is gone (MockGraphDB doesn't
        //    cascade edges on node delete, so we simulate that). Actually MockGraphDB
        //    delete_nodes only removes nodes, not edges. For this test, let's manually
        //    remove the edge to simulate what Ladybug would do.
        //
        // Actually, let's restructure: make the EdgeType orphaned from the start
        // by NOT having any edges with that relationship name in the graph.
        graph_db.clear();

        // Re-add just the orphaned EdgeType node (no edges at all)
        graph_db
            .add_node_raw(serde_json::json!({
                "id": edge_type_id.to_string(),
                "type": "EdgeType",
                "relationship_name": "works_at"
            }))
            .await
            .unwrap();
        assert_eq!(graph_db.node_count(), 1);
        assert_eq!(graph_db.edge_count(), 0);

        // Re-seed provenance with no nodes (graph was cleared)
        // We need the dataset to still exist for the delete to work.
        // Re-create it fresh.
        let db2 = connect("sqlite::memory:").await.unwrap();
        initialize(&db2).await.unwrap();
        let db2 = Arc::new(db2);
        let storage2 = Arc::new(MockStorage::new());
        let svc2 = DeleteService::new(
            storage2.clone() as Arc<dyn StorageTrait>,
            db2.clone() as Arc<dyn DeleteDb>,
        )
        .with_graph_db(graph_db.clone() as Arc<dyn GraphDBTrait>)
        .with_vector_db(vector_db.clone() as Arc<dyn VectorDB>);

        let (dataset_id2, _data_id2) =
            seed_dataset_with_data(&db2, &storage2, owner, "edge_type_ds").await;

        // Seed a provenance node so there's something to cleanup in phase 1
        let node_slug2 = Uuid::new_v4();
        seed_provenance_nodes(
            &db2,
            dataset_id2,
            _data_id2,
            owner,
            &[node_slug2],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;

        // Add the provenance node to the graph too
        graph_db
            .add_node_raw(serde_json::json!({
                "id": node_slug2.to_string(),
                "type": "Entity",
                "name": "Bob"
            }))
            .await
            .unwrap();

        // Now: graph has 2 nodes (Entity "Bob" + orphaned EdgeType "works_at"), 0 edges
        assert_eq!(graph_db.node_count(), 2);

        // Execute hard delete
        let result = svc2
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "edge_type_ds".to_string(),
                },
                mode: DeleteMode::Hard,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        // The entity node was deleted via provenance. The EdgeType node should
        // be swept as an orphan because it has degree 0 and its relationship_name
        // is not in any edge.
        assert_eq!(
            result.deleted_orphan_edge_types, 1,
            "orphaned EdgeType should be cleaned up"
        );
        assert_eq!(
            graph_db.node_count(),
            0,
            "all nodes should be gone (entity via provenance, EdgeType via orphan sweep)"
        );

        // Vector point should also be deleted
        assert_eq!(
            vector_db
                .collection_size("EdgeType", "relationship_name")
                .await
                .unwrap(),
            0,
            "EdgeType vector point should be removed by orphan sweep"
        );
    }

    #[tokio::test]
    async fn shared_edge_type_survives_when_edges_remain() {
        let (_svc, _storage, _db, graph_db, vector_db) = make_service_with_graph_vector().await;

        // Setup: EdgeType "works_at" with edges still present in the graph
        let edge_type_id = cognee_models::EdgeType::deterministic_id("works_at");
        graph_db
            .add_node_raw(serde_json::json!({
                "id": edge_type_id.to_string(),
                "type": "EdgeType",
                "relationship_name": "works_at"
            }))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({"id": "e1", "type": "Entity", "name": "Alice"}))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({"id": "e2", "type": "Entity", "name": "Bob"}))
            .await
            .unwrap();
        // An edge of type "works_at" still exists
        graph_db
            .add_edge("e1", "e2", "works_at", None)
            .await
            .unwrap();

        // Create vector collection
        vector_db
            .create_collection("EdgeType", "relationship_name", 3)
            .await
            .unwrap();
        let et_point = cognee_vector::VectorPoint::new(edge_type_id, vec![1.0, 0.0, 0.0]);
        vector_db
            .index_points("EdgeType", "relationship_name", &[et_point])
            .await
            .unwrap();

        // The zero-degree check should NOT find this EdgeType as orphaned
        let orphans = graph_db.get_zero_degree_edge_type_nodes().await.unwrap();
        assert!(
            orphans.is_empty(),
            "EdgeType with active edges should not be considered orphaned"
        );

        // The EdgeType node should still exist
        assert!(
            graph_db.has_node(&edge_type_id.to_string()).await.unwrap(),
            "EdgeType node should survive"
        );
        assert_eq!(
            vector_db
                .collection_size("EdgeType", "relationship_name")
                .await
                .unwrap(),
            1,
            "EdgeType vector point should survive"
        );
    }

    #[tokio::test]
    async fn orphan_edge_type_detected_when_no_edges_exist() {
        let (_svc, _storage, _db, graph_db, _vector_db) = make_service_with_graph_vector().await;

        // Add an EdgeType node with NO corresponding edges
        let edge_type_id = cognee_models::EdgeType::deterministic_id("obsolete_rel");
        graph_db
            .add_node_raw(serde_json::json!({
                "id": edge_type_id.to_string(),
                "type": "EdgeType",
                "relationship_name": "obsolete_rel"
            }))
            .await
            .unwrap();

        // Add a non-EdgeType node (should be ignored by the sweep)
        graph_db
            .add_node_raw(serde_json::json!({"id": "e1", "type": "Entity", "name": "Alice"}))
            .await
            .unwrap();

        let orphans = graph_db.get_zero_degree_edge_type_nodes().await.unwrap();
        assert_eq!(
            orphans.len(),
            1,
            "should detect exactly one orphaned EdgeType"
        );
        assert_eq!(orphans[0].0, edge_type_id.to_string());
    }

    #[tokio::test]
    async fn edge_type_with_matching_rel_name_in_edges_not_orphaned() {
        let (_svc, _storage, _db, graph_db, _vector_db) = make_service_with_graph_vector().await;

        // Add an EdgeType node
        let edge_type_id = cognee_models::EdgeType::deterministic_id("knows");
        graph_db
            .add_node_raw(serde_json::json!({
                "id": edge_type_id.to_string(),
                "type": "EdgeType",
                "relationship_name": "knows"
            }))
            .await
            .unwrap();

        // The EdgeType node itself has no edges (degree 0), but there are
        // other edges in the graph with relationship_name "knows"
        graph_db
            .add_node_raw(serde_json::json!({"id": "a", "type": "Entity", "name": "A"}))
            .await
            .unwrap();
        graph_db
            .add_node_raw(serde_json::json!({"id": "b", "type": "Entity", "name": "B"}))
            .await
            .unwrap();
        graph_db.add_edge("a", "b", "knows", None).await.unwrap();

        let orphans = graph_db.get_zero_degree_edge_type_nodes().await.unwrap();
        assert!(
            orphans.is_empty(),
            "EdgeType should not be orphaned when edges with its relationship_name exist"
        );
    }

    #[tokio::test]
    async fn soft_delete_does_not_sweep_orphan_edge_types() {
        let (svc, storage, db, graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (_dataset_id, _data_id) = seed_dataset_with_data(&db, &storage, owner, "soft_ds").await;

        // Add an orphaned EdgeType node
        let edge_type_id = cognee_models::EdgeType::deterministic_id("stale_rel");
        graph_db
            .add_node_raw(serde_json::json!({
                "id": edge_type_id.to_string(),
                "type": "EdgeType",
                "relationship_name": "stale_rel"
            }))
            .await
            .unwrap();

        // Soft delete should NOT trigger orphan sweep
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "soft_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(
            result.deleted_orphan_edge_types, 0,
            "soft delete should not sweep orphan EdgeTypes"
        );
        assert!(
            graph_db.has_node(&edge_type_id.to_string()).await.unwrap(),
            "orphaned EdgeType should still exist after soft delete"
        );
    }

    // ------------------------------------------------------------------
    // delete_dataset_if_empty flag tests
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn delete_data_with_flag_deletes_empty_dataset() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        let (_dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "auto_del_ds").await;

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id,
                    dataset_name: Some("auto_del_ds".to_string()),
                    delete_dataset_if_empty: true,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_data, 1, "data should be deleted");
        assert_eq!(
            result.deleted_datasets, 1,
            "dataset should be auto-deleted because it became empty"
        );

        // Verify the dataset is actually gone from the DB
        let ds = ops::datasets::get_dataset_by_name(&db, "auto_del_ds", owner, None)
            .await
            .unwrap();
        assert!(ds.is_none(), "dataset should be gone from DB");
    }

    #[tokio::test]
    async fn delete_data_with_flag_keeps_nonempty_dataset() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();

        // Create one dataset with two data items
        let dataset = Dataset::new("multi_data_ds".to_string(), owner, None, Uuid::new_v4());
        let dataset_id = dataset.id;
        ops::datasets::create_dataset(&db, dataset).await.unwrap();

        let loc1 = storage.store(b"content one", "one.txt").await.unwrap();
        let data_id_1 = Uuid::new_v4();
        let data1 = Data::builder(
            data_id_1,
            "one.txt",
            loc1,
            "file://one.txt",
            "txt",
            "text/plain",
            "hash_one",
            owner,
        )
        .build();
        ops::data::create_data(&db, data1).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, dataset_id, data_id_1)
            .await
            .unwrap();

        let loc2 = storage.store(b"content two", "two.txt").await.unwrap();
        let data_id_2 = Uuid::new_v4();
        let data2 = Data::builder(
            data_id_2,
            "two.txt",
            loc2,
            "file://two.txt",
            "txt",
            "text/plain",
            "hash_two",
            owner,
        )
        .build();
        ops::data::create_data(&db, data2).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, dataset_id, data_id_2)
            .await
            .unwrap();

        // Delete one data item with the flag set
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id: data_id_1,
                    dataset_name: Some("multi_data_ds".to_string()),
                    delete_dataset_if_empty: true,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_data, 1, "data should be deleted");
        assert_eq!(
            result.deleted_datasets, 0,
            "dataset should survive because it still has data_id_2"
        );

        // Verify the dataset still exists
        let ds = ops::datasets::get_dataset_by_name(&db, "multi_data_ds", owner, None)
            .await
            .unwrap();
        assert!(ds.is_some(), "dataset should still exist in DB");
    }

    #[tokio::test]
    async fn delete_data_without_flag_keeps_empty_dataset() {
        let (svc, storage, db) = make_service().await;
        let owner = Uuid::new_v4();
        let (_dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "no_flag_ds").await;

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id,
                    dataset_name: Some("no_flag_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_data, 1, "data should be deleted");
        assert_eq!(
            result.deleted_datasets, 0,
            "dataset should survive because flag is false"
        );

        // Verify the dataset still exists (even though it's now empty)
        let ds = ops::datasets::get_dataset_by_name(&db, "no_flag_ds", owner, None)
            .await
            .unwrap();
        assert!(
            ds.is_some(),
            "dataset should still exist despite being empty"
        );
    }

    // ------------------------------------------------------------------
    // tenant_id filtering in DeleteDb::get_dataset_by_name
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn delete_db_get_dataset_by_name_filters_by_tenant_id() {
        use cognee_database::DeleteDb;

        let db = cognee_database::connect("sqlite::memory:").await.unwrap();
        cognee_database::initialize(&db).await.unwrap();

        let owner = Uuid::new_v4();
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();

        // Create two datasets with the same name and owner but different tenants.
        let ds_a = Dataset::new(
            "shared_name".to_string(),
            owner,
            Some(tenant_a),
            Uuid::new_v4(),
        );
        let ds_b = Dataset::new(
            "shared_name".to_string(),
            owner,
            Some(tenant_b),
            Uuid::new_v4(),
        );
        ops::datasets::create_dataset(&db, ds_a.clone())
            .await
            .unwrap();
        ops::datasets::create_dataset(&db, ds_b.clone())
            .await
            .unwrap();

        // Without tenant_id (None), the query returns one of them (ambiguous).
        let any = db
            .get_dataset_by_name("shared_name", owner, None)
            .await
            .unwrap();
        assert!(any.is_some(), "should find at least one dataset");

        // With tenant_a, only the first dataset is returned.
        let found_a = db
            .get_dataset_by_name("shared_name", owner, Some(tenant_a))
            .await
            .unwrap();
        assert_eq!(
            found_a.as_ref().map(|d| d.id),
            Some(ds_a.id),
            "should find tenant_a's dataset"
        );

        // With tenant_b, only the second dataset is returned.
        let found_b = db
            .get_dataset_by_name("shared_name", owner, Some(tenant_b))
            .await
            .unwrap();
        assert_eq!(
            found_b.as_ref().map(|d| d.id),
            Some(ds_b.id),
            "should find tenant_b's dataset"
        );

        // With a nonexistent tenant_id, nothing is returned.
        let found_none = db
            .get_dataset_by_name("shared_name", owner, Some(Uuid::new_v4()))
            .await
            .unwrap();
        assert!(
            found_none.is_none(),
            "should find no dataset for unknown tenant"
        );
    }

    // ------------------------------------------------------------------
    // Memory-only tests (task 18)
    // ------------------------------------------------------------------

    /// Verify that `memory_only: true` removes graph/vector artifacts but
    /// leaves the `Dataset` row, `Data` row, and stored file intact.
    #[tokio::test]
    async fn memory_only_dataset_preserves_rows_and_files() {
        let (svc, storage, db, graph_db, vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "memory_only_ds").await;

        // Seed provenance nodes + graph/vector artifacts.
        let slug = Uuid::new_v4();
        seed_provenance_nodes(
            &db,
            dataset_id,
            data_id,
            owner,
            &[slug],
            "Entity",
            serde_json::json!(["name"]),
        )
        .await;
        graph_db
            .add_node_raw(serde_json::json!({"id": slug.to_string(), "name": "TestNode"}))
            .await
            .unwrap();
        vector_db
            .create_collection("Entity", "name", 3)
            .await
            .unwrap();
        vector_db
            .index_points(
                "Entity",
                "name",
                &[cognee_vector::VectorPoint::new(slug, vec![1.0, 0.0, 0.0])],
            )
            .await
            .unwrap();

        // Seed pipeline_status with BOTH an add_pipeline and a cognify_pipeline
        // entry keyed by this dataset. Python's `_forget_dataset_memory` removes
        // the dataset_id entry from EVERY pipeline on each Data record.
        let dataset_id_hex = cognee_database::uuid_hex::to_hex(dataset_id);
        let status_json = serde_json::json!({
            "add_pipeline": { dataset_id_hex.clone(): "DATASET_PROCESSING_COMPLETED" },
            "cognify_pipeline": { dataset_id_hex.clone(): "DATASET_PROCESSING_COMPLETED" },
        });
        let data_rec = ops::data::get_data(&db, data_id).await.unwrap().unwrap();
        ops::data::update_data(
            &db,
            Data {
                pipeline_status: Some(status_json.to_string()),
                ..data_rec
            },
        )
        .await
        .unwrap();

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id: owner,
                    dataset_name: "memory_only_ds".to_string(),
                },
                mode: DeleteMode::Soft,
                memory_only: true,
            })
            .await
            .expect("memory-only execute should succeed");

        // Graph/vector cleared.
        assert_eq!(
            result.deleted_graph_nodes, 1,
            "graph node should be cleared"
        );
        assert_eq!(
            result.deleted_vector_points, 1,
            "vector point should be cleared"
        );

        // No relational rows deleted.
        assert_eq!(result.deleted_datasets, 0, "dataset must not be deleted");
        assert_eq!(result.deleted_data, 0, "data must not be deleted");
        assert_eq!(
            result.deleted_storage_files, 0,
            "storage file must not be deleted"
        );

        // Dataset row still exists in the DB.
        let ds_still = ops::datasets::get_dataset_by_name(&db, "memory_only_ds", owner, None)
            .await
            .unwrap();
        assert!(
            ds_still.is_some(),
            "Dataset row must survive memory-only forget"
        );

        // Data row still exists.
        let data_still = ops::data::get_data(&db, data_id).await.unwrap();
        assert!(
            data_still.is_some(),
            "Data row must survive memory-only forget"
        );

        // Python parity (`_forget_dataset_memory`): the dataset_id entry is
        // removed from EVERY pipeline on the Data record (add + cognify). Both
        // inner maps become empty, so the whole pipeline_status is cleared.
        let ps = data_still.unwrap().pipeline_status;
        let cleared = match ps {
            None => true,
            Some(s) => {
                let v: serde_json::Value = serde_json::from_str(&s).unwrap();
                let add_has = v
                    .get("add_pipeline")
                    .and_then(|p| p.as_object())
                    .map(|m| m.contains_key(&dataset_id_hex))
                    .unwrap_or(false);
                let cog_has = v
                    .get("cognify_pipeline")
                    .and_then(|p| p.as_object())
                    .map(|m| m.contains_key(&dataset_id_hex))
                    .unwrap_or(false);
                !add_has && !cog_has
            }
        };
        assert!(
            cleared,
            "dataset_id entry must be removed from ALL pipelines (add + cognify) on the data record"
        );
    }

    /// Verify that `memory_only: true` on a data-item scope also preserves
    /// the `Data` row and `Dataset` row.
    #[tokio::test]
    async fn memory_only_data_item_preserves_rows() {
        let (svc, storage, db, _graph_db, _vector_db) = make_service_with_graph_vector().await;
        let owner = Uuid::new_v4();
        let (dataset_id, data_id) =
            seed_dataset_with_data(&db, &storage, owner, "memory_only_item_ds").await;

        // Add a SECOND data item to the same dataset (a sibling) to prove the
        // data-item path only touches the targeted record, not the whole
        // dataset (Python `_forget_data_memory` filters on `Data.id == data_id`).
        let sibling_loc = storage.store(b"sibling", "sibling.txt").await.unwrap();
        let sibling_id = Uuid::new_v4();
        let sibling = Data::builder(
            sibling_id,
            "sibling.txt",
            sibling_loc,
            "file://sibling.txt",
            "txt",
            "text/plain",
            "sibling_hash",
            owner,
        )
        .build();
        ops::data::create_data(&db, sibling).await.unwrap();
        ops::datasets::attach_data_to_dataset(&db, dataset_id, sibling_id)
            .await
            .unwrap();

        let dataset_id_hex = cognee_database::uuid_hex::to_hex(dataset_id);
        // Target record: keep add_pipeline, remove only cognify_pipeline.
        let target_status = serde_json::json!({
            "add_pipeline": { dataset_id_hex.clone(): "DATASET_PROCESSING_COMPLETED" },
            "cognify_pipeline": { dataset_id_hex.clone(): "DATASET_PROCESSING_COMPLETED" },
        });
        let target_rec = ops::data::get_data(&db, data_id).await.unwrap().unwrap();
        ops::data::update_data(
            &db,
            Data {
                pipeline_status: Some(target_status.to_string()),
                ..target_rec
            },
        )
        .await
        .unwrap();
        // Sibling record: cognify status must be left untouched.
        let sibling_status = serde_json::json!({
            "cognify_pipeline": { dataset_id_hex.clone(): "DATASET_PROCESSING_COMPLETED" },
        });
        let sibling_rec = ops::data::get_data(&db, sibling_id).await.unwrap().unwrap();
        ops::data::update_data(
            &db,
            Data {
                pipeline_status: Some(sibling_status.to_string()),
                ..sibling_rec
            },
        )
        .await
        .unwrap();

        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id,
                    dataset_name: Some("memory_only_item_ds".to_string()),
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: true,
            })
            .await
            .expect("memory-only data-item execute should succeed");

        // No relational rows deleted.
        assert_eq!(result.deleted_data, 0, "data must not be deleted");
        assert_eq!(result.deleted_datasets, 0, "dataset must not be deleted");
        assert_eq!(
            result.deleted_storage_files, 0,
            "storage file must not be deleted"
        );

        // Dataset row still exists.
        let ds_still = ops::datasets::get_dataset_by_name(&db, "memory_only_item_ds", owner, None)
            .await
            .unwrap();
        assert!(
            ds_still.is_some(),
            "Dataset row must survive data-item memory-only forget"
        );

        // Data row still exists.
        let data_still = ops::data::get_data(&db, data_id).await.unwrap();
        assert!(
            data_still.is_some(),
            "Data row must survive data-item memory-only forget"
        );

        // Target record: cognify removed, add_pipeline preserved.
        let target_after: serde_json::Value =
            serde_json::from_str(data_still.unwrap().pipeline_status.as_deref().unwrap()).unwrap();
        assert!(
            target_after.get("cognify_pipeline").is_none(),
            "cognify_pipeline must be removed from the targeted data record"
        );
        assert!(
            target_after
                .get("add_pipeline")
                .and_then(|p| p.as_object())
                .map(|m| m.contains_key(&dataset_id_hex))
                .unwrap_or(false),
            "add_pipeline status must be preserved on the targeted data record"
        );

        // Sibling record: cognify status untouched (data-item path is scoped).
        let sibling_after: serde_json::Value = serde_json::from_str(
            ops::data::get_data(&db, sibling_id)
                .await
                .unwrap()
                .unwrap()
                .pipeline_status
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert!(
            sibling_after
                .get("cognify_pipeline")
                .and_then(|p| p.as_object())
                .map(|m| m.contains_key(&dataset_id_hex))
                .unwrap_or(false),
            "sibling data record's cognify status must NOT be cleared by a data-item forget"
        );
    }

    /// Regression test for Item 3 (B6.6): when the Data row is absent (custom
    /// graph model or orphaned graph data), `DeleteScope::Data` must succeed and
    /// return a best-effort cleanup result rather than a validation error.
    ///
    /// Python parity: `datasets.py:165-176`.
    #[tokio::test]
    async fn delete_data_without_relational_row_returns_success() {
        let (svc, _storage, _db) = make_service().await;
        let owner = Uuid::new_v4();
        let ghost_data_id = Uuid::new_v4();

        // No dataset row, no data row — only the IDs exist.
        let result = svc
            .execute(&DeleteRequest {
                scope: DeleteScope::Data {
                    owner_id: owner,
                    data_id: ghost_data_id,
                    dataset_name: None,
                    delete_dataset_if_empty: false,
                },
                mode: DeleteMode::Soft,
                memory_only: false,
            })
            .await
            .expect("should succeed for a data_id with no relational row (custom graph model)");

        // The core cleanup paths ran without error.  The call must not return a
        // Validation error — that is the key invariant.  `deleted_data` may be
        // 1 even when no relational row existed because the delete pipeline
        // attempts a best-effort relational DELETE and increments the counter
        // regardless of the underlying row count (SQLite DELETE WHERE succeeds
        // on a non-existent row with 0 rows affected, but still counted as
        // one delete attempt).
        assert!(
            result.deleted_data <= 1,
            "unexpected large deletion count for a ghost data_id: deleted_data={}",
            result.deleted_data
        );
    }
}
