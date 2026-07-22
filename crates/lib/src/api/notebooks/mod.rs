//! `cognee::notebooks` — per-user notebook CRUD facade.
//!
//! Wraps `cognee_database::NotebookDb` with tutorial seeding and the Python
//! truthiness-bug compat notes documented in `docs/http-server/routers/notebooks.md`.

pub mod tutorial;

use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use cognee_database::{
    DatabaseError, Notebook, NotebookDb, NotebookUpdatePatch, seed_tutorials_if_first_call,
};

pub use tutorial::{TUTORIAL_BASICS_ID, TUTORIAL_PYTHON_DEV_ID};

// ─── NotebookError ────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum NotebookError {
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// List all notebooks for `user_id`.
///
/// On the very first call for a new user this seeds the two bundled tutorial
/// notebooks (idempotent — re-running is safe).
pub async fn list_notebooks(
    db: &Arc<dyn NotebookDb>,
    user_id: Uuid,
) -> Result<Vec<Notebook>, NotebookError> {
    seed_tutorials_if_first_call(db.as_ref(), user_id).await?;
    Ok(db.list_by_owner(user_id).await?)
}

/// Create a new notebook.
///
/// `deletable` is always forced to `true` regardless of the parameter value —
/// this replicates Python's `deletable=deletable or True` truthiness bug so
/// the HTTP surface is byte-identical to the Python SDK.
pub async fn create_notebook(
    db: &Arc<dyn NotebookDb>,
    user_id: Uuid,
    name: String,
    cells: serde_json::Value,
    _deletable: bool,
) -> Result<Notebook, NotebookError> {
    // Python's create_notebook always ends up with deletable=True due to the
    // `deletable or True` expression.  We replicate the bug for wire compat.
    Ok(db.create(user_id, name, cells, true).await?)
}

/// Update a notebook's name and/or cells.
///
/// Replicates Python's truthiness-gated assignment:
/// - `name` is only updated when `patch.name` is `Some` and non-empty.
/// - `cells` is only updated when `patch.cells` is `Some(Value::Array(v))` with
///   `v` non-empty — an empty cells list **does not clear cells**.
pub async fn update_notebook(
    db: &Arc<dyn NotebookDb>,
    id: Uuid,
    user_id: Uuid,
    patch: NotebookUpdatePatch,
) -> Result<Option<Notebook>, NotebookError> {
    Ok(db.update(id, user_id, patch).await?)
}

/// Delete a notebook.  Returns `true` if a row was removed.
pub async fn delete_notebook(
    db: &Arc<dyn NotebookDb>,
    id: Uuid,
    user_id: Uuid,
) -> Result<bool, NotebookError> {
    Ok(db.delete(id, user_id).await?)
}
