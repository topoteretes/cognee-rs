use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use cognee_database::{ArtifactReference, DatabaseTrait};
use cognee_models::Dataset;
use cognee_storage::{StorageError, StorageTrait};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeleteResult {
    pub deleted_datasets: usize,
    pub deleted_dataset_links: usize,
    pub deleted_data: usize,
    pub deleted_storage_files: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Error)]
pub enum DeleteError {
    #[error("{0}")]
    Validation(String),

    #[error("{0}")]
    Runtime(String),
}

struct ResolvedDeleteTargets {
    datasets_to_delete: Vec<Dataset>,
    links_to_detach: Vec<(Uuid, Uuid)>,
    candidate_data_ids: Vec<Uuid>,
}

pub struct DeleteService<S: StorageTrait, D: DatabaseTrait> {
    storage: Arc<S>,
    database: Arc<D>,
}

impl<S: StorageTrait, D: DatabaseTrait> DeleteService<S, D> {
    pub fn new(storage: Arc<S>, database: Arc<D>) -> Self {
        Self { storage, database }
    }

    pub async fn preview(&self, request: &DeleteRequest) -> Result<DeletePreview, DeleteError> {
        let targets = self.resolve_targets(request).await?;
        let data_to_delete = self
            .count_data_that_would_be_deleted(&targets.candidate_data_ids, &targets.links_to_detach)
            .await?;

        Ok(DeletePreview {
            datasets_to_delete: targets.datasets_to_delete.len(),
            dataset_links_to_delete: targets.links_to_detach.len(),
            data_to_delete,
            storage_files_to_delete: data_to_delete,
        })
    }

    pub async fn execute(&self, request: &DeleteRequest) -> Result<DeleteResult, DeleteError> {
        let targets = self.resolve_targets(request).await?;

        let mut warnings = Vec::new();
        let mut deleted_links = 0usize;
        let mut deleted_datasets = 0usize;
        let mut deleted_data = 0usize;
        let mut deleted_storage = 0usize;

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

        if matches!(request.mode, DeleteMode::Hard) {
            warnings.push(
                "Hard mode currently performs soft deletion plus warnings; orphan graph/vector sweep is planned next."
                    .to_string(),
            );
        }

        Ok(DeleteResult {
            deleted_datasets,
            deleted_dataset_links: deleted_links,
            deleted_data,
            deleted_storage_files: deleted_storage,
            warnings,
        })
    }

