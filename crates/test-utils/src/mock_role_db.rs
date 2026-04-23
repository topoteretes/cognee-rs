//! In-memory mock implementation of [`RoleDb`] for testing.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use cognee_database::{DatabaseError, RoleDb};
use cognee_models::Role;
use uuid::Uuid;

/// A `HashMap`-backed mock role database for unit and integration tests.
///
/// Thread-safe via `Mutex`.
pub struct MockRoleDb {
    roles: Mutex<HashMap<Uuid, Role>>,
    /// Set of (user_id, role_id) assignment pairs.
    assignments: Mutex<HashSet<(Uuid, Uuid)>>,
}

impl MockRoleDb {
    pub fn new() -> Self {
        Self {
            roles: Mutex::new(HashMap::new()),
            assignments: Mutex::new(HashSet::new()),
        }
    }

    /// Return the number of roles currently stored.
    pub fn role_count(&self) -> usize {
        let roles = self.roles.lock().unwrap(); // lock poison is unrecoverable
        roles.len()
    }
}

impl Default for MockRoleDb {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RoleDb for MockRoleDb {
    async fn create_role(&self, role: &Role) -> Result<Role, DatabaseError> {
        let mut roles = self.roles.lock().unwrap(); // lock poison is unrecoverable
        if roles.contains_key(&role.id) {
            return Err(DatabaseError::UniqueViolation(format!(
                "Role with id {} already exists",
                role.id
            )));
        }
        roles.insert(role.id, role.clone());
        Ok(role.clone())
    }

    async fn get_role(&self, id: Uuid) -> Result<Option<Role>, DatabaseError> {
        let roles = self.roles.lock().unwrap(); // lock poison is unrecoverable
        Ok(roles.get(&id).cloned())
    }

    async fn list_roles_in_tenant(&self, tenant_id: Uuid) -> Result<Vec<Role>, DatabaseError> {
        let roles = self.roles.lock().unwrap(); // lock poison is unrecoverable
        let result: Vec<Role> = roles
            .values()
            .filter(|r| r.tenant_id == tenant_id)
            .cloned()
            .collect();
        Ok(result)
    }

    async fn assign_user_to_role(&self, user_id: Uuid, role_id: Uuid) -> Result<(), DatabaseError> {
        let mut assignments = self.assignments.lock().unwrap(); // lock poison is unrecoverable
        assignments.insert((user_id, role_id));
        Ok(())
    }

    async fn remove_user_from_role(
        &self,
        user_id: Uuid,
        role_id: Uuid,
    ) -> Result<(), DatabaseError> {
        let mut assignments = self.assignments.lock().unwrap(); // lock poison is unrecoverable
        assignments.remove(&(user_id, role_id));
        Ok(())
    }

    async fn get_user_roles(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
    ) -> Result<Vec<Role>, DatabaseError> {
        let assignments = self.assignments.lock().unwrap(); // lock poison is unrecoverable
        let roles = self.roles.lock().unwrap(); // lock poison is unrecoverable
        let result: Vec<Role> = assignments
            .iter()
            .filter(|(uid, _)| *uid == user_id)
            .filter_map(|(_, rid)| roles.get(rid).cloned())
            .filter(|r| r.tenant_id == tenant_id)
            .collect();
        Ok(result)
    }
}
