//! Phase 6 — admin / session / pipeline-run / user / notebook ops:
//! `cg_sdk_get_session`, `cg_sdk_add_feedback`, `cg_sdk_delete_feedback`,
//! `cg_sdk_get_graph_context`, `cg_sdk_set_graph_context`,
//! `cg_sdk_reset_pipeline_run_status`, `cg_sdk_reset_dataset_pipeline_run_status`,
//! `cg_sdk_get_or_create_default_user`,
//! `cg_sdk_list_notebooks`, `cg_sdk_create_notebook`,
//! `cg_sdk_update_notebook`, `cg_sdk_delete_notebook`.
//!
//! All follow the Phase-4 canonical pattern:
//!   `Arc::clone(&(*sdk).state)` → `spawn_sdk_op` → `state.services().await?`
//!   → call cognee-lib API → serialize result → callback.
//!
//! ## Serde notes
//! - `SessionQAEntry`, `User`, `Notebook` derive `Serialize` → direct serde.
//! - `bool` results → `serde_json::Value::Bool(b)` → `"true"`/`"false"` (D9).
//! - Void results → `serde_json::Value::Null` → `"null"` (D9).
//! - `get_graph_context`: `Option<String>` → `"\"<ctx>\""` (D9 quoted string)
//!   or `"null"` (D9 null).

use std::ffi::c_char;
use std::sync::Arc;

use uuid::Uuid;

use cognee_bindings_common::{HandleState, SdkError};
use cognee_lib::api::get_or_create_default_user;
use cognee_lib::api::notebooks::{
    create_notebook, delete_notebook, list_notebooks, update_notebook,
};
use cognee_lib::api::{reset_dataset_pipeline_run_status, reset_pipeline_run_status};
use cognee_lib::database::{NotebookDb, NotebookUpdatePatch, UserDb};
use cognee_lib::session::get_session;

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

// ---------------------------------------------------------------------------
// Core async logic.
// ---------------------------------------------------------------------------

async fn run_get_session(
    state: &HandleState,
    session_id: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let last_n = opts
        .get("lastN")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let entries = get_session(
        svc.session_store.as_ref(),
        session_id,
        Some(&owner_str),
        last_n,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("get_session failed: {e}")))?;

    serde_json::to_value(&entries)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize SessionQAEntry[]: {e}")))
}

async fn run_add_feedback(
    state: &HandleState,
    session_id: &str,
    qa_id: &str,
    feedback_text: Option<String>,
    feedback_score: Option<i32>,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let ok = cognee_lib::session::add_feedback(
        svc.session_manager.as_ref(),
        session_id,
        qa_id,
        Some(&owner_str),
        feedback_text.as_deref(),
        feedback_score,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("add_feedback failed: {e}")))?;

    Ok(serde_json::Value::Bool(ok))
}

async fn run_delete_feedback(
    state: &HandleState,
    session_id: &str,
    qa_id: &str,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let ok = cognee_lib::session::delete_feedback(
        svc.session_manager.as_ref(),
        session_id,
        qa_id,
        Some(&owner_str),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("delete_feedback failed: {e}")))?;

    Ok(serde_json::Value::Bool(ok))
}

async fn run_get_graph_context(
    state: &HandleState,
    session_id: &str,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let ctx = cognee_lib::session::get_graph_context(
        svc.session_manager.as_ref(),
        session_id,
        Some(&owner_str),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("get_graph_context failed: {e}")))?;

    // D9: quoted JSON string or null.
    match ctx {
        Some(s) => Ok(serde_json::Value::String(s)),
        None => Ok(serde_json::Value::Null),
    }
}

async fn run_set_graph_context(
    state: &HandleState,
    session_id: &str,
    context: &str,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    cognee_lib::session::set_graph_context(
        svc.session_manager.as_ref(),
        session_id,
        Some(&owner_str),
        context,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("set_graph_context failed: {e}")))?;

    // D9: void ops return null.
    Ok(serde_json::Value::Null)
}

