//! High-level dataset management facade.
//!
//! [`DatasetManager`] composes the existing `IngestDb`, `DeleteDb`, and `AclDb`
//! traits into a unified API matching the Python SDK's `datasets` class.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_database::ops::acl::grant_all_permissions_on_dataset_via_trait;
use cognee_database::{AclDb, DeleteDb, IngestDb, PipelineRunStatus};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteResult, DeleteScope, DeleteService};
use cognee_ingestion::generate_dataset_id;
use cognee_models::{Data, Dataset};
use uuid::Uuid;

use super::error::DatasetError;

// The canonical permission list lives in `cognee_database::ops::acl::PERMISSION_NAMES`,
// which `grant_all_permissions_on_dataset_via_trait` iterates. Tests below assert
// against that same list rather than a local copy that could drift from it.

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
            let authorized_ids = acl
                .authorized_dataset_ids_with_roles(owner_id, "read")
                .await?;
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
    ///
    /// Results are sorted by `data_size` descending (largest first), matching
    /// Python SDK behaviour.
    pub async fn list_data(
        &self,
        dataset_id: Uuid,
        owner_id: Uuid,
    ) -> Result<Vec<Data>, DatasetError> {
        self.check_read_permission(owner_id, dataset_id).await?;
        let mut items = self.db.get_dataset_data(dataset_id).await?;
        items.sort_by_key(|b| std::cmp::Reverse(b.data_size));
        Ok(items)
    }

    /// Check whether a dataset contains any data items.
    ///
    /// Enforces read permission when ACL is configured, then uses an efficient
    /// COUNT query instead of loading all records.
    pub async fn has_data(&self, dataset_id: Uuid, owner_id: Uuid) -> Result<bool, DatasetError> {
        self.check_read_permission(owner_id, dataset_id).await?;
        let count = self.db.count_dataset_data(dataset_id).await?;
        Ok(count > 0)
    }

    /// Get the latest pipeline status for each dataset, across all tracked pipelines.
    ///
    /// Returns a nested map `{ dataset_id → { pipeline_name → status } }`.
    /// Datasets with no pipeline runs for a given pipeline are omitted from the
    /// inner map (equivalent to Python's "not started" behaviour).
    pub async fn get_status(
        &self,
        dataset_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, HashMap<String, PipelineRunStatus>>, DatasetError> {
        const PIPELINES: &[&str] = &["add_pipeline", "cognify_pipeline"];
        let mut statuses: HashMap<Uuid, HashMap<String, PipelineRunStatus>> =
            HashMap::with_capacity(dataset_ids.len());
        for &id in dataset_ids {
            for pipeline_name in PIPELINES {
                if let Some(status) = self
                    .db
                    .get_latest_pipeline_status(pipeline_name, id)
                    .await?
                {
                    statuses
                        .entry(id)
                        .or_default()
                        .insert(pipeline_name.to_string(), status);
                }
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
            memory_only: false,
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
            memory_only: false,
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
                memory_only: false,
            };
            results.push(delete_service.execute(&request).await?);
        }
        Ok(results)
    }

    // ------------------------------------------------------------------
    // Create operations
    // ------------------------------------------------------------------

    /// Create a dataset with a deterministic ID matching Python's formula:
    ///   `uuid5(NAMESPACE_OID, f"{name}{user_id}{tenant_id}")`.
    ///
    /// Idempotent: if a dataset with the same deterministic ID already exists,
    /// returns the existing row.
    pub async fn create_dataset(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> Result<Dataset, DatasetError> {
        let id = generate_dataset_id(name, owner_id, tenant_id);
        // Try to get existing dataset first (idempotent create).
        if let Some(existing) = self.db.get_dataset(id).await? {
            return Ok(existing);
        }
        let dataset = Dataset::new(name.to_string(), owner_id, tenant_id, id);
        Ok(self.db.create_dataset(dataset).await?)
    }

    /// Create a dataset and grant all four ACL permissions (`read`, `write`,
    /// `delete`, `share`) to the owner.
    pub async fn create_authorized_dataset(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
        parent_user_id: Option<Uuid>,
    ) -> Result<Dataset, DatasetError> {
        let ds = self.create_dataset(name, owner_id, tenant_id).await?;
        let acl = self.acl_db.as_ref().ok_or(DatasetError::AclNotConfigured)?;

        // `grant_all_permissions_on_dataset_via_trait` ensures the principal
        // row exists before granting: the `acls.principal_id` FK references
        // `principals.id`, so a bare id would otherwise fail a foreign-key
        // constraint. Python's `give_permission_on_dataset` takes an
        // already-persisted `User`; this facade may be called with a bare id.
        // Both the upsert and the grants are idempotent.
        //
        // The same helper backs `POST /v1/datasets`
        // (`cognee_http_server::routers::datasets::create_new_dataset`), which
        // cannot call this facade — `cognee-http-server` deliberately does not
        // depend on `cognee`. One grant implementation is what keeps the two
        // create paths from drifting.
        grant_all_permissions_on_dataset_via_trait(acl.as_ref(), owner_id, ds.id).await?;
        if let Some(parent) = parent_user_id
            && parent != owner_id
        {
            grant_all_permissions_on_dataset_via_trait(acl.as_ref(), parent, ds.id).await?;
        }
        Ok(ds)
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
            && !acl
                .has_permission_with_roles(owner_id, dataset_id, "read")
                .await?
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
            && !acl
                .has_permission_with_roles(owner_id, dataset_id, "delete")
                .await?
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
        let loc = format!("file:///tmp/test/{id}.txt");
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

    fn make_data_with_size(owner_id: Uuid, size: i64) -> Data {
        let id = Uuid::new_v4();
        let loc = format!("file:///tmp/test/{id}.txt");
        Data::builder(
            id,
            "file.txt",
            loc.as_str(),
            loc.as_str(),
            "txt",
            "text/plain",
            format!("{:x}", Uuid::new_v4()),
            owner_id,
        )
        .data_size(size)
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
        assert!(!mgr.has_data(ds.id, owner_id).await.expect("has_data"));
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
        assert!(mgr.has_data(ds.id, owner_id).await.expect("has_data"));
    }

    #[tokio::test]
    async fn test_has_data_permission_denied_with_acl() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let ds = make_dataset(owner_id);

        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");

        // Grant read permission to owner only (via ACL). The OSS test path
        // uses `MockAclDb` because the closed `cognee-access-control` crate
        // (which provides `AclDb for DatabaseConnection`) is not present.
        let acl: Arc<dyn AclDb> = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.ensure_principal(owner_id, "user")
            .await
            .expect("ensure_principal");
        acl.grant_permission(owner_id, ds.id, "read")
            .await
            .expect("grant_permission");

        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>).with_acl(acl);

        // Owner can check — should succeed.
        assert!(
            mgr.has_data(ds.id, owner_id).await.is_ok(),
            "owner must be able to call has_data"
        );

        // Other user gets PermissionDenied.
        let err = mgr
            .has_data(ds.id, other_id)
            .await
            .expect_err("must fail for unauthorized user");
        assert!(
            matches!(err, DatasetError::PermissionDenied),
            "expected PermissionDenied, got {err:?}"
        );
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
    async fn test_list_data_sorted_by_size_descending() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let ds = make_dataset(owner_id);

        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");

        // Create three data items with distinct sizes.
        let small = make_data_with_size(owner_id, 10);
        let large = make_data_with_size(owner_id, 1000);
        let medium = make_data_with_size(owner_id, 500);

        for d in [&small, &large, &medium] {
            ingest.create_data(d.clone()).await.expect("create_data");
            ingest
                .attach_data_to_dataset(ds.id, d.id)
                .await
                .expect("attach_data");
        }

        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);
        let items = mgr.list_data(ds.id, owner_id).await.expect("list_data");
        assert_eq!(items.len(), 3);
        // Must be sorted largest first.
        assert_eq!(items[0].id, large.id, "largest must come first");
        assert_eq!(items[1].id, medium.id, "medium second");
        assert_eq!(items[2].id, small.id, "smallest last");
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
        // No pipeline runs recorded → the outer map should be empty.
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

    #[tokio::test]
    async fn test_create_dataset_deterministic_id() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let tenant_id = Some(Uuid::new_v4());
        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);

        let ds = mgr
            .create_dataset("my-ds", owner_id, tenant_id)
            .await
            .expect("create_dataset");

        let expected_id = generate_dataset_id("my-ds", owner_id, tenant_id);
        assert_eq!(ds.id, expected_id, "ID must match generate_dataset_id");
        assert_eq!(ds.name, "my-ds");
        assert_eq!(ds.owner_id, owner_id);
    }

    #[tokio::test]
    async fn test_create_dataset_idempotent() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>);

        let ds1 = mgr
            .create_dataset("dup-ds", owner_id, None)
            .await
            .expect("first create");
        let ds2 = mgr
            .create_dataset("dup-ds", owner_id, None)
            .await
            .expect("second create");

        assert_eq!(ds1.id, ds2.id, "Idempotent: same ID returned on duplicate");

        // Ensure only one row exists
        let list = IngestDb::list_datasets_by_owner(db.as_ref(), owner_id)
            .await
            .unwrap();
        assert_eq!(list.len(), 1, "Only one row should exist");
    }

    #[tokio::test]
    async fn test_create_authorized_dataset_without_acl_errors() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let mgr = DatasetManager::new(db as Arc<dyn DatasetDb>);

        let result = mgr
            .create_authorized_dataset("auth-ds", owner_id, None, None)
            .await;
        assert!(
            matches!(result, Err(DatasetError::AclNotConfigured)),
            "Should error when ACL not configured"
        );
    }

    #[tokio::test]
    async fn test_create_authorized_dataset_grants_four_permissions() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let parent_id = Uuid::new_v4();
        let acl: Arc<dyn AclDb> = Arc::new(cognee_test_utils::MockAclDb::new());
        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>).with_acl(acl.clone());

        let ds = mgr
            .create_authorized_dataset("auth-ds", owner_id, None, Some(parent_id))
            .await
            .expect("create_authorized_dataset");

        // Owner and parent both receive all four permissions on the dataset.
        for perm in cognee_database::ops::acl::PERMISSION_NAMES {
            assert!(
                acl.has_permission(owner_id, ds.id, perm).await.unwrap(),
                "owner must have '{perm}'"
            );
            assert!(
                acl.has_permission(parent_id, ds.id, perm).await.unwrap(),
                "parent must have '{perm}'"
            );
        }
    }

    #[tokio::test]
    async fn test_create_authorized_dataset_parent_equals_owner_no_duplicate() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let acl: Arc<dyn AclDb> = Arc::new(cognee_test_utils::MockAclDb::new());
        let mgr = DatasetManager::new(db.clone() as Arc<dyn DatasetDb>).with_acl(acl.clone());

        // parent_user_id == owner_id: must succeed (idempotent) and grant once.
        let ds = mgr
            .create_authorized_dataset("auth-ds-self", owner_id, None, Some(owner_id))
            .await
            .expect("create_authorized_dataset with self-parent should succeed");

        for perm in cognee_database::ops::acl::PERMISSION_NAMES {
            assert!(
                acl.has_permission(owner_id, ds.id, perm).await.unwrap(),
                "owner must have '{perm}'"
            );
        }
    }

    // ------------------------------------------------------------------
    // Role / tenant inheritance (issue #206)
    //
    // Python's `datasets.list_datasets` → `get_authorized_existing_datasets
    // ([], "read", user)` and `get_authorized_dataset(user, id, perm)` walk
    // tenant and role grants. These tests grant to a role/tenant id and call
    // as the member; with the plain `AclDb` variants they go red.
    // ------------------------------------------------------------------

    /// Scenario: one dataset granted `read` directly, one only via a role,
    /// one not at all. Expected: `list_datasets` returns exactly the first two
    /// (exercises `authorized_dataset_ids_with_roles`).
    #[tokio::test]
    async fn acl_list_datasets_includes_role_granted_dataset() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let member = Uuid::new_v4();
        let role = Uuid::new_v4();
        let direct = make_dataset(owner_id);
        let via_role = make_dataset(owner_id);
        let ungranted = make_dataset(owner_id);
        let ingest: &dyn IngestDb = db.as_ref();
        for ds in [&direct, &via_role, &ungranted] {
            ingest
                .create_dataset(ds.clone())
                .await
                .expect("create_dataset");
        }

        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        // A role only confers grants within a tenant the user belongs to
        // (AclDb's contract, and Python nests the role walk in the tenant loop).
        acl.add_user_to_tenant(member, Uuid::new_v4());
        acl.add_user_to_role(member, role);
        acl.grant_permission(member, direct.id, "read")
            .await
            .expect("grant direct");
        acl.grant_permission(role, via_role.id, "read")
            .await
            .expect("grant role");

        let mgr =
            DatasetManager::new(db.clone() as Arc<dyn DatasetDb>).with_acl(acl as Arc<dyn AclDb>);
        let mut listed: Vec<Uuid> = mgr
            .list_datasets(member)
            .await
            .expect("list_datasets")
            .into_iter()
            .map(|d| d.id)
            .collect();
        listed.sort();
        let mut expected = vec![direct.id, via_role.id];
        expected.sort();
        assert_eq!(listed, expected, "direct + role grants, nothing else");
    }

    /// Scenario: `read` granted only to a tenant the caller belongs to.
    /// Expected: `has_data` / `list_data` succeed (exercises
    /// `check_read_permission` → `has_permission_with_roles`).
    #[tokio::test]
    async fn acl_read_grant_via_tenant_admits_has_data_and_list_data() {
        let db = fresh_db().await;
        let owner_id = Uuid::new_v4();
        let member = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let ds = make_dataset(owner_id);
        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");

        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.add_user_to_tenant(member, tenant);
        acl.grant_permission(tenant, ds.id, "read")
            .await
            .expect("grant tenant");

        let mgr =
            DatasetManager::new(db.clone() as Arc<dyn DatasetDb>).with_acl(acl as Arc<dyn AclDb>);
        assert!(
            mgr.has_data(ds.id, member).await.is_ok(),
            "tenant-held read must admit has_data"
        );
        assert!(
            mgr.list_data(ds.id, member).await.is_ok(),
            "tenant-held read must admit list_data"
        );
    }

    /// Scenario: the dataset owner holds `delete` only through a role (no
    /// direct grant — e.g. revoked), and a second user holds `read` through
    /// another role. Expected: the owner's `empty_dataset` passes the gate
    /// (exercises `check_delete_permission` → `has_permission_with_roles`);
    /// the reader is denied.
    ///
    /// The caller is made the owner deliberately: `empty_dataset` feeds the
    /// caller id into `DeleteScope::Dataset { owner_id }`, so the inner
    /// `DeleteService` resolves the dataset by `(name, caller)` and a
    /// non-owner would fail *after* the gate with `Validation(not found)`.
    #[tokio::test]
    async fn acl_delete_grant_via_role_admits_empty_dataset() {
        use cognee_database::DeleteDb;
        let db = fresh_db().await;
        let member = Uuid::new_v4();
        let reader = Uuid::new_v4();
        let deleter_role = Uuid::new_v4();
        let reader_role = Uuid::new_v4();
        let ds = make_dataset(member);
        let ingest: &dyn IngestDb = db.as_ref();
        ingest
            .create_dataset(ds.clone())
            .await
            .expect("create_dataset");

        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        // A role only confers grants within a tenant the user belongs to
        // (AclDb's contract, and Python nests the role walk in the tenant loop).
        acl.add_user_to_tenant(member, Uuid::new_v4());
        acl.add_user_to_tenant(reader, Uuid::new_v4());
        acl.add_user_to_role(member, deleter_role);
        acl.add_user_to_role(reader, reader_role);
        acl.grant_permission(deleter_role, ds.id, "delete")
            .await
            .expect("grant delete to role");
        acl.grant_permission(reader_role, ds.id, "read")
            .await
            .expect("grant read to role");

        let storage = Arc::new(cognee_storage::MockStorage::new());
        let delete_service = DeleteService::new(
            storage as Arc<dyn cognee_storage::StorageTrait>,
            db.clone() as Arc<dyn DeleteDb>,
        );
        let mgr =
            DatasetManager::new(db.clone() as Arc<dyn DatasetDb>).with_acl(acl as Arc<dyn AclDb>);

        let err = mgr
            .empty_dataset(ds.id, reader, &delete_service)
            .await
            .expect_err("a role-held read grant must not authorize delete");
        assert!(
            matches!(err, DatasetError::PermissionDenied),
            "expected PermissionDenied, got {err:?}"
        );

        mgr.empty_dataset(ds.id, member, &delete_service)
            .await
            .expect("a role-held delete grant must authorize empty_dataset");
    }
}
