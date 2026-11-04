//! Phase 6 — admin / session / user ops:
//! `cg_sdk_get_session`, `cg_sdk_add_feedback`, `cg_sdk_delete_feedback`,
//! `cg_sdk_get_or_create_default_user`.
//!
//! All follow the Phase-4 canonical pattern:
//!   `Arc::clone(&(*sdk).state)` → `spawn_sdk_op` → `state.services().await?`
//!   → call cognee API → serialize result → callback.
//!
//! ## Serde notes
//! - `SessionQAEntry`, `User` derive `Serialize` → direct serde.
//! - `bool` results → `serde_json::Value::Bool(b)` → `"true"`/`"false"` (D9).
//! - Void results → `serde_json::Value::Null` → `"null"` (D9).

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
