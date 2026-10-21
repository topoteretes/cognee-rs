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

    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError>;
    /// Count the number of data items linked to a dataset without loading them.
    async fn count_dataset_data(&self, dataset_id: Uuid) -> Result<usize, DatabaseError>;
    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError>;
    async fn list_datasets(&self) -> Result<Vec<Dataset>, DatabaseError>;
    /// The dataset named `name` owned by `owner_id` in `tenant_id`, with
    /// **strict** tenant equality (`None` matches only a tenant-less row) —
    /// Python's `get_dataset_ids` rule
    /// (`cognee/modules/data/methods/get_dataset_ids.py:30-34@37f788bd2`).
    /// This is the only by-name dataset lookup: the tenant-loose variant was
    /// removed (SDK-936), so a name never resolves across tenants.
    ///
    /// The default filters [`Self::list_datasets_by_owner`] and is correct for
    /// any implementation; override it with a point query.
    async fn get_dataset_by_name_in_tenant(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Dataset>, DatabaseError> {
        Ok(self
            .list_datasets_by_owner(owner_id)
            .await?
            .into_iter()
            .find(|ds| ds.name == name && ds.tenant_id == tenant_id))
    }

    /// The datasets among `ids` that exist, in `ids` order (duplicates and
    /// missing ids dropped). The default looks each id up with
    /// [`Self::get_dataset_by_id`]; override it with a batch query.
    async fn get_datasets_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Dataset>, DatabaseError> {
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if seen.insert(*id)
                && let Some(ds) = self.get_dataset_by_id(*id).await?
            {
                out.push(ds);
            }
        }
        Ok(out)
    }
    /// A dataset by id, whoever owns it. Defaults to scanning
    /// [`Self::list_datasets`]; implementations with an indexed lookup
    /// override it.
    async fn get_dataset_by_id(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError> {
        Ok(self
            .list_datasets()
            .await?
            .into_iter()
            .find(|dataset| dataset.id == id))
    }
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

    /// Clear only the `cognify_pipeline` key from the `pipeline_status` JSON
    /// of a single Data record, removing only the entry keyed by `dataset_id`.
    async fn clear_cognify_pipeline_status_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError>;

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

    /// Structural edge rows (`is_part_of`, `contains`, ...) that cognify wrote
    /// with a nil `data_id` but that belong to `(data_id, dataset_id)` because
    /// one of their endpoints is exclusive to it. Selected through their
    /// endpoints because the `data_id` column does not name them, so this must
    /// be called before the item's node rows are deleted.
    async fn get_structural_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphEdge>, DatabaseError>;

    /// Delete the provenance edge rows with these ids, returning how many
    /// rows were actually deleted.
    async fn delete_provenance_edges_by_ids(&self, ids: &[Uuid]) -> Result<u64, DatabaseError>;

    /// The subset of `candidates` (`EdgeType` retrieval texts) still claimed
    /// by edge rows that survive the deletion of `(data_id, dataset_id)`.
    async fn get_edge_retrieval_texts_claimed_outside_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
        candidates: &[String],
    ) -> Result<Vec<String>, DatabaseError>;

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

    async fn get_dataset_by_name_in_tenant(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Dataset>, DatabaseError> {
        datasets::get_dataset_by_name_in_tenant(self, name, owner_id, tenant_id).await
    }

    async fn get_datasets_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Dataset>, DatabaseError> {
        datasets::get_datasets_by_ids(self, ids).await
    }

    async fn get_dataset_by_id(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError> {
        datasets::get_dataset(self, id).await
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

    async fn clear_cognify_pipeline_status_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        data::clear_cognify_pipeline_status_for_data(self, data_id, dataset_id).await
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

    async fn get_structural_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphEdge>, DatabaseError> {
        graph_storage::get_structural_edges_for_data(self, data_id, dataset_id).await
    }

    async fn delete_provenance_edges_by_ids(&self, ids: &[Uuid]) -> Result<u64, DatabaseError> {
        graph_storage::delete_edges_by_ids(self, ids).await
    }

    async fn get_edge_retrieval_texts_claimed_outside_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
        candidates: &[String],
    ) -> Result<Vec<String>, DatabaseError> {
        graph_storage::get_edge_retrieval_texts_claimed_outside_data(
            self, data_id, dataset_id, candidates,
        )
        .await
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