async fn run_reset_pipeline_run_status(
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

async fn run_reset_dataset_pipeline_run_status(
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

async fn run_get_or_create_default_user(
    state: &HandleState,
) -> Result<serde_json::Value, SdkError> {
    let email = state.cm.settings().default_user_email.clone();
    let svc = state.services().await?;

    let user =
        get_or_create_default_user(Arc::clone(&svc.database).as_ref() as &dyn UserDb, &email)
            .await
            .map_err(|e| {
                SdkError::UserBootstrap(format!("get_or_create_default_user failed: {e}"))
            })?;

    serde_json::to_value(&user)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize User: {e}")))
}

async fn run_list_notebooks(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    let notebooks = list_notebooks(&nb_db, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_notebooks failed: {e}")))?;

    serde_json::to_value(&notebooks)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize notebooks: {e}")))
}

async fn run_create_notebook(
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

async fn run_update_notebook(
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
        cells: patch_json.get("cells").cloned(),
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

async fn run_delete_notebook(
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

// ---------------------------------------------------------------------------
// C-exported functions.
// ---------------------------------------------------------------------------

/// Get session QA history entries.
///
/// `session_id` is the session identifier (null-terminated UTF-8).
/// `opts_json` may be `NULL` or a JSON object with optional `"lastN"` (integer).
///
/// On success `result_json` is a JSON array of `SessionQAEntry` objects.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `session_id` must be valid non-null null-terminated UTF-8 strings.
/// `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_get_session(
    sdk: *const CgSdk,
    session_id: *const c_char,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let session_str = match parse_c_str_or_fire(session_id, "session_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        run_get_session(&state, &session_str, &opts_val).await
    });
}

/// Add feedback to a QA interaction.
///
/// `session_id` is the session identifier.  `qa_id` is the QA entry identifier.
/// `opts_json` may be `NULL` or a JSON object with optional fields:
///   `"feedbackText"` (string), `"feedbackScore"` (integer).
///
/// On success `result_json` is `"true"` or `"false"` (D9 strict JSON bool).
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `session_id`, `qa_id` must be valid non-null null-terminated UTF-8
/// strings.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_add_feedback(
    sdk: *const CgSdk,
    session_id: *const c_char,
    qa_id: *const c_char,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let session_str = match parse_c_str_or_fire(session_id, "session_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let qa_str = match parse_c_str_or_fire(qa_id, "qa_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        // feedbackText and feedbackScore folded into opts_json (C-ABI uniformity).
        let feedback_text = opts_val
            .get("feedbackText")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let feedback_score = opts_val
            .get("feedbackScore")
            .and_then(|v| v.as_i64())
            .map(|n| n as i32);
        run_add_feedback(&state, &session_str, &qa_str, feedback_text, feedback_score).await
    });
}

/// Delete feedback from a QA interaction.
///
/// `session_id` is the session identifier.  `qa_id` is the QA entry identifier.
/// (No `opts_json` — mirrors neon which also has no opts for this op.)
///
/// On success `result_json` is `"true"` or `"false"` (D9 strict JSON bool).
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `session_id`, `qa_id` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_delete_feedback(
    sdk: *const CgSdk,
    session_id: *const c_char,
    qa_id: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let session_str = match parse_c_str_or_fire(session_id, "session_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let qa_str = match parse_c_str_or_fire(qa_id, "qa_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        run_delete_feedback(&state, &session_str, &qa_str).await
    });
}

/// Get the graph context stored in a session.
///
/// `session_id` is the session identifier.
/// (No `opts_json` — mirrors neon which also has no opts for this op.)
///
/// On success `result_json` is a quoted JSON string `"\"<ctx>\""` (D9) when a
/// context is present, or `"null"` (D9) when absent.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `session_id` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_get_graph_context(
    sdk: *const CgSdk,
    session_id: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let session_str = match parse_c_str_or_fire(session_id, "session_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        run_get_graph_context(&state, &session_str).await
    });
}

/// Set the graph context stored in a session.
///
/// `session_id` is the session identifier.  `context` is the context string.
/// (No `opts_json` — mirrors neon.)
///
/// Returns `"null"` (D9) on success.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `session_id`, `context` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_set_graph_context(
    sdk: *const CgSdk,
    session_id: *const c_char,
    context: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let session_str = match parse_c_str_or_fire(session_id, "session_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let context_str = match parse_c_str_or_fire(context, "context", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        run_set_graph_context(&state, &session_str, &context_str).await
    });
}

