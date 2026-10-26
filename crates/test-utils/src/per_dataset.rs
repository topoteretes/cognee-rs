//! In-memory per-dataset storage for tests: routing graph and vector engines
//! (`ENABLE_BACKEND_ACCESS_CONTROL`) over one fresh mock store per dataset.
//!
//! Lets a pipeline test run exactly as it does under per-dataset storage —
//! every graph and vector call goes through `DatasetRoutedGraphDB` /
//! `DatasetRoutedVectorDB` and fails outside a dataset scope — without a
//! relational `dataset_database` row or a component registry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use uuid::Uuid;

use cognee_graph::{
    DatasetGraphResolver, DatasetRoutedGraphDB, GraphDBResult, GraphDBTrait, MockGraphDB,
};
use cognee_vector::{
    DatasetRoutedVectorDB, DatasetVectorResolver, MockVectorDB, VectorDB, VectorDBResult,
};

/// One [`MockGraphDB`] and one [`MockVectorDB`] per dataset, created on first
/// use, behind routing engines.
#[derive(Default)]
pub struct PerDatasetMocks {
    graphs: Mutex<HashMap<Uuid, Arc<MockGraphDB>>>,
    vectors: Mutex<HashMap<Uuid, Arc<MockVectorDB>>>,
}

impl PerDatasetMocks {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The routing graph engine to hand to the code under test.
    pub fn graph_db(self: &Arc<Self>) -> Arc<dyn GraphDBTrait> {
        Arc::new(DatasetRoutedGraphDB::new(
            Arc::clone(self) as Arc<dyn DatasetGraphResolver>
        ))
    }

    /// The routing vector engine to hand to the code under test.
    pub fn vector_db(self: &Arc<Self>) -> Arc<dyn VectorDB> {
        Arc::new(DatasetRoutedVectorDB::new(
            Arc::clone(self) as Arc<dyn DatasetVectorResolver>
        ))
    }

    /// The dataset's own graph, for assertions (created when missing).
    pub fn graph(&self, dataset_id: Uuid) -> Arc<MockGraphDB> {
        Arc::clone(
            self.graphs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(dataset_id)
                .or_default(),
        )
    }

    /// The dataset's own vector store, for assertions (created when missing).
    pub fn vector(&self, dataset_id: Uuid) -> Arc<MockVectorDB> {
        Arc::clone(
            self.vectors
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(dataset_id)
                .or_default(),
        )
    }

    /// The datasets whose graph has been reached so far.
    pub fn graph_datasets(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .graphs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .copied()
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Forget a dataset's stores, as dropping its databases would.
    pub fn drop_dataset(&self, dataset_id: Uuid) {
        self.graphs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&dataset_id);
        self.vectors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&dataset_id);
    }
}

#[async_trait]
impl DatasetGraphResolver for PerDatasetMocks {
    async fn graph_for_dataset(&self, dataset_id: Uuid) -> GraphDBResult<Arc<dyn GraphDBTrait>> {
        Ok(self.graph(dataset_id) as Arc<dyn GraphDBTrait>)
    }

    fn cached_graph(&self, dataset_id: Uuid) -> Option<Arc<dyn GraphDBTrait>> {
        self.graphs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&dataset_id)
            .map(|g| Arc::clone(g) as Arc<dyn GraphDBTrait>)
    }

    fn cached_graphs(&self) -> Vec<Arc<dyn GraphDBTrait>> {
        self.graphs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|g| Arc::clone(g) as Arc<dyn GraphDBTrait>)
            .collect()
    }
}

#[async_trait]
impl DatasetVectorResolver for PerDatasetMocks {
    async fn vector_for_dataset(&self, dataset_id: Uuid) -> VectorDBResult<Arc<dyn VectorDB>> {
        Ok(self.vector(dataset_id) as Arc<dyn VectorDB>)
    }

    fn cached_vector(&self, dataset_id: Uuid) -> Option<Arc<dyn VectorDB>> {
        self.vectors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&dataset_id)
            .map(|v| Arc::clone(v) as Arc<dyn VectorDB>)
    }

    fn cached_vectors(&self) -> Vec<Arc<dyn VectorDB>> {
        self.vectors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|v| Arc::clone(v) as Arc<dyn VectorDB>)
            .collect()
    }
}

#[async_trait]
impl cognee_database::DatasetStorageLifecycle for PerDatasetMocks {
    async fn drop_dataset_storage(
        &self,
        dataset_id: Uuid,
    ) -> Result<bool, cognee_database::DatabaseError> {
        let existed =
            self.cached_graph(dataset_id).is_some() || self.cached_vector(dataset_id).is_some();
        self.drop_dataset(dataset_id);
        Ok(existed)
    }

    async fn drop_all_dataset_storage(&self) -> Result<usize, cognee_database::DatabaseError> {
        let mut datasets = self.graph_datasets();
        datasets.extend(
            self.vectors
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .keys()
                .copied(),
        );
        datasets.sort_unstable();
        datasets.dedup();
        for id in &datasets {
            self.drop_dataset(*id);
        }
        Ok(datasets.len())
    }
}
