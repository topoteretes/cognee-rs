//! [`FailingDeleteDb`]: a [`DeleteDb`] wrapper whose `delete_dataset` fails
//! for one dataset id — drives the "stop at the first failing dataset"
//! (`DeleteError::Partial`) paths of the per-dataset deletes.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_database::{DatabaseError, DeleteDb, GraphEdge, GraphNode};
use cognee_models::{Data, Dataset};
use uuid::Uuid;

/// Forwards every [`DeleteDb`] call to `inner`, except that
/// `delete_dataset(fail_delete_of)` returns a `QueryError`.
pub struct FailingDeleteDb {
    inner: Arc<dyn DeleteDb>,
    fail_delete_of: Option<Uuid>,
}

impl FailingDeleteDb {
    /// Wrap `inner`; `delete_dataset` fails for `fail_delete_of` (never when
    /// `None`).
    pub fn new(inner: Arc<dyn DeleteDb>, fail_delete_of: Option<Uuid>) -> Self {
        Self {
            inner,
            fail_delete_of,
        }
    }
}

#[async_trait]
impl DeleteDb for FailingDeleteDb {
    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError> {
        DeleteDb::get_data(self.inner.as_ref(), id).await
    }

    async fn delete_data(&self, id: Uuid) -> Result<(), DatabaseError> {
        DeleteDb::delete_data(self.inner.as_ref(), id).await
    }

    async fn count_data_dataset_links(&self, data_id: Uuid) -> Result<usize, DatabaseError> {
        DeleteDb::count_data_dataset_links(self.inner.as_ref(), data_id).await
    }

    async fn list_datasets_for_data(&self, data_id: Uuid) -> Result<Vec<Dataset>, DatabaseError> {
        DeleteDb::list_datasets_for_data(self.inner.as_ref(), data_id).await
    }

    async fn get_dataset_data(&self, dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError> {
        DeleteDb::get_dataset_data(self.inner.as_ref(), dataset_id).await
    }

    async fn count_dataset_data(&self, dataset_id: Uuid) -> Result<usize, DatabaseError> {
        DeleteDb::count_dataset_data(self.inner.as_ref(), dataset_id).await
    }

    async fn list_datasets_by_owner(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatabaseError> {
        DeleteDb::list_datasets_by_owner(self.inner.as_ref(), owner_id).await
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>, DatabaseError> {
        DeleteDb::list_datasets(self.inner.as_ref()).await
    }

    async fn delete_dataset(&self, id: Uuid) -> Result<(), DatabaseError> {
        if Some(id) == self.fail_delete_of {
            return Err(DatabaseError::QueryError(
                "simulated delete_dataset failure".into(),
            ));
        }
        DeleteDb::delete_dataset(self.inner.as_ref(), id).await
    }

    async fn detach_data_from_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError> {
        DeleteDb::detach_data_from_dataset(self.inner.as_ref(), dataset_id, data_id).await
    }

    async fn delete_pipeline_runs_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<u64, DatabaseError> {
        DeleteDb::delete_pipeline_runs_by_dataset(self.inner.as_ref(), dataset_id).await
    }

    async fn clear_pipeline_status_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError> {
        DeleteDb::clear_pipeline_status_for_dataset(self.inner.as_ref(), dataset_id).await
    }

    async fn clear_cognify_pipeline_status_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        DeleteDb::clear_cognify_pipeline_status_for_data(self.inner.as_ref(), data_id, dataset_id)
            .await
    }

    async fn get_nodes_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphNode>, DatabaseError> {
        DeleteDb::get_nodes_by_dataset(self.inner.as_ref(), dataset_id).await
    }

    async fn get_edges_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphEdge>, DatabaseError> {
        DeleteDb::get_edges_by_dataset(self.inner.as_ref(), dataset_id).await
    }

    async fn get_unique_nodes_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphNode>, DatabaseError> {
        DeleteDb::get_unique_nodes_for_data(self.inner.as_ref(), data_id, dataset_id).await
    }

    async fn get_unique_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphEdge>, DatabaseError> {
        DeleteDb::get_unique_edges_for_data(self.inner.as_ref(), data_id, dataset_id).await
    }

    async fn get_structural_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<Vec<GraphEdge>, DatabaseError> {
        DeleteDb::get_structural_edges_for_data(self.inner.as_ref(), data_id, dataset_id).await
    }

    async fn delete_provenance_edges_by_ids(&self, ids: &[Uuid]) -> Result<u64, DatabaseError> {
        DeleteDb::delete_provenance_edges_by_ids(self.inner.as_ref(), ids).await
    }

    async fn get_edge_retrieval_texts_claimed_outside_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
        candidates: &[String],
    ) -> Result<Vec<String>, DatabaseError> {
        DeleteDb::get_edge_retrieval_texts_claimed_outside_data(
            self.inner.as_ref(),
            data_id,
            dataset_id,
            candidates,
        )
        .await
    }

    async fn delete_provenance_nodes_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        DeleteDb::delete_provenance_nodes_for_dataset(self.inner.as_ref(), dataset_id).await
    }

    async fn delete_provenance_edges_for_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        DeleteDb::delete_provenance_edges_for_dataset(self.inner.as_ref(), dataset_id).await
    }

    async fn delete_provenance_nodes_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        DeleteDb::delete_provenance_nodes_for_data(self.inner.as_ref(), data_id, dataset_id).await
    }

    async fn delete_provenance_edges_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatabaseError> {
        DeleteDb::delete_provenance_edges_for_data(self.inner.as_ref(), data_id, dataset_id).await
    }

    async fn get_provenance_node_count_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError> {
        DeleteDb::get_provenance_node_count_for_data(self.inner.as_ref(), data_id, dataset_id).await
    }

    async fn get_provenance_edge_count_for_data(
        &self,
        data_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<usize, DatabaseError> {
        DeleteDb::get_provenance_edge_count_for_data(self.inner.as_ref(), data_id, dataset_id).await
    }

    async fn delete_search_history_for_user(&self, user_id: Uuid) -> Result<u64, DatabaseError> {
        DeleteDb::delete_search_history_for_user(self.inner.as_ref(), user_id).await
    }

    async fn delete_all_search_history(&self) -> Result<u64, DatabaseError> {
        DeleteDb::delete_all_search_history(self.inner.as_ref()).await
    }

    async fn count_search_history_for_user(&self, user_id: Uuid) -> Result<u64, DatabaseError> {
        DeleteDb::count_search_history_for_user(self.inner.as_ref(), user_id).await
    }

    async fn count_all_search_history(&self) -> Result<u64, DatabaseError> {
        DeleteDb::count_all_search_history(self.inner.as_ref()).await
    }
}
