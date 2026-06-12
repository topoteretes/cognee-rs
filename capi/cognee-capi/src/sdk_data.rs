//! Phase 6 — data ops: `cg_sdk_forget`, `cg_sdk_update`,
//! `cg_sdk_prune_data`, `cg_sdk_prune_system`.
//!
//! All four follow the Phase-4 canonical pattern:
//!   `Arc::clone(&(*sdk).state)` → `spawn_sdk_op` → shared op in
//!   `cognee_bindings_common::ops::data` → serialize result → callback.
//!
//! The shared async logic lives in `cognee_bindings_common::ops::data`;
//! this module only handles C string parsing / null-pointer checks and
//! the callback dispatch pattern.

use std::ffi::c_char;
use std::sync::Arc;

use cognee_bindings_common::ops::data;
use cognee_bindings_common::SdkError;

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

// ---------------------------------------------------------------------------
// C-exported functions.
// ---------------------------------------------------------------------------

/// Delete data from the knowledge graph.
///
/// `target_json` is a discriminated union on `"kind"`:
///   `{"kind":"item","dataId":"<uuid>","dataset":{"name":"…"}|{"id":"<uuid>"}}`
///   `{"kind":"dataset","dataset":{"name":"…"}|{"id":"<uuid>"}}`
///   `{"kind":"all"}`
/// `opts_json` may be `NULL` or a JSON object (reserved for future `"tenant"` support).
///
/// On success `result_json` is: `{"target":"…","deleteResult":{…}}`
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `target_json` must be valid non-null null-terminated UTF-8 strings.
/// `opts_json` may be NULL.  `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_forget(
    sdk: *const CgSdk,
    target_json: *const c_char,
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

    let target_str = match parse_c_str_or_fire(target_json, "target_json", callback, ud_raw) {
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
        let target_val: serde_json::Value = serde_json::from_str(&target_str)
            .map_err(|e| SdkError::Validation(format!("target_json parse error: {e}")))?;
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        data::forget(&state, target_val, &opts_val).await
    });
}

/// Replace a data item with new content and re-cognify.
///
/// `data_id` is a UUID string (C string, not a JSON string — no quotes).
/// `new_data_json` is a `CogneeDataInput` object or array.
/// `dataset_name` is the dataset name.
/// `opts_json` may be `NULL` or a JSON object with optional `"tenant"` (UUID string).
///
/// On success `result_json` is:
///   `{"deletedDataId":"<uuid>","deleteResult":{…},"newData":[…],"cognifyResult":{…}}`
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `data_id`, `new_data_json`, `dataset_name` must be valid non-null
/// null-terminated UTF-8 strings.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_update(
    sdk: *const CgSdk,
    data_id: *const c_char,
    new_data_json: *const c_char,
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

    let data_id_str = match parse_c_str_or_fire(data_id, "data_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let new_data_str = match parse_c_str_or_fire(new_data_json, "new_data_json", callback, ud_raw) {
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
        let new_data_val: serde_json::Value = serde_json::from_str(&new_data_str)
            .map_err(|e| SdkError::Validation(format!("new_data_json parse error: {e}")))?;
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        data::update(&state, &data_id_str, new_data_val, &dataset_str, &opts_val).await
    });
}

/// Remove all files from data storage.
///
/// Returns `"null"` (D9) on success.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `user_data` is forwarded to
/// `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_prune_data(
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
    spawn_sdk_op(callback, ud, async move { data::prune_data(&state).await });
}

/// Selective backend cleanup (graph, vector, session cache).
///
/// `opts_json` may be `NULL` or a JSON object with optional boolean fields:
///   `"pruneGraph"` (default true), `"pruneVector"` (default true),
///   `"pruneMetadata"` (default false), `"pruneCache"` (default true).
///
/// On success `result_json` is:
///   `{"dataPruned":bool,"graphPruned":bool,"vectorPruned":bool,
///     "metadataPruned":bool,"cachePruned":bool}`
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_prune_system(
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
        data::prune_system(&state, &opts_val).await
    });
}
