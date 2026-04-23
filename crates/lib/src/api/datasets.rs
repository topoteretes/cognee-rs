//! High-level dataset management facade.
//!
//! [`DatasetManager`] composes the existing `IngestDb`, `DeleteDb`, and `AclDb`
//! traits into a unified API matching the Python SDK's `datasets` class.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_database::{AclDb, DeleteDb, IngestDb, PipelineRunStatus};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteResult, DeleteScope, DeleteService};
use cognee_models::{Data, Dataset};
use uuid::Uuid;

use super::error::DatasetError;

/// Combined trait for dataset operations.
///
/// Any `DatabaseConnection` implements both `IngestDb` and `DeleteDb`,
/// so it automatically satisfies this super-trait.
pub trait DatasetDb: IngestDb + DeleteDb + Send + Sync {}
impl<T: IngestDb + DeleteDb + Send + Sync> DatasetDb for T {}

/// High-level facade for dataset CRUD operations.
///
/// Wraps the low-level DB traits with optional ACL enforcement, matching
/// the Python SDK's `datasets` class.
pub struct DatasetManager {
    db: Arc<dyn DatasetDb>,
    acl_db: Option<Arc<dyn AclDb>>,
}

impl DatasetManager {
    /// Create a new `DatasetManager` without ACL enforcement.
    pub fn new(db: Arc<dyn DatasetDb>) -> Self {
        Self { db, acl_db: None }
    }

    /// Enable ACL enforcement using the given ACL database.
    pub fn with_acl(mut self, acl_db: Arc<dyn AclDb>) -> Self {
        self.acl_db = Some(acl_db);
        self
    }

    // ------------------------------------------------------------------
    // Read operations
    // ------------------------------------------------------------------

    /// List all datasets accessible to the given owner.
    ///
    /// When ACL is configured, only datasets the owner has "read" permission
    /// on are returned. Without ACL, all datasets owned by the user are listed.
    pub async fn list_datasets(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatasetError> {
        if let Some(acl) = &self.acl_db {
            let authorized_ids = acl.authorized_dataset_ids(owner_id, "read").await?;
            let mut datasets = Vec::with_capacity(authorized_ids.len());
            for id in authorized_ids {
                if let Some(ds) = self.db.get_dataset(id).await? {
                    datasets.push(ds);
                }
            }
            Ok(datasets)
        } else {
            Ok(IngestDb::list_datasets_by_owner(self.db.as_ref(), owner_id).await?)
        }
    }

    /// List all data items in a dataset, with permission check.
    pub async fn list_data(
        &self,
        dataset_id: Uuid,
        owner_id: Uuid,
    ) -> Result<Vec<Data>, DatasetError> {
        self.check_read_permission(owner_id, dataset_id).await?;
        Ok(self.db.get_dataset_data(dataset_id).await?)
    }

    /// Check whether a dataset contains any data items.
    ///
    /// Uses an efficient COUNT query instead of loading all records.
    pub async fn has_data(&self, dataset_id: Uuid) -> Result<bool, DatasetError> {
        let count = self.db.count_dataset_data(dataset_id).await?;
        Ok(count > 0)
    }

    /// Get the latest cognify pipeline status for each dataset.
    ///
    /// Datasets with no pipeline runs are omitted from the result map
    /// (equivalent to Python's "not started" behavior).
    pub async fn get_status(
        &self,
        dataset_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DatasetError> {
        let mut statuses = HashMap::with_capacity(dataset_ids.len());
        for &id in dataset_ids {
            if let Some(status) = self
                .db
                .get_latest_pipeline_status("cognify_pipeline", id)
                .await?
            {
                statuses.insert(id, status);
            }
        }
        Ok(statuses)
    }

    /// Scan a filesystem directory for dataset-like sub-directories.
    ///
    /// Returns the names of immediate child directories. This is a sync
    /// utility matching the Python SDK's `discover_datasets` method.
    pub fn discover_datasets(
        directory_path: &std::path::Path,
    ) -> Result<Vec<String>, DatasetError> {
        let mut datasets = Vec::new();
        for entry in std::fs::read_dir(directory_path)? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                datasets.push(name.to_owned());
            }
        }
        Ok(datasets)
    }

    // ------------------------------------------------------------------
    // Write / delete operations
    // ------------------------------------------------------------------

