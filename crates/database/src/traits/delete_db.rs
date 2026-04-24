use async_trait::async_trait;
use cognee_models::{Data, Dataset};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::{data, datasets, graph_storage, pipeline_runs, search_history};
use crate::types::{DatabaseError, GraphEdge, GraphNode};

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
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Dataset>, DatabaseError>;
    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError>;
    /// Count the number of data items linked to a dataset without loading them.
    async fn count_dataset_data(&self, dataset_id: Uuid) -> Result<usize, DatabaseError>;
    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError>;
    async fn list_datasets(&self) -> Result<Vec<Dataset>, DatabaseError>;
    async fn delete_dataset(&self, id: Uuid) -> Result<(), DatabaseError>;
    async fn detach_data_from_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError>;

    // ------------------------------------------------------------------
    // Pipeline cleanup methods
    // ------------------------------------------------------------------

    /// Delete all `pipeline_runs` rows for a given dataset.
    ///
    /// Needed for data-scoped deletion where the dataset itself is not deleted
    /// (FK cascade does not fire) but the pipeline cache should be invalidated.
    async fn delete_pipeline_runs_by_dataset(&self, dataset_id: Uuid)
    -> Result<u64, DatabaseError>;

    /// Clear `pipeline_status` JSON entries keyed by `dataset_id` from all
    /// `Data` records linked to that dataset.
    ///
    /// Must be called while junction rows still exist.
    async fn clear_pipeline_status_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError>;

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

    // ------------------------------------------------------------------
    // Search history cleanup methods
    // ------------------------------------------------------------------

    /// Delete all search history (queries + cascaded results) for a user.
    ///
    /// Returns the number of deleted query rows.
    async fn delete_search_history_for_user(&self, user_id: Uuid) -> Result<u64, DatabaseError>;

    /// Delete all search history (queries + cascaded results).
    ///
    /// Returns the number of deleted query rows.
    async fn delete_all_search_history(&self) -> Result<u64, DatabaseError>;

    /// Count search history query rows for a specific user.
    async fn count_search_history_for_user(&self, user_id: Uuid) -> Result<u64, DatabaseError>;

    /// Count all search history query rows.
    async fn count_all_search_history(&self) -> Result<u64, DatabaseError>;
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
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Dataset>, DatabaseError> {
        datasets::get_dataset_by_name(self, name, owner_id, tenant_id).await
    }

    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError> {
        datasets::get_dataset_data(self, dataset_id).await
    }

    async fn count_dataset_data(&self, dataset_id: Uuid) -> Result<usize, DatabaseError> {
        datasets::count_dataset_data(self, dataset_id).await
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

    // ------------------------------------------------------------------
    // Pipeline cleanup
    // ------------------------------------------------------------------

    async fn delete_pipeline_runs_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<u64, DatabaseError> {
        pipeline_runs::delete_pipeline_runs_by_dataset(self, dataset_id).await
    }

    async fn clear_pipeline_status_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError> {
        data::clear_pipeline_status_for_dataset(self, dataset_id).await
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

    // ------------------------------------------------------------------
    // Search history cleanup
    // ------------------------------------------------------------------

    async fn delete_search_history_for_user(&self, user_id: Uuid) -> Result<u64, DatabaseError> {
        search_history::delete_queries_by_user(self, user_id).await
    }

    async fn delete_all_search_history(&self) -> Result<u64, DatabaseError> {
        search_history::delete_all_queries(self).await
    }

    async fn count_search_history_for_user(&self, user_id: Uuid) -> Result<u64, DatabaseError> {
        search_history::count_queries_by_user(self, user_id).await
    }

    async fn count_all_search_history(&self) -> Result<u64, DatabaseError> {
        search_history::count_all_queries(self).await
    }
}
