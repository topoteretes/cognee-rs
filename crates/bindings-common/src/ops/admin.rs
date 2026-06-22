//! Shared async admin and notebook operations:
//! `reset_pipeline_run_status`, `reset_dataset_pipeline_run_status`,
//! `get_or_create_default_user`, `list_notebooks`, `create_notebook`,
//! `update_notebook`, `delete_notebook`.
//!
//! These functions contain the pure-Rust async logic shared between every
//! language binding surface (C API, Neon JS, Python). Each function takes a
//! [`HandleState`] reference plus typed arguments, performs the operation
//! against the underlying cognee-lib APIs, and returns a
//! `serde_json::Value` result (or an [`SdkError`]).
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.

use std::sync::Arc;

use uuid::Uuid;

use cognee_lib::api::get_or_create_default_user;
use cognee_lib::api::notebooks::{
    create_notebook, delete_notebook, list_notebooks, update_notebook,
};
use cognee_lib::api::{reset_dataset_pipeline_run_status, reset_pipeline_run_status};
use cognee_lib::database::{NotebookDb, NotebookUpdatePatch};

use crate::{HandleState, SdkError};

// ---------------------------------------------------------------------------
// Pipeline-run reset ops.
// ---------------------------------------------------------------------------

/// Re-arm a pipeline run (insert INITIATED row) for a specific pipeline
/// within a dataset.
///
/// `dataset_id_str` is a UUID string. Returns `serde_json::Value::Null` (void).
pub async fn run_reset_pipeline_run_status(
    state: &HandleState,
    dataset_id_str: &str,
    pipeline_name: &str,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    reset_pipeline_run_status(
        Arc::clone(&svc.pipeline_run_repo),
        owner_id,
        dataset_id,
        pipeline_name,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("reset_pipeline_run_status failed: {e}")))?;

    Ok(serde_json::Value::Null)
}

/// Reset all pipeline run statuses for a dataset.
///
/// `dataset_id_str` is a UUID string. Returns `serde_json::Value::Null` (void).
pub async fn run_reset_dataset_pipeline_run_status(
    state: &HandleState,
    dataset_id_str: &str,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    reset_dataset_pipeline_run_status(Arc::clone(&svc.pipeline_run_repo), owner_id, dataset_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("reset_dataset_pipeline_run_status failed: {e}")))?;

    Ok(serde_json::Value::Null)
}

// ---------------------------------------------------------------------------
// Default user op.
// ---------------------------------------------------------------------------

/// Get or create the default user for the configured email address.
///
/// Returns a `User` JSON object.
pub async fn run_get_or_create_default_user(
    state: &HandleState,
) -> Result<serde_json::Value, SdkError> {
    // Snapshot the email under the guard, drop the guard, then await.
    // `RwLockReadGuard` from `std::sync` is `!Send` and would poison the
    // `Send`-bounded futures the PyO3 / Neon bindings build on top of us.
    let default_user_email = {
        let settings = state.cm.settings();
        settings.default_user_email.clone()
    };
    let user = get_or_create_default_user(&default_user_email)
        .await
        .map_err(|e| SdkError::UserBootstrap(format!("get_or_create_default_user failed: {e}")))?;

    serde_json::to_value(&user)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize User: {e}")))
}

// ---------------------------------------------------------------------------
// Notebook ops.
// ---------------------------------------------------------------------------

/// List all notebooks for the current owner.
///
/// Returns a JSON array of `Notebook` objects.
pub async fn run_list_notebooks(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    let notebooks = list_notebooks(&nb_db, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_notebooks failed: {e}")))?;

    serde_json::to_value(&notebooks)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize notebooks: {e}")))
}

/// Create a new notebook.
///
/// `cells` must be a JSON array (use `serde_json::Value::Array(vec![])` for empty).
/// `deletable` controls whether the notebook can be deleted (always `true` to match
/// Python lib behavior — callers should pass `true`).
/// Returns a `Notebook` JSON object.
pub async fn run_create_notebook(
    state: &HandleState,
    name: String,
    cells: serde_json::Value,
    deletable: bool,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    let notebook = create_notebook(&nb_db, owner_id, name, cells, deletable)
        .await
        .map_err(|e| SdkError::Runtime(format!("create_notebook failed: {e}")))?;

    serde_json::to_value(&notebook)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize Notebook: {e}")))
}

/// Update a notebook's name and/or cells.
///
/// `patch_json` is an object with optional `"name"` (string) and/or `"cells"` (array) fields.
/// Returns a `Notebook` JSON object, or `serde_json::Value::Null` if not found.
pub async fn run_update_notebook(
    state: &HandleState,
    id_str: &str,
    patch_json: serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let id = Uuid::parse_str(id_str)
        .map_err(|e| SdkError::Validation(format!("invalid notebook id UUID: {e}")))?;

    let patch = NotebookUpdatePatch {
        name: patch_json
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        // Explicit null must behave like a missing key (leave cells unchanged),
        // matching how a null `name` is treated by the `as_str` filter above —
        // otherwise `{"cells": None}` from Python would silently clear cells.
        cells: patch_json.get("cells").filter(|v| !v.is_null()).cloned(),
    };

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    let result = update_notebook(&nb_db, id, owner_id, patch)
        .await
        .map_err(|e| SdkError::Runtime(format!("update_notebook failed: {e}")))?;

    match result {
        Some(nb) => serde_json::to_value(&nb)
            .map_err(|e| SdkError::Runtime(format!("failed to serialize Notebook: {e}"))),
        None => Ok(serde_json::Value::Null),
    }
}

/// Delete a notebook by UUID.
///
/// Returns `serde_json::Value::Bool(true)` if deleted, `Bool(false)` if not found.
pub async fn run_delete_notebook(
    state: &HandleState,
    id_str: &str,
) -> Result<serde_json::Value, SdkError> {
    let id = Uuid::parse_str(id_str)
        .map_err(|e| SdkError::Validation(format!("invalid notebook id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    let removed = delete_notebook(&nb_db, id, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("delete_notebook failed: {e}")))?;

    Ok(serde_json::Value::Bool(removed))
}