    /// Delete all data in a dataset (and the dataset record itself).
    ///
    /// Delegates to `DeleteService` with `DeleteScope::Dataset`.
    pub async fn empty_dataset(
        &self,
        dataset_id: Uuid,
        owner_id: Uuid,
        delete_service: &DeleteService,
    ) -> Result<DeleteResult, DatasetError> {
        let dataset = self.require_dataset(dataset_id).await?;
        self.check_delete_permission(owner_id, dataset_id).await?;
        let request = DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id,
                dataset_name: dataset.name,
            },
            mode: DeleteMode::Hard,
        };
        Ok(delete_service.execute(&request).await?)
    }

    /// Delete a specific data item from a dataset.
    ///
    /// Delegates to `DeleteService` with `DeleteScope::Data`.
    pub async fn delete_data(
        &self,
        dataset_id: Uuid,
        data_id: Uuid,
        owner_id: Uuid,
        mode: DeleteMode,
        delete_dataset_if_empty: bool,
        delete_service: &DeleteService,
    ) -> Result<DeleteResult, DatasetError> {
        let dataset = self.require_dataset(dataset_id).await?;
        self.check_delete_permission(owner_id, dataset_id).await?;
        let request = DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name: Some(dataset.name),
                delete_dataset_if_empty,
            },
            mode,
        };
        Ok(delete_service.execute(&request).await?)
    }

    /// Delete all datasets for an owner.
    ///
    /// Lists all accessible datasets and delegates each to `DeleteService`.
    pub async fn delete_all(
        &self,
        owner_id: Uuid,
        delete_service: &DeleteService,
    ) -> Result<Vec<DeleteResult>, DatasetError> {
        let datasets = self.list_datasets(owner_id).await?;
        let mut results = Vec::with_capacity(datasets.len());
        for ds in datasets {
            let request = DeleteRequest {
                scope: DeleteScope::Dataset {
                    owner_id,
                    dataset_name: ds.name,
                },
                mode: DeleteMode::Hard,
            };
            results.push(delete_service.execute(&request).await?);
        }
        Ok(results)
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    async fn check_read_permission(
        &self,
        owner_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatasetError> {
        if let Some(acl) = &self.acl_db
            && !acl.has_permission(owner_id, dataset_id, "read").await?
        {
            return Err(DatasetError::PermissionDenied);
        }
        Ok(())
    }

    async fn check_delete_permission(
        &self,
        owner_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DatasetError> {
        if let Some(acl) = &self.acl_db
            && !acl.has_permission(owner_id, dataset_id, "delete").await?
        {
            return Err(DatasetError::PermissionDenied);
        }
        Ok(())
    }

    async fn require_dataset(&self, id: Uuid) -> Result<Dataset, DatasetError> {
        self.db.get_dataset(id).await?.ok_or(DatasetError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_database::{connect, initialize};
    use cognee_models::{Data, Dataset};
    use uuid::Uuid;

    /// Create a fresh in-memory SQLite database with migrations applied.
    async fn fresh_db() -> Arc<cognee_database::DatabaseConnection> {
        let db = connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite always connects");
        initialize(&db)
            .await
            .expect("migrations succeed on empty DB");
        Arc::new(db)
    }

    fn make_dataset(owner_id: Uuid) -> Dataset {
        Dataset::new(
            format!("test-dataset-{}", Uuid::new_v4()),
            owner_id,
            None,
            Uuid::new_v4(),
        )
    }

    fn make_data(owner_id: Uuid) -> Data {
        let id = Uuid::new_v4();
        let loc = format!("file:///tmp/test/{}.txt", id);
        Data::builder(
            id,
            "test-data.txt",
            loc.as_str(),
            loc.as_str(),
            "txt",
            "text/plain",
            format!("{:x}", Uuid::new_v4()),
            owner_id,
        )
        .build()
    }

    #[tokio::test]
    async fn test_list_datasets_no_acl() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let ds = make_dataset(owner_id);

        // Insert dataset directly via IngestDb
        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");

        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);
        let result = mgr.list_datasets(owner_id).await.expect("list_datasets");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, ds.id);
    }

    #[tokio::test]
    async fn test_list_datasets_different_owner() {
        let db = fresh_db().await;
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();

        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(make_dataset(owner_a))
            .await
            .expect("create_dataset");
        ingest
            .create_dataset(make_dataset(owner_b))
            .await
            .expect("create_dataset");

        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);
        let result_a = mgr.list_datasets(owner_a).await.expect("list_datasets");
        assert_eq!(result_a.len(), 1);
        let result_b = mgr.list_datasets(owner_b).await.expect("list_datasets");
        assert_eq!(result_b.len(), 1);
    }

    #[tokio::test]
    async fn test_has_data_empty_dataset() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let ds = make_dataset(owner_id);

        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");

        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);
        assert!(!mgr.has_data(ds.id).await.expect("has_data"));
    }

    #[tokio::test]
    async fn test_has_data_with_data() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let ds = make_dataset(owner_id);
        let data = make_data(owner_id);

        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");
        ingest.create_data(data.clone()).await.expect("create_data");
        ingest
            .attach_data_to_dataset(ds.id, data.id)
            .await
            .expect("attach_data");

        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);
        assert!(mgr.has_data(ds.id).await.expect("has_data"));
    }

    #[tokio::test]
    async fn test_list_data() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let ds = make_dataset(owner_id);
        let data = make_data(owner_id);

        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");
        ingest.create_data(data.clone()).await.expect("create_data");
        ingest
            .attach_data_to_dataset(ds.id, data.id)
            .await
            .expect("attach_data");

        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);
        let items = mgr.list_data(ds.id, owner_id).await.expect("list_data");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, data.id);
    }

    #[tokio::test]
    async fn test_get_status_no_runs() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let ds = make_dataset(owner_id);

        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");

        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);
        let statuses = mgr.get_status(&[ds.id]).await.expect("get_status");
        assert!(statuses.is_empty());
    }

    #[tokio::test]
    async fn test_discover_datasets() {
        let tmpdir = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir(tmpdir.path().join("dataset-a")).expect("create dir");
        std::fs::create_dir(tmpdir.path().join("dataset-b")).expect("create dir");
        // Create a file to verify it's excluded
        std::fs::write(tmpdir.path().join("not-a-dataset.txt"), "hello").expect("create file");

        let mut result =
            DatasetManager::discover_datasets(tmpdir.path()).expect("discover_datasets");
        result.sort();
        assert_eq!(result, vec!["dataset-a", "dataset-b"]);
    }

    #[tokio::test]
    async fn test_require_dataset_not_found() {
        let db = fresh_db().await;
        let mgr = DatasetManager::new(db as Arc<dyn DatasetDb>);
        let err = mgr.require_dataset(Uuid::new_v4()).await;
        assert!(matches!(err, Err(DatasetError::NotFound)));
    }
}
