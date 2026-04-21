//! ACL-enforcing wrapper around [`DeleteService`].
//!
//! [`AuthorizedDeleteService`] checks that the calling principal holds the
//! `"delete"` permission on each target dataset before delegating to the
//! inner [`DeleteService`]. The plain `DeleteService` remains available for
//! edge/embedded deployments that do not require ACL enforcement.

use std::sync::Arc;

use cognee_database::{AclDb, DeleteDb};
use uuid::Uuid;

use crate::{DeleteError, DeletePreview, DeleteRequest, DeleteResult, DeleteScope, DeleteService};

/// Authorization-enforcing delete service.
///
/// Wraps a [`DeleteService`] and an [`AclDb`] implementation. Before every
/// `execute()` or `preview()` call the wrapper verifies that `principal_id`
/// holds `"delete"` permission on all affected datasets.
pub struct AuthorizedDeleteService {
    inner: DeleteService,
    acl_db: Arc<dyn AclDb>,
    database: Arc<dyn DeleteDb>,
}

impl AuthorizedDeleteService {
    /// Create a new authorized delete service.
    ///
    /// `database` must be the same connection used to construct the inner
    /// `DeleteService`, so that dataset resolution is consistent.
    pub fn new(inner: DeleteService, acl_db: Arc<dyn AclDb>, database: Arc<dyn DeleteDb>) -> Self {
        Self {
            inner,
            acl_db,
            database,
        }
    }

    /// Preview the deletion, checking ACL first.
    pub async fn preview(
        &self,
        request: &DeleteRequest,
        principal_id: Uuid,
    ) -> Result<DeletePreview, DeleteError> {
        self.check_authorization(request, principal_id).await?;
        self.inner.preview(request).await
    }

    /// Execute the deletion, checking ACL first.
    pub async fn execute(
        &self,
        request: &DeleteRequest,
        principal_id: Uuid,
    ) -> Result<DeleteResult, DeleteError> {
        self.check_authorization(request, principal_id).await?;
        self.inner.execute(request).await
    }

    /// Verify that `principal_id` has "delete" permission on all datasets
    /// that would be affected by the request.
    async fn check_authorization(
        &self,
        request: &DeleteRequest,
        principal_id: Uuid,
    ) -> Result<(), DeleteError> {
        match &request.scope {
            DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name,
                ..
            } => {
                if let Some(ds_name) = dataset_name {
                    let dataset_id = self.resolve_dataset_id(*owner_id, ds_name).await?;
                    self.require_delete_permission(principal_id, dataset_id)
                        .await?;
                } else {
                    // When no dataset_name is given, the inner service resolves
                    // all datasets linked to this data item that belong to the
                    // owner. We must check that the principal has delete
                    // permission on each of those datasets.
                    let datasets = self
                        .database
                        .list_datasets_for_data(*data_id)
                        .await
                        .map_err(|e| {
                            DeleteError::Runtime(format!(
                                "Failed to list datasets for data {data_id}: {e}"
                            ))
                        })?;

                    for ds in &datasets {
                        if ds.owner_id == *owner_id {
                            self.require_delete_permission(principal_id, ds.id).await?;
                        }
                    }
                }
            }
            DeleteScope::Dataset {
                owner_id,
                dataset_name,
            } => {
                let dataset_id = self.resolve_dataset_id(*owner_id, dataset_name).await?;
                self.require_delete_permission(principal_id, dataset_id)
                    .await?;
            }
            DeleteScope::User { owner_id } => {
                // For user-scoped deletion, verify the principal has delete
                // on all datasets that would be affected. The inner service
                // will list all datasets by owner. We check that the
                // principal's authorized set covers them.
                let authorized = self
                    .acl_db
                    .authorized_dataset_ids(principal_id, "delete")
                    .await
                    .map_err(|e| DeleteError::Runtime(format!("ACL query failed: {e}")))?;

                let owner_datasets = self
                    .database
                    .list_datasets_by_owner(*owner_id)
                    .await
                    .map_err(|e| {
                        DeleteError::Runtime(format!("Failed to list owner datasets: {e}"))
                    })?;

                for ds in &owner_datasets {
                    if !authorized.contains(&ds.id) {
                        return Err(DeleteError::PermissionDenied(format!(
                            "Principal {} does not have 'delete' permission on dataset '{}'",
                            principal_id, ds.name
                        )));
                    }
                }
            }
            DeleteScope::All => {
                // All-scope is an administrative operation. We check that
                // the principal has delete permission on every dataset that
                // exists. If any dataset lacks the permission, deny.
                let authorized = self
                    .acl_db
                    .authorized_dataset_ids(principal_id, "delete")
                    .await
                    .map_err(|e| DeleteError::Runtime(format!("ACL query failed: {e}")))?;

                let all_datasets = self.database.list_datasets().await.map_err(|e| {
                    DeleteError::Runtime(format!("Failed to list all datasets: {e}"))
                })?;

                for ds in &all_datasets {
                    if !authorized.contains(&ds.id) {
                        return Err(DeleteError::PermissionDenied(format!(
                            "Principal {} does not have 'delete' permission on dataset '{}'",
                            principal_id, ds.name
                        )));
                    }
                }
            }
        }

        Ok(())
    }

    /// Look up a dataset by name + owner and return its ID.
    async fn resolve_dataset_id(
        &self,
        owner_id: Uuid,
        dataset_name: &str,
    ) -> Result<Uuid, DeleteError> {
        let dataset = self
            .database
            .get_dataset_by_name(dataset_name, owner_id)
            .await
            .map_err(|e| {
                DeleteError::Runtime(format!("Failed to resolve dataset '{}': {e}", dataset_name))
            })?
            .ok_or_else(|| {
                DeleteError::Validation(format!(
                    "Dataset '{}' was not found for owner {}",
                    dataset_name, owner_id
                ))
            })?;

        Ok(dataset.id)
    }

    /// Check that the principal has "delete" permission on the dataset.
    async fn require_delete_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
    ) -> Result<(), DeleteError> {
        let has_perm = self
            .acl_db
            .has_permission(principal_id, dataset_id, "delete")
            .await
            .map_err(|e| DeleteError::Runtime(format!("ACL check failed: {e}")))?;

        if !has_perm {
            return Err(DeleteError::PermissionDenied(format!(
                "Principal {} does not have 'delete' permission on dataset {}",
                principal_id, dataset_id
            )));
        }

        Ok(())
    }
}
