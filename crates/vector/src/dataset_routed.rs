//! The vector engine of per-dataset storage (`ENABLE_BACKEND_ACCESS_CONTROL`).
//!
//! The vector twin of `cognee_graph::DatasetRoutedGraphDB`: every call forwards
//! to the engine of the dataset in the current
//! [`cognee_models::dataset_scope`], and a call outside every dataset scope
//! fails instead of reaching a shared store. See
//! `docs/design/per-dataset-storage.md`.

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use cognee_models::dataset_scope;

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::vector_db_trait::{VectorDB, VectorIndexBackfill};

/// The message of a vector call made outside every dataset scope.
pub const NO_DATASET_SCOPE: &str = "no dataset scope: per-dataset storage \
     (ENABLE_BACKEND_ACCESS_CONTROL) requires every vector access to run inside a dataset \
     (cognee_models::dataset_scope::with_dataset)";

/// Resolves a dataset's own vector engine.
#[async_trait]
pub trait DatasetVectorResolver: Send + Sync {
    /// The dataset's engine, provisioning its database on first use.
    async fn vector_for_dataset(&self, dataset_id: Uuid) -> VectorDBResult<Arc<dyn VectorDB>>;
    /// The dataset's engine if it is already open; never provisions. For the
    /// synchronous paths.
    fn cached_vector(&self, dataset_id: Uuid) -> Option<Arc<dyn VectorDB>>;
    /// Every open engine, for `close`.
    fn cached_vectors(&self) -> Vec<Arc<dyn VectorDB>>;
}

/// A vector engine that routes each call to the current dataset's engine.
pub struct DatasetRoutedVectorDB {
    resolver: Arc<dyn DatasetVectorResolver>,
}

impl DatasetRoutedVectorDB {
    pub fn new(resolver: Arc<dyn DatasetVectorResolver>) -> Self {
        Self { resolver }
    }

    async fn engine(&self) -> VectorDBResult<Arc<dyn VectorDB>> {
        match dataset_scope::current_dataset() {
            Some(dataset_id) => self.resolver.vector_for_dataset(dataset_id).await,
            None => Err(VectorDBError::StorageError(NO_DATASET_SCOPE.to_string())),
        }
    }
}