    pub async fn data_ids_to_delete(
        &self,
        request: &DeleteRequest,
    ) -> Result<Vec<Uuid>, DeleteError> {
        let targets = self.resolve_targets(request).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_database::MockDatabase;
    use cognee_models::{Data, Dataset};
    use cognee_storage::MockStorage;

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    fn make_service() -> DeleteService<MockStorage, MockDatabase> {
        DeleteService::new(Arc::new(MockStorage::new()), Arc::new(MockDatabase::new()))
    }

    /// Seed one dataset + one data item, attach them, and return their IDs.
    async fn seed_dataset_with_data(
        db: &MockDatabase,
        storage: &MockStorage,
        owner_id: Uuid,
        dataset_name: &str,
    ) -> (Uuid, Uuid) {
        let dataset = Dataset::new(dataset_name.to_string(), owner_id);
        let dataset_id = dataset.id;
        db.create_dataset(dataset).await.unwrap();

        let location = storage.store(b"test content", "test.txt").await.unwrap();

        let data_id = Uuid::new_v4();
        let data = Data::new(
            data_id,
            "test.txt".to_string(),
            location,
            "file://test.txt".to_string(),
            "txt".to_string(),
            "text/plain".to_string(),
            "hash_placeholder".to_string(),
            owner_id,
        );
        db.create_data(data).await.unwrap();
        db.attach_data_to_dataset(dataset_id, data_id)
            .await
            .unwrap();

        (dataset_id, data_id)
    }

    // ------------------------------------------------------------------
    // Step 2 — test_delete_dataset_with_force
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn delete_dataset_with_force_removes_dataset_and_data() {
        let storage = Arc::new(MockStorage::new());
        let database = Arc::new(MockDatabase::new());
        let svc = DeleteService::new(Arc::clone(&storage), Arc::clone(&database));

        let owner = Uuid::new_v4();
        seed_dataset_with_data(&database, &storage, owner, "test_dataset").await;

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

        let still_exists = database
            .get_dataset_by_name("test_dataset", owner)
            .await
            .unwrap();
        assert!(still_exists.is_none(), "dataset should be gone");
    }

    // ------------------------------------------------------------------
    // Step 3 — test_delete_preview_does_not_mutate_state
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn preview_does_not_mutate_database_state() {
        let storage = Arc::new(MockStorage::new());
        let database = Arc::new(MockDatabase::new());
        let svc = DeleteService::new(Arc::clone(&storage), Arc::clone(&database));

        let owner = Uuid::new_v4();
        seed_dataset_with_data(&database, &storage, owner, "test_dataset").await;

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

        let still_exists = database
            .get_dataset_by_name("test_dataset", owner)
            .await
            .unwrap();
        assert!(
            still_exists.is_some(),
            "dataset should still exist after preview"
        );
        assert_eq!(
            database.get_data_count(),
            1,
            "data count should be unchanged"
        );
    }

    // ------------------------------------------------------------------
    // Step 4 — test_delete_missing_dataset_returns_validation_error
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn delete_nonexistent_dataset_returns_validation_error() {
        let svc = make_service();

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

    // ------------------------------------------------------------------
    // Step 5 — test_shared_data_not_deleted_while_linked_to_another_dataset
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn shared_data_not_deleted_while_linked_to_another_dataset() {
        let storage = Arc::new(MockStorage::new());
        let database = Arc::new(MockDatabase::new());
        let svc = DeleteService::new(Arc::clone(&storage), Arc::clone(&database));

        let owner = Uuid::new_v4();

        let ds1 = Dataset::new("dataset1".to_string(), owner);
        let ds2 = Dataset::new("dataset2".to_string(), owner);
        let ds1_id = ds1.id;
        let ds2_id = ds2.id;
        database.create_dataset(ds1).await.unwrap();
        database.create_dataset(ds2).await.unwrap();

        let location = storage
            .store(b"shared content", "shared.txt")
            .await
            .unwrap();
        let data_id = Uuid::new_v4();
        let data = Data::new(
            data_id,
            "shared.txt".to_string(),
            location,
            "file://shared.txt".to_string(),
            "txt".to_string(),
            "text/plain".to_string(),
            "shared_hash".to_string(),
            owner,
        );
        database.create_data(data).await.unwrap();
        database
            .attach_data_to_dataset(ds1_id, data_id)
            .await
            .unwrap();
        database
            .attach_data_to_dataset(ds2_id, data_id)
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

        let data_still_there = database.get_data(data_id).await.unwrap();
        assert!(data_still_there.is_some(), "data record must survive");
    }

    // ------------------------------------------------------------------
    // Step 6 — test_data_deleted_when_last_link_removed
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn data_deleted_when_last_dataset_link_removed() {
        let storage = Arc::new(MockStorage::new());
        let database = Arc::new(MockDatabase::new());
        let svc = DeleteService::new(Arc::clone(&storage), Arc::clone(&database));

        let owner = Uuid::new_v4();

        let ds1 = Dataset::new("dataset1".to_string(), owner);
        let ds2 = Dataset::new("dataset2".to_string(), owner);
        let ds1_id = ds1.id;
        let ds2_id = ds2.id;
        database.create_dataset(ds1).await.unwrap();
        database.create_dataset(ds2).await.unwrap();

        let location = storage
            .store(b"shared content", "shared.txt")
            .await
            .unwrap();
        let data_id = Uuid::new_v4();
        let data = Data::new(
            data_id,
            "shared.txt".to_string(),
            location,
            "file://shared.txt".to_string(),
            "txt".to_string(),
            "text/plain".to_string(),
            "shared_hash".to_string(),
            owner,
        );
        database.create_data(data).await.unwrap();
        database
            .attach_data_to_dataset(ds1_id, data_id)
            .await
            .unwrap();
        database
            .attach_data_to_dataset(ds2_id, data_id)
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

        let data_gone = database.get_data(data_id).await.unwrap();
        assert!(data_gone.is_none(), "data record must be gone");
    }

    // ------------------------------------------------------------------
    // Step 7 — test_delete_wrong_owner_returns_validation_error
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn delete_dataset_with_wrong_owner_returns_validation_error() {
        let storage = Arc::new(MockStorage::new());
        let database = Arc::new(MockDatabase::new());
        let svc = DeleteService::new(Arc::clone(&storage), Arc::clone(&database));

        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();

        seed_dataset_with_data(&database, &storage, owner_a, "owner_a_dataset").await;

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
}
