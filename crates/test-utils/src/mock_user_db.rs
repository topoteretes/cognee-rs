//! In-memory mock implementation of [`UserDb`] for testing.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use cognee_database::{DatabaseError, UserDb};
use cognee_models::User;
use uuid::Uuid;

/// A `HashMap`-backed mock user database for unit and integration tests.
///
/// Thread-safe via `Mutex`.
pub struct MockUserDb {
    users: Mutex<HashMap<Uuid, User>>,
}

impl MockUserDb {
    pub fn new() -> Self {
        Self {
            users: Mutex::new(HashMap::new()),
        }
    }

    /// Return the number of users currently stored.
    pub fn user_count(&self) -> usize {
        let users = self.users.lock().unwrap(); // lock poison is unrecoverable
        users.len()
    }
}

impl Default for MockUserDb {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UserDb for MockUserDb {
    async fn get_user(&self, id: Uuid) -> Result<Option<User>, DatabaseError> {
        let users = self.users.lock().unwrap(); // lock poison is unrecoverable
        Ok(users.get(&id).cloned())
    }

    async fn get_user_by_email(&self, email: &str) -> Result<Option<User>, DatabaseError> {
        let users = self.users.lock().unwrap(); // lock poison is unrecoverable
        Ok(users.values().find(|u| u.email == email).cloned())
    }

    async fn create_user(&self, user: &User) -> Result<User, DatabaseError> {
        let mut users = self.users.lock().unwrap(); // lock poison is unrecoverable
        if users.contains_key(&user.id) {
            return Err(DatabaseError::UniqueViolation(format!(
                "User with id {} already exists",
                user.id
            )));
        }
        if users.values().any(|u| u.email == user.email) {
            return Err(DatabaseError::UniqueViolation(format!(
                "User with email {} already exists",
                user.email
            )));
        }
        users.insert(user.id, user.clone());
        Ok(user.clone())
    }

    async fn update_user(&self, user: &User) -> Result<User, DatabaseError> {
        let mut users = self.users.lock().unwrap(); // lock poison is unrecoverable
        if !users.contains_key(&user.id) {
            return Err(DatabaseError::NotFound(format!(
                "User '{}' not found",
                user.id
            )));
        }
        users.insert(user.id, user.clone());
        Ok(user.clone())
    }

    async fn delete_user(&self, id: Uuid) -> Result<(), DatabaseError> {
        let mut users = self.users.lock().unwrap(); // lock poison is unrecoverable
        users.remove(&id);
        Ok(())
    }

    async fn list_users(&self, tenant_id: Option<Uuid>) -> Result<Vec<User>, DatabaseError> {
        let users = self.users.lock().unwrap(); // lock poison is unrecoverable
        let result: Vec<User> = users
            .values()
            .filter(|u| match tenant_id {
                Some(tid) => u.tenant_id == Some(tid),
                None => true,
            })
            .cloned()
            .collect();
        Ok(result)
    }
}