#[async_trait]
impl VectorDB for DatasetRoutedVectorDB {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        self.engine()
            .await?
            .create_collection(data_type, field_name, dimension)
            .await
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        self.engine()
            .await?
            .has_collection(data_type, field_name)
            .await
    }

    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        self.engine()
            .await?
            .index_points(data_type, field_name, points)
            .await
    }

    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.engine()
            .await?
            .search_similar(data_type, field_name, query_vector, top_k)
            .await
    }

    async fn search_similar_filtered(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
        node_name: Option<&[String]>,
        node_name_filter_operator: &str,
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.engine()
            .await?
            .search_similar_filtered(
                data_type,
                field_name,
                query_vector,
                top_k,
                node_name,
                node_name_filter_operator,
            )
            .await
    }

    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        self.engine()
            .await?
            .delete_collection(data_type, field_name)
            .await
    }

    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        self.engine()
            .await?
            .delete_points(data_type, field_name, point_ids)
            .await
    }

    async fn upsert_raw_vectors(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        self.engine()
            .await?
            .upsert_raw_vectors(data_type, field_name, points)
            .await
    }

    async fn retrieve(
        &self,
        data_type: &str,
        field_name: &str,
        ids: &[Uuid],
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.engine()
            .await?
            .retrieve(data_type, field_name, ids)
            .await
    }

    async fn score_by_ids(
        &self,
        data_type: &str,
        field_name: &str,
        ids: &[Uuid],
        query_vector: &[f32],
    ) -> VectorDBResult<Option<Vec<SearchResult>>> {
        self.engine()
            .await?
            .score_by_ids(data_type, field_name, ids, query_vector)
            .await
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        self.engine()
            .await?
            .collection_size(data_type, field_name)
            .await
    }

    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        self.engine().await?.list_collections().await
    }

    /// The current dataset's collections. Dropping *every* dataset's store is
    /// per-dataset storage's prune, not this engine's.
    async fn prune(&self) -> VectorDBResult<()> {
        self.engine().await?.prune().await
    }

    /// Close every open dataset engine: the process-level teardown, never a
    /// per-dataset one.
    async fn close(&self) -> VectorDBResult<()> {
        let mut first_error = None;
        for engine in self.resolver.cached_vectors() {
            if let Err(error) = engine.close().await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn create_missing_vector_indexes(&self) -> VectorDBResult<VectorIndexBackfill> {
        self.engine().await?.create_missing_vector_indexes().await
    }

    async fn begin_bulk_load(&self) -> VectorDBResult<()> {
        self.engine().await?.begin_bulk_load().await
    }

    async fn end_bulk_load(&self) -> VectorDBResult<()> {
        self.engine().await?.end_bulk_load().await
    }

    /// Synchronous, so it reaches only an engine that is already open — the
    /// one `begin_bulk_load` opened.
    fn abandon_bulk_load(&self) {
        if let Some(engine) =
            dataset_scope::current_dataset().and_then(|id| self.resolver.cached_vector(id))
        {
            engine.abandon_bulk_load();
        }
    }

    async fn announce_bulk_write(
        &self,
        data_type: &str,
        field_name: &str,
        points: usize,
    ) -> VectorDBResult<()> {
        self.engine()
            .await?
            .announce_bulk_write(data_type, field_name, points)
            .await
    }

    async fn batch_search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vectors: &[Vec<f32>],
        top_k: usize,
    ) -> VectorDBResult<Vec<Vec<SearchResult>>> {
        self.engine()
            .await?
            .batch_search_similar(data_type, field_name, query_vectors, top_k)
            .await
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::BruteForceVectorDB;

    #[derive(Default)]
    struct PerDataset {
        engines: Mutex<HashMap<Uuid, Arc<dyn VectorDB>>>,
    }

    #[async_trait]
    impl DatasetVectorResolver for PerDataset {
        async fn vector_for_dataset(&self, dataset_id: Uuid) -> VectorDBResult<Arc<dyn VectorDB>> {
            // lock poison is unrecoverable
            let mut engines = self.engines.lock().unwrap();
            Ok(Arc::clone(engines.entry(dataset_id).or_insert_with(|| {
                Arc::new(BruteForceVectorDB::new()) as Arc<dyn VectorDB>
            })))
        }

        fn cached_vector(&self, dataset_id: Uuid) -> Option<Arc<dyn VectorDB>> {
            // lock poison is unrecoverable
            self.engines.lock().unwrap().get(&dataset_id).cloned()
        }

        fn cached_vectors(&self) -> Vec<Arc<dyn VectorDB>> {
            // lock poison is unrecoverable
            self.engines.lock().unwrap().values().cloned().collect()
        }
    }

    #[tokio::test]
    async fn each_dataset_has_its_own_collections() {
        let vectors = DatasetRoutedVectorDB::new(Arc::new(PerDataset::default()));
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        dataset_scope::with_dataset(a, vectors.create_collection("Entity", "name", 3))
            .await
            .unwrap();
        assert!(
            dataset_scope::with_dataset(a, vectors.has_collection("Entity", "name"))
                .await
                .unwrap()
        );
        assert!(
            !dataset_scope::with_dataset(b, vectors.has_collection("Entity", "name"))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_call_outside_every_scope_fails_closed() {
        let vectors = DatasetRoutedVectorDB::new(Arc::new(PerDataset::default()));
        let error = vectors.list_collections().await.unwrap_err();
        assert!(error.to_string().contains("no dataset scope"), "{error}");
        vectors.abandon_bulk_load();
        vectors.close().await.unwrap();
    }
}