/// Reset the pipeline run status for a specific pipeline within a dataset.
///
/// `dataset_id` is a UUID string.  `pipeline_name` is the pipeline name.
///
/// Returns `"null"` (D9) on success.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `dataset_id`, `pipeline_name` must be valid non-null null-terminated
/// UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_reset_pipeline_run_status(
    sdk: *const CgSdk,
    dataset_id: *const c_char,
    pipeline_name: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let ds_str = match parse_c_str_or_fire(dataset_id, "dataset_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let pipe_str = match parse_c_str_or_fire(pipeline_name, "pipeline_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        run_reset_pipeline_run_status(&state, &ds_str, &pipe_str).await
    });
}

/// Reset all pipeline run statuses for a dataset.
///
/// `dataset_id` is a UUID string.
///
/// Returns `"null"` (D9) on success.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `dataset_id` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_reset_dataset_pipeline_run_status(
    sdk: *const CgSdk,
    dataset_id: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let ds_str = match parse_c_str_or_fire(dataset_id, "dataset_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        run_reset_dataset_pipeline_run_status(&state, &ds_str).await
    });
}

/// Get or create the default user account.
///
/// On success `result_json` is a `User` JSON object.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `user_data` is forwarded to
/// `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_get_or_create_default_user(
    sdk: *const CgSdk,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        run_get_or_create_default_user(&state).await
    });
}

/// List all notebooks for the current owner.
///
/// On success `result_json` is a JSON array of `Notebook` objects.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_list_notebooks(
    sdk: *const CgSdk,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud = SendUserData(user_data);
    spawn_sdk_op(
        callback,
        ud,
        async move { run_list_notebooks(&state).await },
    );
}

/// Create a new notebook.
///
/// `name` is the notebook name (null-terminated UTF-8).
/// `cells_json` may be `NULL` (interpreted as `[]`) or a JSON array of cells.
/// `deletable` is non-zero for true, zero for false.
///
/// On success `result_json` is a `Notebook` JSON object.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `name` must be valid non-null null-terminated UTF-8 strings.
/// `cells_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_create_notebook(
    sdk: *const CgSdk,
    name: *const c_char,
    cells_json: *const c_char,
    deletable: std::ffi::c_int,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let name_str = match parse_c_str_or_fire(name, "name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    // cells_json NULL → empty array.
    let cells_str: Option<String> = if cells_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(cells_json, "cells_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let is_deletable = deletable != 0;

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let cells_val: serde_json::Value = match cells_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("cells_json parse error: {e}")))?,
            None => serde_json::Value::Array(vec![]),
        };
        run_create_notebook(&state, name_str, cells_val, is_deletable).await
    });
}

/// Update a notebook's name and/or cells.
///
/// `id` is a UUID string (C string).
/// `patch_json` is a JSON object with optional `"name"` (string) and/or
/// `"cells"` (JSON array) fields.
///
/// On success `result_json` is a `Notebook` JSON object, or `"null"` (D9) if
/// the notebook was not found.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `id`, `patch_json` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_update_notebook(
    sdk: *const CgSdk,
    id: *const c_char,
    patch_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let id_str = match parse_c_str_or_fire(id, "id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let patch_str = match parse_c_str_or_fire(patch_json, "patch_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let patch_val: serde_json::Value = serde_json::from_str(&patch_str)
            .map_err(|e| SdkError::Validation(format!("patch_json parse error: {e}")))?;
        run_update_notebook(&state, &id_str, patch_val).await
    });
}

/// Delete a notebook by UUID.
///
/// `id` is a UUID string (C string).
///
/// On success `result_json` is `"true"` if the notebook was deleted, `"false"`
/// if it was not found (D9 strict JSON bool).
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `id` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_delete_notebook(
    sdk: *const CgSdk,
    id: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let id_str = match parse_c_str_or_fire(id, "id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        run_delete_notebook(&state, &id_str).await
    });
}
