//! Phase 6 — data ops: `cg_sdk_forget`, `cg_sdk_update`,
//! `cg_sdk_prune_data`, `cg_sdk_prune_system`.
//!
//! All four follow the Phase-4 canonical pattern:
//!   `Arc::clone(&(*sdk).state)` → `spawn_sdk_op` → `state.services().await?`
//!   → call cognee-lib API → serialize result → callback.
//!
//! ## Serde notes
//! - `ForgetResult` and `UpdateResult` do NOT derive `Serialize` → hand-built JSON.
//!   `ForgetResult.delete_result` and `UpdateResult.delete_result` / `new_data`
//!   DO derive `Serialize` → used via serde directly.
//! - `PruneResult` does NOT derive `Serialize` → hand-built JSON.
//! - `prune_data` returns `Ok(serde_json::Value::Null)` per D9 ("null" string).

use std::ffi::c_char;
use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use cognee_bindings_common::wire::{cognify_result_json, marshal_inputs};
use cognee_bindings_common::{HandleState, SdkError};
use cognee_lib::api::{
    DatasetRef, ForgetTarget, PruneTarget, forget, prune_data, prune_system, update,
};
use cognee_lib::database::IngestDb;

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

// ---------------------------------------------------------------------------
// opts helpers (local copy, NOT in bindings-common — same decision as neon).
// ---------------------------------------------------------------------------

fn opts_tenant(opts: &serde_json::Value) -> Result<Option<Uuid>, SdkError> {
    match opts.get("tenant").and_then(|v| v.as_str()) {
        Some(s) => Uuid::parse_str(s)
            .map(Some)
            .map_err(|e| SdkError::Validation(format!("invalid `tenant` UUID: {e}"))),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// ForgetTarget marshalling.
// ---------------------------------------------------------------------------

fn marshal_forget_target(value: &serde_json::Value) -> Result<ForgetTarget, SdkError> {
    let obj = value
        .as_object()
        .ok_or_else(|| SdkError::Validation("forget target must be an object".to_string()))?;
    let kind = obj.get("kind").and_then(|v| v.as_str()).ok_or_else(|| {
        SdkError::Validation("forget target is missing a string `kind`".to_string())
    })?;

    match kind {
        "item" => {
            let data_id_str = obj.get("dataId").and_then(|v| v.as_str()).ok_or_else(|| {
                SdkError::Validation("item target requires a `dataId` UUID string".to_string())
            })?;
            let data_id = Uuid::parse_str(data_id_str)
                .map_err(|e| SdkError::Validation(format!("invalid `dataId` UUID: {e}")))?;
            let dataset = marshal_dataset_ref(obj.get("dataset"))?;
            Ok(ForgetTarget::Item { data_id, dataset })
        }
        "dataset" => {
            let dataset = marshal_dataset_ref(obj.get("dataset"))?;
            Ok(ForgetTarget::Dataset { dataset })
        }
        "all" => Ok(ForgetTarget::All),
        other => Err(SdkError::Validation(format!(
            "unknown forget target kind `{other}`. Valid: item, dataset, all"
        ))),
    }
}

fn marshal_dataset_ref(value: Option<&serde_json::Value>) -> Result<DatasetRef, SdkError> {
    let obj = value
        .and_then(|v| v.as_object())
        .ok_or_else(|| SdkError::Validation("dataset reference must be an object".to_string()))?;

    if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
        return Ok(DatasetRef::Name(name.to_string()));
    }
    if let Some(id_str) = obj.get("id").and_then(|v| v.as_str()) {
        let id = Uuid::parse_str(id_str)
            .map_err(|e| SdkError::Validation(format!("invalid dataset `id` UUID: {e}")))?;
        return Ok(DatasetRef::Id(id));
    }
    Err(SdkError::Validation(
        "dataset reference must have either `name` or `id`".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Core async logic.
// ---------------------------------------------------------------------------

async fn run_forget(
    state: &HandleState,
    target_json: serde_json::Value,
    _opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    // opts is ignored for now (reserved for future tenant support — same as neon).
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let target = marshal_forget_target(&target_json)?;

    let db_ref: &dyn IngestDb = svc.database.as_ref();

    let result = forget(target, owner_id, svc.delete_service.as_ref(), Some(db_ref))
        .await
        .map_err(|e| SdkError::Runtime(format!("forget failed: {e}")))?;

    let delete_result_json = serde_json::to_value(&result.delete_result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))?;

    Ok(json!({
        "target": result.target,
        "deleteResult": delete_result_json,
    }))
}

async fn run_update(
    state: &HandleState,
    data_id_str: &str,
    new_data_json: serde_json::Value,
    dataset_name: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let data_id = Uuid::parse_str(data_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid `dataId` UUID: {e}")))?;
    let tenant_id = opts_tenant(opts)?;
    let new_data = marshal_inputs(&new_data_json)?;

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let result = update(
        data_id,
        new_data,
        dataset_name,
        owner_id,
        tenant_id,
        svc.delete_service.as_ref(),
        svc.add_pipeline.as_ref(),
        svc.llm.clone(),
        svc.storage.clone(),
        svc.graph_db.clone(),
        svc.vector_db.clone(),
        svc.embedding_engine.clone(),
        Some(svc.database.clone()),
        svc.ontology_resolver.clone(),
        &svc.cognify_config,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("update failed: {e}")))?;

    let delete_result_json = serde_json::to_value(&result.delete_result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize delete_result: {e}")))?;
    let new_data_val = serde_json::to_value(&result.new_data)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize new_data: {e}")))?;
    let cognify_result_val = result
        .cognify_result
        .as_ref()
        .map(cognify_result_json)
        .unwrap_or(serde_json::Value::Null);

    Ok(json!({
        "deletedDataId": result.deleted_data_id.to_string(),
        "deleteResult": delete_result_json,
        "newData": new_data_val,
        "cognifyResult": cognify_result_val,
    }))
}

async fn run_prune_data(state: &HandleState) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    prune_data(svc.storage.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("prune_data failed: {e}")))?;
    // D9: void ops return "null".
    Ok(serde_json::Value::Null)
}

async fn run_prune_system(
    state: &HandleState,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;

    let defaults = PruneTarget::default_system();
    let target = PruneTarget {
        graph: opts
            .get("pruneGraph")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.graph),
        vector: opts
            .get("pruneVector")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.vector),
        metadata: opts
            .get("pruneMetadata")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.metadata),
        cache: opts
            .get("pruneCache")
            .and_then(|v| v.as_bool())
            .unwrap_or(defaults.cache),
    };

    let result = prune_system(
        &target,
        Some(svc.graph_db.as_ref()),
        Some(svc.vector_db.as_ref()),
        Some(svc.session_store.as_ref()),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("prune_system failed: {e}")))?;

    Ok(json!({
        "dataPruned": result.data_pruned,
        "graphPruned": result.graph_pruned,
        "vectorPruned": result.vector_pruned,
        "metadataPruned": result.metadata_pruned,
        "cachePruned": result.cache_pruned,
    }))
}

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
        run_forget(&state, target_val, &opts_val).await
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
        run_update(&state, &data_id_str, new_data_val, &dataset_str, &opts_val).await
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
    spawn_sdk_op(callback, ud, async move { run_prune_data(&state).await });
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
        run_prune_system(&state, &opts_val).await
    });
}
