//! Phase 6 — dataset management ops:
//! `cg_sdk_list_datasets`, `cg_sdk_list_data`, `cg_sdk_has_data`,
//! `cg_sdk_dataset_status`, `cg_sdk_empty_dataset`,
//! `cg_sdk_delete_data`, `cg_sdk_delete_all_datasets`.
//!
//! All follow the Phase-4 canonical pattern:
//!   `Arc::clone(&(*sdk).state)` → `spawn_sdk_op` → `state.services().await?`
//!   → call `DatasetManager` API → serialize result → callback.
//!
//! ## Serde notes
//! - `Dataset`, `Data`, `DeleteResult` ARE `Serialize` → direct serde.
//! - `PipelineRunStatus` IS `Serialize` but `HashMap<Uuid, PipelineRunStatus>`
//!   has non-string JSON keys → convert to `HashMap<String, _>` before
//!   serialising (same as neon).
//! - `has_data` returns `serde_json::Value::Bool(b)` → serialises to `"true"`
//!   or `"false"` (D9).

use std::ffi::c_char;
use std::sync::Arc;

use uuid::Uuid;

use cognee_bindings_common::{HandleState, SdkError};
use cognee_lib::api::{DatasetDb, DatasetManager};
use cognee_lib::delete::DeleteMode;

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

// ---------------------------------------------------------------------------
// Core async logic.
// ---------------------------------------------------------------------------

async fn run_list_datasets(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let datasets = mgr
        .list_datasets(owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_datasets failed: {e}")))?;

    serde_json::to_value(&datasets)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize datasets: {e}")))
}

async fn run_list_data(
    state: &HandleState,
    dataset_id_str: &str,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let items = mgr
        .list_data(dataset_id, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_data failed: {e}")))?;

    serde_json::to_value(&items)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize data items: {e}")))
}

async fn run_has_data(
    state: &HandleState,
    dataset_id_str: &str,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let has = mgr
        .has_data(dataset_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("has_data failed: {e}")))?;

    // D9: strict JSON bool.
    Ok(serde_json::Value::Bool(has))
}

async fn run_dataset_status(
    state: &HandleState,
    ids_json: serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let ids: Vec<Uuid> = ids_json
        .as_array()
        .ok_or_else(|| SdkError::Validation("datasetIds must be a JSON array".to_string()))?
        .iter()
        .map(|v| {
            v.as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| {
                    SdkError::Validation("each datasetId must be a valid UUID string".to_string())
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let svc = state.services().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let statuses = mgr
        .get_status(&ids)
        .await
        .map_err(|e| SdkError::Runtime(format!("get_status failed: {e}")))?;

    // Convert HashMap<Uuid, PipelineRunStatus> → HashMap<String, _> so JSON
    // keys are valid strings (same as neon).
    let string_keyed: std::collections::HashMap<String, _> = statuses
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

    serde_json::to_value(&string_keyed)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize status map: {e}")))
}

async fn run_empty_dataset(
    state: &HandleState,
    dataset_id_str: &str,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let result = mgr
        .empty_dataset(dataset_id, owner_id, svc.delete_service.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("empty_dataset failed: {e}")))?;

    serde_json::to_value(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))
}

async fn run_delete_data(
    state: &HandleState,
    dataset_id_str: &str,
    data_id_str: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let data_id = Uuid::parse_str(data_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid data id UUID: {e}")))?;
    let soft_delete = opts
        .get("softDelete")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let delete_dataset_if_empty = opts
        .get("deleteDatasetIfEmpty")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mode = if soft_delete {
        DeleteMode::Soft
    } else {
        DeleteMode::Hard
    };

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let result = mgr
        .delete_data(
            dataset_id,
            data_id,
            owner_id,
            mode,
            delete_dataset_if_empty,
            svc.delete_service.as_ref(),
        )
        .await
        .map_err(|e| SdkError::Runtime(format!("delete_data failed: {e}")))?;

    serde_json::to_value(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))
}

async fn run_delete_all_datasets(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mgr = DatasetManager::new(Arc::clone(&svc.database) as Arc<dyn DatasetDb>);
    let results = mgr
        .delete_all(owner_id, svc.delete_service.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("delete_all failed: {e}")))?;

    serde_json::to_value(&results)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult[]: {e}")))
}

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
    spawn_sdk_op(callback, ud, async move { run_list_datasets(&state).await });
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
    spawn_sdk_op(
        callback,
        ud,
        async move { run_list_data(&state, &id_str).await },
    );
}

/// Check whether a dataset has any data.
///
/// `dataset_id` is a UUID string (C string).
///
/// On success `result_json` is `"true"` or `"false"` (D9 strict JSON bool).
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
    spawn_sdk_op(
        callback,
        ud,
        async move { run_has_data(&state, &id_str).await },
    );
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
        run_dataset_status(&state, ids_val).await
    });
}

/// Remove all data items from a dataset (keep the dataset itself).
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
        run_empty_dataset(&state, &id_str).await
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
        run_delete_data(&state, &ds_id_str, &d_id_str, &opts_val).await
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
        run_delete_all_datasets(&state).await
    });
}
