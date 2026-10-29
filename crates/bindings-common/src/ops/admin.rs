//! Shared async admin operations:
//! `reset_pipeline_run_status`, `reset_dataset_pipeline_run_status`,
//! `get_or_create_default_user`.
//!
//! These functions contain the pure-Rust async logic shared between every
//! language binding surface (C API, Neon JS, Python). Each function takes a
//! [`HandleState`] reference plus typed arguments, performs the operation
//! against the underlying cognee APIs, and returns a
//! `serde_json::Value` result (or an [`SdkError`]).
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.

use std::sync::Arc;

use uuid::Uuid;

use cognee::api::get_or_create_default_user;
use cognee::api::{reset_dataset_pipeline_run_status, reset_pipeline_run_status};

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
    // When a DB-backed bootstrap hook is attached (downstream build), resolve
    // (and persist) the user through it; otherwise use the DB-free OSS path.
    let user = if let Some(hook) = state.default_user_bootstrap() {
        let svc = state.services().await?;
        hook.bootstrap(&svc.database, &default_user_email)
            .await
            .map_err(|e| {
                SdkError::UserBootstrap(format!("get_or_create_default_user failed: {e}"))
            })?
    } else {
        get_or_create_default_user(&default_user_email)
            .await
            .map_err(|e| {
                SdkError::UserBootstrap(format!("get_or_create_default_user failed: {e}"))
            })?
    };

    serde_json::to_value(&user)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize User: {e}")))
}
