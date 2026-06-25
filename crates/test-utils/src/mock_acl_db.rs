//! In-memory mock implementation of [`AclDb`] for testing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "mock infrastructure — panics are acceptable"
)]

use std::collections::HashSet;
use std::sync::Mutex;

use async_trait::async_trait;
use cognee_database::{AclDb, DatabaseError};
use uuid::Uuid;

/// A `HashMap`-backed mock ACL database for unit and integration tests.
///
/// Thread-safe via `Mutex`. The inner store is a set of `(principal_id, dataset_id, permission_name)` tuples.
pub struct MockAclDb {
    /// Set of granted permissions: (principal_id, dataset_id, permission_name).
    grants: Mutex<HashSet<(Uuid, Uuid, String)>>,
    /// Set of principals: (principal_id, principal_type).
    principals: Mutex<HashSet<(Uuid, String)>>,
}

impl MockAclDb {
    pub fn new() -> Self {
        Self {
            grants: Mutex::new(HashSet::new()),
            principals: Mutex::new(HashSet::new()),
        }
    }

    /// Return the number of ACL grants currently stored.
    pub fn grant_count(&self) -> usize {
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.len()
    }

    /// Return the number of principals currently stored.
    pub fn principal_count(&self) -> usize {
        let principals = self.principals.lock().unwrap(); // lock poison is unrecoverable
        principals.len()
    }

    /// Check if a specific grant exists.
    pub fn has_grant(&self, principal_id: Uuid, dataset_id: Uuid, permission_name: &str) -> bool {
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.contains(&(principal_id, dataset_id, permission_name.to_string()))
    }

    /// Remove all grants for a specific dataset (simulates CASCADE DELETE).
    pub fn cascade_delete_dataset(&self, dataset_id: Uuid) {
        let mut grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.retain(|(_, ds_id, _)| *ds_id != dataset_id);
    }
}

impl Default for MockAclDb {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AclDb for MockAclDb {
    async fn has_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<bool, DatabaseError> {
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        Ok(grants.contains(&(principal_id, dataset_id, permission_name.to_string())))
    }

    async fn authorized_dataset_ids(
        &self,
        principal_id: Uuid,
        permission_name: &str,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        let grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        let ids: Vec<Uuid> = grants
            .iter()
            .filter(|(pid, _, pname)| *pid == principal_id && pname == permission_name)
            .map(|(_, ds_id, _)| *ds_id)
            .collect();
        Ok(ids)
    }

    async fn grant_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<(), DatabaseError> {
        let mut grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.insert((principal_id, dataset_id, permission_name.to_string()));
        Ok(())
    }

    async fn revoke_permission(
        &self,
        principal_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<(), DatabaseError> {
        let mut grants = self.grants.lock().unwrap(); // lock poison is unrecoverable
        grants.remove(&(principal_id, dataset_id, permission_name.to_string()));
        Ok(())
    }

    async fn ensure_principal(
        &self,
        principal_id: Uuid,
        principal_type: &str,
    ) -> Result<(), DatabaseError> {
        let mut principals = self.principals.lock().unwrap(); // lock poison is unrecoverable
        principals.insert((principal_id, principal_type.to_string()));
        Ok(())
    }

    async fn has_permission_with_roles(
        &self,
        user_id: Uuid,
        dataset_id: Uuid,
        permission_name: &str,
    ) -> Result<bool, DatabaseError> {
        // In the mock, just delegate to the direct check (no role/tenant resolution).
        self.has_permission(user_id, dataset_id, permission_name)
            .await
    }

    async fn authorized_dataset_ids_with_roles(
        &self,
        user_id: Uuid,
        permission_name: &str,
    ) -> Result<Vec<Uuid>, DatabaseError> {
        // In the mock, just delegate to the direct check (no role/tenant resolution).
        self.authorized_dataset_ids(user_id, permission_name).await
    }
}
