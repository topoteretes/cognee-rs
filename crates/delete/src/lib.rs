mod authorized;

pub use authorized::AuthorizedDeleteService;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cognee_database::{ArtifactReference, DeleteDb, GraphEdge, GraphNode};
use cognee_graph::GraphDBTrait;
use cognee_models::Dataset;
use cognee_storage::{StorageError, StorageTrait};
use cognee_vector::VectorDB;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;
use uuid::Uuid;

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
    pub deleted_pipeline_runs: usize,
    pub cleared_pipeline_statuses: usize,
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
}

impl DeleteService {
    pub fn new(storage: Arc<dyn StorageTrait>, database: Arc<dyn DeleteDb>) -> Self {
        Self {
            storage,
            database,
            graph_db: None,
            vector_db: None,
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

    pub async fn preview(&self, request: &DeleteRequest) -> Result<DeletePreview, DeleteError> {
        let targets = self.resolve_targets(request).await?;
        let data_to_delete = self
            .count_data_that_would_be_deleted(&targets.candidate_data_ids, &targets.links_to_detach)
            .await?;

        // Count graph nodes, vector points, and provenance rows from tables
        let (graph_node_count, vector_point_count, prov_node_count, prov_edge_count) =
            self.count_graph_vector_artifacts(request, &targets).await?;

        Ok(DeletePreview {
            datasets_to_delete: targets.datasets_to_delete.len(),
            dataset_links_to_delete: targets.links_to_detach.len(),
            data_to_delete,
            storage_files_to_delete: data_to_delete,
            graph_nodes_to_delete: graph_node_count,
            vector_points_to_delete: vector_point_count,
            provenance_nodes_to_delete: prov_node_count,
            provenance_edges_to_delete: prov_edge_count,
        })
    }

    pub async fn execute(&self, request: &DeleteRequest) -> Result<DeleteResult, DeleteError> {
        let targets = self.resolve_targets(request).await?;

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
            let (gn, vp, pn, pe, gv_warnings) = self.cleanup_all().await?;
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

        // ------------------------------------------------------------------
        // Phase 3: Hard-mode orphan sweep (degree-one Entity/EntityType nodes)
        // ------------------------------------------------------------------

        let mut deleted_orphan_entities = 0usize;
        let mut deleted_orphan_entity_types = 0usize;

        if matches!(request.mode, DeleteMode::Hard) {
            let (oe, oet, sweep_warnings) = self.sweep_orphan_nodes().await?;
            deleted_orphan_entities = oe;
            deleted_orphan_entity_types = oet;
            warnings.extend(sweep_warnings);
        }

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
            deleted_pipeline_runs,
            cleared_pipeline_statuses,
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

    pub async fn artifact_references_for_request(
        &self,
        request: &DeleteRequest,
    ) -> Result<Vec<ArtifactReference>, DeleteError> {
        let targets = self.resolve_targets(request).await?;
        let deletable_data_ids = self.data_ids_to_delete(request).await?;

        let mut references = Vec::new();
        let mut seen_ids = HashSet::new();

        for data_id in deletable_data_ids {
            let data_refs = self
                .database
                .list_artifact_references_for_data(data_id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to list artifact references for data {}: {}",
                        data_id, error
                    ))
                })?;
            for reference in data_refs {
                if seen_ids.insert(reference.id) {
                    references.push(reference);
                }
            }
        }

        for dataset in &targets.datasets_to_delete {
            let dataset_refs = self
                .database
                .list_artifact_references_for_dataset(dataset.id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to list artifact references for dataset {}: {}",
                        dataset.id, error
                    ))
                })?;
            for reference in dataset_refs {
                if seen_ids.insert(reference.id) {
                    references.push(reference);
                }
            }
        }

        Ok(references)
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

            // Edges contribute to EdgeType_relationship_name and Triplet_text
            for edge in edges {
                by_collection
                    .entry(("EdgeType".to_string(), "relationship_name".to_string()))
                    .or_default()
                    .push(edge.slug);

                // Triplet ID is the edge slug as well (matching cognify pipeline)
                by_collection
                    .entry(("Triplet".to_string(), "text".to_string()))
                    .or_default()
                    .push(edge.slug);
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

    /// Count graph nodes, vector points, and provenance rows that would be
    /// affected. Returns `(graph_nodes, vector_points, prov_nodes, prov_edges)`.
    async fn count_graph_vector_artifacts(
        &self,
        request: &DeleteRequest,
        targets: &ResolvedDeleteTargets,
    ) -> Result<(usize, usize, usize, usize), DeleteError> {
        let mut graph_nodes = 0usize;
        let mut vector_points = 0usize;
        let mut prov_nodes = 0usize;
        let mut prov_edges = 0usize;

        if matches!(request.scope, DeleteScope::All) {
            // For All scope we cannot provide exact counts without graph_db
            // inspection. Return 0 for now (the preview focuses on relational
            // counts).
            return Ok((0, 0, 0, 0));
        }

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
                        "Failed to count dataset links for data {}: {}",
                        data_id, error
                    ))
                })?;
            let to_remove = links_to_remove_per_data.get(data_id).copied().unwrap_or(0);
            if link_count <= to_remove {
                deletable.push(*data_id);
            }
        }

        Ok(deletable)
    }

    async fn resolve_targets(
        &self,
        request: &DeleteRequest,
    ) -> Result<ResolvedDeleteTargets, DeleteError> {
        match &request.scope {
            DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name,
            } => {
                self.resolve_data_scope(*owner_id, *data_id, dataset_name.as_deref())
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

    async fn resolve_data_scope(
        &self,
        owner_id: Uuid,
        data_id: Uuid,
        dataset_name: Option<&str>,
    ) -> Result<ResolvedDeleteTargets, DeleteError> {
        let data = self.database.get_data(data_id).await.map_err(|error| {
            DeleteError::Runtime(format!("Failed to fetch data {data_id}: {error}"))
        })?;

        let data =
            data.ok_or_else(|| DeleteError::Validation(format!("Data {data_id} was not found")))?;
        if data.owner_id != owner_id {
            return Err(DeleteError::Validation(format!(
                "Data {data_id} does not belong to owner {}",
                owner_id
            )));
        }

        let mut links_to_detach = Vec::new();
        if let Some(dataset_name) = dataset_name {
            let dataset = self
                .database
                .get_dataset_by_name(dataset_name, owner_id)
                .await
                .map_err(|error| {
                    DeleteError::Runtime(format!(
                        "Failed to resolve dataset '{dataset_name}': {error}"
                    ))
                })?
                .ok_or_else(|| {
                    DeleteError::Validation(format!(
                        "Dataset '{}' was not found for owner {}",
                        dataset_name, owner_id
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
                }
            }

            if links_to_detach.is_empty() {
                return Err(DeleteError::Validation(format!(
                    "No dataset links found for data {} and owner {}",
                    data_id, owner_id
                )));
            }
        }

        Ok(ResolvedDeleteTargets {
            datasets_to_delete: vec![],
            links_to_detach,
            candidate_data_ids: vec![data_id],
        })
    }

    async fn resolve_dataset_scope(
        &self,
        owner_id: Uuid,
        dataset_name: &str,
    ) -> Result<ResolvedDeleteTargets, DeleteError> {
        let dataset = self
            .database
            .get_dataset_by_name(dataset_name, owner_id)
            .await
            .map_err(|error| {
                DeleteError::Runtime(format!(
                    "Failed to resolve dataset '{dataset_name}': {error}"
                ))
            })?
            .ok_or_else(|| {
                DeleteError::Validation(format!(
                    "Dataset '{}' was not found for owner {}",
                    dataset_name, owner_id
                ))
            })?;

        self.resolve_dataset_list(vec![dataset]).await
    }

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
                        "Failed to count dataset links for data {}: {}",
                        data_id, error
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

#[cfg(test)]
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
        let edge_slug = Uuid::new_v4();
        seed_provenance_edges(&db, dataset_id, data_id, owner, &[edge_slug], "knows").await;

        // Create EdgeType and Triplet collections and index points
        vector_db
            .create_collection("EdgeType", "relationship_name", 3)
            .await
            .unwrap();
        vector_db
            .create_collection("Triplet", "text", 3)
            .await
            .unwrap();

        let et_point = cognee_vector::VectorPoint::new(edge_slug, vec![1.0, 0.0, 0.0]);
        vector_db
            .index_points("EdgeType", "relationship_name", &[et_point])
            .await
            .unwrap();
        let triplet_point = cognee_vector::VectorPoint::new(edge_slug, vec![0.0, 1.0, 0.0]);
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
                },
                mode: DeleteMode::Soft,
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
                },
                mode: DeleteMode::Soft,
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
                },
                mode: DeleteMode::Soft,
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
    async fn make_authorized_service() -> (
        AuthorizedDeleteService,
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
        let auth_svc = AuthorizedDeleteService::new(
            svc,
            db.clone() as Arc<dyn cognee_database::AclDb>,
            db.clone() as Arc<dyn DeleteDb>,
        );
        (auth_svc, storage, db)
    }

    /// Grant all four permissions (read, write, delete, share) to the owner
    /// on a dataset, matching what the ingestion pipeline would do.
    async fn grant_all_perms(
        db: &cognee_database::DatabaseConnection,
        owner_id: Uuid,
        dataset_id: Uuid,
    ) {
        ops::acl::grant_all_permissions_on_dataset(db, owner_id, dataset_id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn authorized_delete_succeeds_with_permission() {
        let (svc, storage, db) = make_authorized_service().await;
        let owner = Uuid::new_v4();
        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner, "acl_ok_ds").await;

        grant_all_perms(&db, owner, dataset_id).await;

        let result = svc
            .execute(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner,
                        dataset_name: "acl_ok_ds".to_string(),
                    },
                    mode: DeleteMode::Soft,
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
        let (svc, storage, db) = make_authorized_service().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "acl_fail_ds").await;

        // Do NOT grant any permissions.
        // Ensure the principal exists but without delete permission.
        ops::acl::ensure_principal(&db, owner, "user")
            .await
            .unwrap();

        let err = svc
            .execute(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner,
                        dataset_name: "acl_fail_ds".to_string(),
                    },
                    mode: DeleteMode::Soft,
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
        let (svc, storage, db) = make_authorized_service().await;
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();
        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner_a, "acl_wrong_principal").await;

        // Grant permissions to owner_a only
        grant_all_perms(&db, owner_a, dataset_id).await;
        // Ensure owner_b exists as principal but has no permissions
        ops::acl::ensure_principal(&db, owner_b, "user")
            .await
            .unwrap();

        let err = svc
            .execute(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner_a,
                        dataset_name: "acl_wrong_principal".to_string(),
                    },
                    mode: DeleteMode::Soft,
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
        let (svc, storage, db) = make_authorized_service().await;
        let owner_a = Uuid::new_v4();
        let user_b = Uuid::new_v4();
        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner_a, "acl_delegated").await;

        // Owner A gets all permissions
        grant_all_perms(&db, owner_a, dataset_id).await;

        // Grant "delete" to user B (delegated access)
        ops::acl::ensure_principal(&db, user_b, "user")
            .await
            .unwrap();
        ops::acl::grant_permission(&db, user_b, dataset_id, "delete")
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
                },
                user_b,
            )
            .await
            .expect("delegated delete should succeed");

        assert_eq!(result.deleted_datasets, 1);
    }

    #[tokio::test]
    async fn delete_cascades_acl_entries() {
        // Verify that deleting a dataset via FK CASCADE also removes ACL rows.
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let owner = Uuid::new_v4();
        let storage = MockStorage::new();

        let (dataset_id, _data_id) =
            seed_dataset_with_data(&db, &storage, owner, "cascade_ds").await;
        grant_all_perms(&db, owner, dataset_id).await;

        // Verify ACLs exist
        let has_delete = ops::acl::has_permission(&db, owner, dataset_id, "delete")
            .await
            .unwrap();
        assert!(has_delete, "should have delete permission before cascade");

        // Delete the dataset directly (bypasses DeleteService to test FK cascade)
        ops::datasets::delete_dataset(&db, dataset_id)
            .await
            .unwrap();

        // ACL rows should be gone (FK CASCADE on dataset_id)
        let has_delete_after = ops::acl::has_permission(&db, owner, dataset_id, "delete")
            .await
            .unwrap();
        assert!(
            !has_delete_after,
            "ACL entries should be cascade-deleted with the dataset"
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
            })
            .await
            .expect("plain service should succeed without ACL");

        assert_eq!(result.deleted_datasets, 1);
        assert_eq!(result.deleted_data, 1);
    }

    #[tokio::test]
    async fn authorized_preview_checks_acl() {
        let (svc, storage, db) = make_authorized_service().await;
        let owner = Uuid::new_v4();
        seed_dataset_with_data(&db, &storage, owner, "preview_acl_ds").await;

        // Without permission, even preview should fail
        ops::acl::ensure_principal(&db, owner, "user")
            .await
            .unwrap();

        let err = svc
            .preview(
                &DeleteRequest {
                    scope: DeleteScope::Dataset {
                        owner_id: owner,
                        dataset_name: "preview_acl_ds".to_string(),
                    },
                    mode: DeleteMode::Soft,
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
            dataset_id,
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
                },
                mode: DeleteMode::Soft,
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
    async fn test_dataset_deletion_cascades_pipeline_runs() {
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
            dataset_id,
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
            })
            .await
            .expect("execute should succeed");

        assert_eq!(result.deleted_datasets, 1);

        // Pipeline runs are handled by FK CASCADE (delete_dataset triggers it),
        // so deleted_pipeline_runs counter should be 0 for dataset-scoped deletion.
        // But the rows should still be gone.
        let status_after =
            ops::pipeline_runs::get_latest_pipeline_status(&db, "cognify_pipeline", dataset_id)
                .await
                .unwrap();
        assert!(
            status_after.is_none(),
            "pipeline run should be cascade-deleted with the dataset"
        );
    }
}
