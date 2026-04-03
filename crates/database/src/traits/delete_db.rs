use async_trait::async_trait;
use cognee_models::{Data, Dataset};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::ops::{artifact_refs, data, datasets};
use crate::types::{ArtifactReference, DatabaseError};

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
        datasets::get_dataset_by_name(self, name, owner_id).await
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
}
