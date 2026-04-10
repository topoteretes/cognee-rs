use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cognee_models::{Data, Dataset};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::{data, datasets};
use crate::types::DatabaseError;

#[async_trait]
pub trait IngestDb: Send + Sync {
    async fn get_dataset_by_name(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Dataset>, DatabaseError>;

    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset, DatabaseError>;

    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError>;

    async fn create_data(&self, d: Data) -> Result<Data, DatabaseError>;

    async fn attach_data_to_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError>;

    /// Update the `last_accessed` timestamp on the given Data records.
    ///
    /// Implementations should perform a bulk `UPDATE data SET last_accessed = ?
    /// WHERE id IN (...)` query. An empty `data_ids` slice is a no-op.
    async fn update_last_accessed(
        &self,
        data_ids: &[Uuid],
        timestamp: DateTime<Utc>,
    ) -> Result<(), DatabaseError>;
}

#[async_trait]
impl IngestDb for DatabaseConnection {
    async fn get_dataset_by_name(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Option<Dataset>, DatabaseError> {
        datasets::get_dataset_by_name(self, name, owner_id, tenant_id).await
    }

    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset, DatabaseError> {
        datasets::create_dataset(self, dataset).await
    }

    async fn get_data(&self, id: Uuid) -> Result<Option<Data>, DatabaseError> {
        data::get_data(self, id).await
    }

    async fn create_data(&self, d: Data) -> Result<Data, DatabaseError> {
        data::create_data(self, d).await
    }

    async fn attach_data_to_dataset(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
    ) -> Result<(), DatabaseError> {
        datasets::attach_data_to_dataset(self, dataset_id, data_id).await
    }

    async fn update_last_accessed(
        &self,
        data_ids: &[Uuid],
        timestamp: DateTime<Utc>,
    ) -> Result<(), DatabaseError> {
        data::update_last_accessed(self, data_ids, timestamp).await
    }
}
