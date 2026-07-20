//! Phase 6 — dataset management ops:
//! `cg_sdk_list_datasets`, `cg_sdk_list_data`, `cg_sdk_has_data`,
//! `cg_sdk_dataset_status`, `cg_sdk_empty_dataset`,
//! `cg_sdk_delete_data`, `cg_sdk_delete_all_datasets`.
//!
//! All follow the Phase-4 canonical pattern:
//!   `Arc::clone(&(*sdk).state)` → `spawn_sdk_op` → shared op in
//!   `cognee_bindings_common::ops::datasets` → serialize result → callback.
//!
//! The async business logic has been hoisted into
//! `cognee_bindings_common::ops::datasets`; this file contains only the
//! C-exported shim layer (null-checks, C-string parsing, `spawn_sdk_op`).

use std::ffi::c_char;
use std::sync::Arc;

use cognee_bindings_common::SdkError;
use cognee_bindings_common::ops::datasets;

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

// ---------------------------------------------------------------------------
// C-exported functions.
// ---------------------------------------------------------------------------

/// List all datasets for the current owner.
///
/// On success `result_json` is a JSON array of `Dataset` objects.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `user_data` is forwarded to
/// `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_list_datasets(
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
        datasets::list_datasets(&state).await
    });
}

/// List all data items in a dataset.
///
/// `dataset_id` is a UUID string (C string).
///
/// On success `result_json` is a JSON array of `Data` objects.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `dataset_id` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_list_data(
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

    let id_str = match parse_c_str_or_fire(dataset_id, "dataset_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        datasets::list_data(&state, &id_str).await
    });
}

/// Check whether a dataset has any data.
///
/// `dataset_id` is a UUID string (C string).
///
/// On success `result_json` is `"true"` or `"false"` (strict JSON bool).
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `dataset_id` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_has_data(
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

    let id_str = match parse_c_str_or_fire(dataset_id, "dataset_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        datasets::has_data(&state, &id_str).await
    });
}

/// Get the pipeline run status for a list of dataset UUIDs.
///
/// `dataset_ids_json` is a JSON array of UUID strings.
///
/// On success `result_json` is a JSON object: `{"<uuid>": "<status-string>", …}`
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `dataset_ids_json` must be valid non-null null-terminated UTF-8
/// strings.  `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_dataset_status(
    sdk: *const CgSdk,
    dataset_ids_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let ids_str = match parse_c_str_or_fire(dataset_ids_json, "dataset_ids_json", callback, ud_raw)
    {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let ids_val: serde_json::Value = serde_json::from_str(&ids_str)
            .map_err(|e| SdkError::Validation(format!("dataset_ids_json parse error: {e}")))?;
        datasets::dataset_status(&state, ids_val).await
    });
}

/// Remove all data items from a dataset and delete the dataset record.
///
/// `dataset_id` is a UUID string (C string).
///
/// On success `result_json` is a `DeleteResult` JSON object.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` and `dataset_id` must be valid non-null null-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_empty_dataset(
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

    let id_str = match parse_c_str_or_fire(dataset_id, "dataset_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        datasets::empty_dataset(&state, &id_str).await
    });
}

/// Delete a specific data item from a dataset.
///
/// `dataset_id` and `data_id` are UUID strings (C strings).
/// `opts_json` may be `NULL` or a JSON object with optional boolean fields:
///   `"softDelete"` (default false), `"deleteDatasetIfEmpty"` (default false).
///
/// On success `result_json` is a `DeleteResult` JSON object.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `dataset_id`, `data_id` must be valid non-null null-terminated UTF-8
/// strings.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_delete_data(
    sdk: *const CgSdk,
    dataset_id: *const c_char,
    data_id: *const c_char,
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

    let ds_id_str = match parse_c_str_or_fire(dataset_id, "dataset_id", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let d_id_str = match parse_c_str_or_fire(data_id, "data_id", callback, ud_raw) {
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
        datasets::delete_data(&state, &ds_id_str, &d_id_str, &opts_val).await
    });
}

/// Delete all datasets for the current owner.
///
/// On success `result_json` is a JSON array of `DeleteResult` objects.
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `user_data` is forwarded to
/// `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_delete_all_datasets(
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
        datasets::delete_all_datasets(&state).await
    });
}
