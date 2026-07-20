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
//!   → call cognee API → serialize result → callback.
//!
//! ## Serde notes
//! - `SessionQAEntry`, `User`, `Notebook` derive `Serialize` → direct serde.
//! - `bool` results → `serde_json::Value::Bool(b)` → `"true"`/`"false"` (D9).
//! - Void results → `serde_json::Value::Null` → `"null"` (D9).
//! - `get_graph_context`: `Option<String>` → `"\"<ctx>\""` (D9 quoted string)
//!   or `"null"` (D9 null).

use std::ffi::c_char;
use std::sync::Arc;

use cognee_bindings_common::SdkError;
use cognee_bindings_common::ops::admin;
use cognee_bindings_common::ops::sessions;

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

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
        sessions::run_get_session(&state, &session_str, &opts_val).await
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
        sessions::run_add_feedback(&state, &session_str, &qa_str, &opts_val).await
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
        sessions::run_delete_feedback(&state, &session_str, &qa_str).await
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
        sessions::run_get_graph_context(&state, &session_str).await
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
        sessions::run_set_graph_context(&state, &session_str, &context_str).await
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
        admin::run_reset_pipeline_run_status(&state, &ds_str, &pipe_str).await
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
        admin::run_reset_dataset_pipeline_run_status(&state, &ds_str).await
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
        admin::run_get_or_create_default_user(&state).await
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
    spawn_sdk_op(callback, ud, async move {
        admin::run_list_notebooks(&state).await
    });
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
        admin::run_create_notebook(&state, name_str, cells_val, is_deletable).await
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
        admin::run_update_notebook(&state, &id_str, patch_val).await
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
        admin::run_delete_notebook(&state, &id_str).await
    });
}
