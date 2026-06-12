//! Phase 6 — memory ops: `cg_sdk_remember`, `cg_sdk_remember_entry`,
//! `cg_sdk_memify`, `cg_sdk_improve`.
//!
//! All four follow the Phase-4 canonical pattern:
//!   `Arc::clone(&(*sdk).state)` → `spawn_sdk_op` → delegate to
//!   `cognee_bindings_common::ops::memory` shared async bodies.
//!
//! The shared async bodies (run_remember / run_remember_entry / run_memify_op /
//! run_improve) now live in `cognee-bindings-common` so they can be reused by
//! the Python and JS bindings without duplication.

use std::ffi::c_char;
use std::sync::Arc;

use cognee_bindings_common::ops::memory;
use cognee_bindings_common::SdkError;

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

// ---------------------------------------------------------------------------
// C-exported functions.
// ---------------------------------------------------------------------------

/// Add data to memory: ingest inputs, cognify, persist session interaction.
///
/// `inputs_json` is a `CogneeDataInput` object **or** array.
/// `dataset_name` is the target dataset name.
/// `opts_json` may be `NULL` or a JSON object with optional fields:
///   `"sessionId"` (string), `"selfImprovement"` (boolean), `"tenant"` (UUID string).
///
/// Async (D4, R1): the callback fires on a tokio worker thread, never
/// synchronously from this call.
///
/// On success `result_json` is a `RememberResult` JSON object.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `inputs_json` and `dataset_name`
/// must be valid null-terminated UTF-8 strings.  `opts_json` may be NULL.
/// `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_remember(
    sdk: *const CgSdk,
    inputs_json: *const c_char,
    dataset_name: *const c_char,
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

    let inputs_str = match parse_c_str_or_fire(inputs_json, "inputs_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
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
        let inputs_val: serde_json::Value = serde_json::from_str(&inputs_str)
            .map_err(|e| SdkError::Validation(format!("inputs_json parse error: {e}")))?;
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        memory::run_remember(&state, inputs_val, &dataset_str, &opts_val).await
    });
}

/// Store a single memory entry (QA, trace, or feedback) into the session.
///
/// `entry_json` is a discriminated union on `"type"`:
///   `{"type":"qa", "question":"…", "answer":"…", …}`
///   `{"type":"trace", "originFunction":"…", "status":"…", …}`
///   `{"type":"feedback", "qaId":"…", …}`
/// `dataset_name` is the target dataset name (null-terminated UTF-8).
/// `session_id` is the session identifier (null-terminated UTF-8).
/// `opts_json` may be `NULL` or a JSON object with optional `"tenant"` (UUID string).
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `entry_json`, `dataset_name`, `session_id` must be valid non-null
/// null-terminated UTF-8 strings.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_remember_entry(
    sdk: *const CgSdk,
    entry_json: *const c_char,
    dataset_name: *const c_char,
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

    let entry_str = match parse_c_str_or_fire(entry_json, "entry_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
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
        let entry_val: serde_json::Value = serde_json::from_str(&entry_str)
            .map_err(|e| SdkError::Validation(format!("entry_json parse error: {e}")))?;
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        memory::run_remember_entry(&state, entry_val, &dataset_str, &session_str, &opts_val).await
    });
}

/// Run the memify pipeline: create triplet embeddings for all graph edges and
/// index them in the vector DB.  Idempotent (safe to re-run).
///
/// `opts_json` may be `NULL` or a JSON object with optional fields:
///   `"tripletBatchSize"` (integer), `"nodeTypeFilter"` (string),
///   `"nodeNameFilter"` (string array), `"nodeNameFilterOperator"` (string).
///
/// On success `result_json` is a JSON object:
///   `{"tripletCount":N,"indexedCount":N,"batchCount":N,"alreadyCompleted":bool,
///     "priorPipelineRunId":null|"<uuid>"}`
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_memify(
    sdk: *const CgSdk,
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
        memory::run_memify_op(&state, &opts_val).await
    });
}

/// Apply graph improvement based on session feedback.
///
/// `opts_json` is a JSON object with required field `"datasetName"` and optional:
///   `"sessionIds"` (string array), `"nodeName"` (string array),
///   `"feedbackAlpha"` (float, default 0.1), `"tenant"` (UUID string).
///
/// On success `result_json` is a JSON object:
///   `{"stagesRun":[…],"memifyResult":…,"feedbackEntriesProcessed":N,
///     "feedbackEntriesApplied":N,"sessionsPersisted":N,"edgesSynced":N}`
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `opts_json` must be a valid
/// non-null null-terminated UTF-8 JSON string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_improve(
    sdk: *const CgSdk,
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

    let opts_str = match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let opts_val: serde_json::Value = serde_json::from_str(&opts_str)
            .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?;
        memory::run_improve(&state, &opts_val).await
    });
}
