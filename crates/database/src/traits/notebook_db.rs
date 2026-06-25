//! `NotebookDb` trait — CRUD operations for the `notebooks` table.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::DatabaseError;

// ─── Notebook model ───────────────────────────────────────────────────────────

/// A single row from the `notebooks` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notebook {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    /// JSON array of notebook cells. Opaque `Value` — serialized/deserialized
    /// as-is so the router DTOs can parse the typed structure.
    pub cells: serde_json::Value,
    pub deletable: bool,
    pub created_at: DateTime<Utc>,
}

// ─── Update patch ─────────────────────────────────────────────────────────────

/// Fields that can be updated in a `PUT /{notebook_id}` call.
///
/// `None` means "leave the existing value unchanged" (mirrors Python's
/// truthiness-gated assignment).
#[derive(Debug, Clone, Default)]
pub struct NotebookUpdatePatch {
    pub name: Option<String>,
    pub cells: Option<serde_json::Value>,
}

// ─── NotebookDb trait ─────────────────────────────────────────────────────────

/// CRUD operations for the `notebooks` table.
#[async_trait]
pub trait NotebookDb: Send + Sync + 'static {
    /// Return all notebooks owned by `owner_id`, ordered by `created_at` asc.
    async fn list_by_owner(&self, owner_id: Uuid) -> Result<Vec<Notebook>, DatabaseError>;

    /// Insert a new notebook row and return it.
    ///
    /// The implementation generates a fresh `uuid4` id for the new row.
    async fn create(
        &self,
        owner_id: Uuid,
        name: String,
        cells: serde_json::Value,
        deletable: bool,
    ) -> Result<Notebook, DatabaseError>;

    /// Insert a notebook with a caller-supplied id (used by the tutorial seeder
    /// to guarantee deterministic UUID5 ids across SDK restarts).
    async fn create_seeded(
        &self,
        id: Uuid,
        owner_id: Uuid,
        name: String,
        cells: serde_json::Value,
        deletable: bool,
    ) -> Result<Notebook, DatabaseError>;

    /// Fetch a notebook by id, scoped to the owner.
    async fn get_by_id_and_owner(
        &self,
        id: Uuid,
        owner_id: Uuid,
    ) -> Result<Option<Notebook>, DatabaseError>;

    /// Apply a partial update.  Returns the updated row or `None` when not
    /// found (ownership check included).
    async fn update(
        &self,
        id: Uuid,
        owner_id: Uuid,
        patch: NotebookUpdatePatch,
    ) -> Result<Option<Notebook>, DatabaseError>;

    /// Delete a notebook.  Returns `true` if a row was actually removed.
    async fn delete(&self, id: Uuid, owner_id: Uuid) -> Result<bool, DatabaseError>;
}
