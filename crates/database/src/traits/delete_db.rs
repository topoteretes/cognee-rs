use async_trait::async_trait;
use cognee_models::{Data, Dataset};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::{artifact_refs, data, datasets, graph_storage};
use crate::types::{ArtifactReference, DatabaseError, GraphEdge, GraphNode};

#[async_trait]
pub trait DeleteDb: Send + Sync {
    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError>;
    async fn delete_data(&self, id: Uuid) -> Result<(), DatabaseError>;
    async fn count_data_dataset_links(&self, data_id: Uuid) -> Result<usize, DatabaseError>;
    async fn list_datasets_for_data(&self, data_id: Uuid) -> Result<Vec<Dataset>, DatabaseError>;

    async fn get_dataset_by_name(
        &self,
        name: &str,
        owner_id: Uuid,
    ) -> Result<Option<Dataset>, DatabaseError>;
    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError>;
    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError>;
    async fn list_datasets(&self) -> Result<Vec<Dataset>, DatabaseError>;
    async fn delete_dataset(&self, id: Uuid) -> Result<(), DatabaseError>;
    async fn detach_data_from_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError>;

    async fn list_artifact_references_for_data(
        &self,
        data_id: Uuid,
    ) -> Result<Vec<ArtifactReference>, DatabaseError>;
    async fn list_artifact_references_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<ArtifactReference>, DatabaseError>;

    // ------------------------------------------------------------------
    // Graph provenance methods
    // ------------------------------------------------------------------

    /// Get all provenance node rows for a dataset.
    async fn get_nodes_by_dataset(&self, dataset_id: Uuid)
    -> Result<Vec<GraphNode>, DatabaseError>;

    /// Get all provenance edge rows for a dataset.
    async fn get_edges_by_dataset(&self, dataset_id: Uuid)
    -> Result<Vec<GraphEdge>, DatabaseError>;

    /// Get nodes belonging to `(data_id, dataset_id)` whose slug is NOT shared
    /// with other data items in the same dataset. Safe for targeted deletion.
    async fn get_unique_nodes_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphNode>, DatabaseError>;

    /// Get edges belonging to `(data_id, dataset_id)` whose slug is NOT shared
    /// with other data items in the same dataset.
    async fn get_unique_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphEdge>, DatabaseError>;

    /// Delete all provenance node rows for a dataset.
    async fn delete_provenance_nodes_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Delete all provenance edge rows for a dataset.
    async fn delete_provenance_edges_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Delete provenance node rows for a specific `(data_id, dataset_id)` pair.
    async fn delete_provenance_nodes_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Delete provenance edge rows for a specific `(data_id, dataset_id)` pair.
    async fn delete_provenance_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Count provenance node rows for a specific `(data_id, dataset_id)` pair.
    async fn get_provenance_node_count_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError>;

    /// Count provenance edge rows for a specific `(data_id, dataset_id)` pair.
    async fn get_provenance_edge_count_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError>;
}

#[async_trait]
impl DeleteDb for DatabaseConnection {
    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError> {
        data::get_data(self, id).await
    }

    async fn delete_data(&self, id: Uuid) -> Result<(), DatabaseError> {
        data::delete_data(self, id).await
    }

    async fn count_data_dataset_links(&self, data_id: Uuid) -> Result<usize, DatabaseError> {
        data::count_data_dataset_links(self, data_id).await
    }

    async fn list_datasets_for_data(&self, data_id: Uuid) -> Result<Vec<Dataset>, DatabaseError> {
        data::list_datasets_for_data(self, data_id).await
    }

    async fn get_dataset_by_name(
        &self,
        name: &str,
        owner_id: Uuid,
    ) -> Result<Option<Dataset>, DatabaseError> {
        datasets::get_dataset_by_name(self, name, owner_id, None).await
    }

    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError> {
        datasets::get_dataset_data(self, dataset_id).await
    }

    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError> {
        datasets::list_datasets_by_owner(self, owner_id).await
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>, DatabaseError> {
        datasets::list_datasets(self).await
    }

    async fn delete_dataset(&self, id: Uuid) -> Result<(), DatabaseError> {
        datasets::delete_dataset(self, id).await
    }

    async fn detach_data_from_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError> {
        datasets::detach_data_from_dataset(self, dataset_id, data_id).await
    }

    async fn list_artifact_references_for_data(
        &self,
        data_id: Uuid,
    ) -> Result<Vec<ArtifactReference>, DatabaseError> {
        artifact_refs::list_artifact_references_for_data(self, data_id).await
    }

    async fn list_artifact_references_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<ArtifactReference>, DatabaseError> {
        artifact_refs::list_artifact_references_for_dataset(self, dataset_id).await
    }

    // ------------------------------------------------------------------
    // Graph provenance
    // ------------------------------------------------------------------

    async fn get_nodes_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphNode>, DatabaseError> {
        graph_storage::get_nodes_by_dataset(self, dataset_id).await
    }

    async fn get_edges_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphEdge>, DatabaseError> {
        graph_storage::get_edges_by_dataset(self, dataset_id).await
    }

    async fn get_unique_nodes_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphNode>, DatabaseError> {
        graph_storage::get_unique_nodes_for_data(self, data_id, dataset_id).await
    }

    async fn get_unique_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphEdge>, DatabaseError> {
        graph_storage::get_unique_edges_for_data(self, data_id, dataset_id).await
    }

    async fn delete_provenance_nodes_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        graph_storage::delete_nodes_by_dataset(self, dataset_id).await
    }

    async fn delete_provenance_edges_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        graph_storage::delete_edges_by_dataset(self, dataset_id).await
    }

    async fn delete_provenance_nodes_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        graph_storage::delete_nodes_for_data(self, data_id, dataset_id).await
    }

    async fn delete_provenance_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        graph_storage::delete_edges_for_data(self, data_id, dataset_id).await
    }

    async fn get_provenance_node_count_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError> {
        graph_storage::count_nodes_for_data(self, data_id, dataset_id).await
    }

    async fn get_provenance_edge_count_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError> {
        graph_storage::count_edges_for_data(self, data_id, dataset_id).await
    }
}
