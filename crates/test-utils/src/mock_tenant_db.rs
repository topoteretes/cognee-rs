//! In-memory mock implementation of [`TenantDb`] for testing.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use cognee_database::{DatabaseError, TenantDb};
use cognee_models::{Tenant, User};
use uuid::Uuid;

/// A `HashMap`-backed mock tenant database for unit and integration tests.
///
/// Thread-safe via `Mutex`. Requires a reference to a `UserDb` for
/// `select_tenant` to work (via interior mutability).
pub struct MockTenantDb {
    tenants: Mutex<HashMap<Uuid, Tenant>>,
    /// Set of (user_id, tenant_id) membership pairs.
    memberships: Mutex<HashSet<(Uuid, Uuid)>>,
}

impl MockTenantDb {
    pub fn new() -> Self {
        Self {
            tenants: Mutex::new(HashMap::new()),
            memberships: Mutex::new(HashSet::new()),
        }
    }

    /// Return the number of tenants currently stored.
    pub fn tenant_count(&self) -> usize {
        let tenants = self.tenants.lock().unwrap(); // lock poison is unrecoverable
        tenants.len()
    }
}

impl Default for MockTenantDb {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TenantDb for MockTenantDb {
    async fn create_tenant(&self, tenant: &Tenant) -> Result<Tenant, DatabaseError> {
        let mut tenants = self.tenants.lock().unwrap(); // lock poison is unrecoverable
        if tenants.contains_key(&tenant.id) {
            return Err(DatabaseError::UniqueViolation(format!(
                "Tenant with id {} already exists",
                tenant.id
            )));
        }
        tenants.insert(tenant.id, tenant.clone());
        Ok(tenant.clone())
    }

    async fn get_tenant(&self, id: Uuid) -> Result<Option<Tenant>, DatabaseError> {
        let tenants = self.tenants.lock().unwrap(); // lock poison is unrecoverable
        Ok(tenants.get(&id).cloned())
    }

    async fn list_tenants_for_user(&self, user_id: Uuid) -> Result<Vec<Tenant>, DatabaseError> {
        let memberships = self.memberships.lock().unwrap(); // lock poison is unrecoverable
        let tenants = self.tenants.lock().unwrap(); // lock poison is unrecoverable
        let result: Vec<Tenant> = memberships
            .iter()
            .filter(|(uid, _)| *uid == user_id)
            .filter_map(|(_, tid)| tenants.get(tid).cloned())
            .collect();
        Ok(result)
    }

    async fn add_user_to_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), DatabaseError> {
        let mut memberships = self.memberships.lock().unwrap(); // lock poison is unrecoverable
        memberships.insert((user_id, tenant_id));
        Ok(())
    }

    async fn remove_user_from_tenant(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(), DatabaseError> {
        let mut memberships = self.memberships.lock().unwrap(); // lock poison is unrecoverable
        memberships.remove(&(user_id, tenant_id));
        Ok(())
    }

    async fn select_tenant(
        &self,
        _user_id: Uuid,
        _tenant_id: Option<Uuid>,
    ) -> Result<User, DatabaseError> {
        // The mock tenant DB does not have access to the user store.
        // In real implementations, this updates the user's tenant_id.
        // For testing, use the real DatabaseConnection or test manually.
        Err(DatabaseError::QueryError(
            "MockTenantDb::select_tenant is not implemented; use a real DB or test manually"
                .to_string(),
        ))
    }
}
