//! Phase 5 — data ops: forget / update / prune.
//!
//! Each export follows the canonical pattern:
//!   `Arc::clone(&handle.state)` → `runtime().spawn` → `state.services().await?`
//!   → call cognee-lib API → `settle_with`.
//!
//! ## Serde notes
//! - `ForgetResult`, `UpdateResult`, `PruneResult` do NOT derive `Serialize`.
//!   `ForgetResult.delete_result`, `UpdateResult.delete_result`,
//!   `UpdateResult.new_data` all DO derive `Serialize` → used via serde directly.
//!   `UpdateResult.cognify_result` hand-built via `cognify_result_json`.

use std::sync::Arc;

use neon::prelude::*;
use serde_json::json;
use uuid::Uuid;

use cognee_lib::api::{
    DatasetRef, ForgetTarget, PruneTarget, forget, prune_data, prune_system, update,
};
use cognee_lib::database::IngestDb;

use crate::errors::{SdkError, throw_sdk_error};
use crate::json::{cognify_result_json, js_to_value, marshal_inputs, parse_js, read_opts};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;

// ---------------------------------------------------------------------------
// opts helpers
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
// cogneeForget
// ---------------------------------------------------------------------------

/// `cogneeForget(handle, target, opts?) -> Promise<ForgetResult>`
///
/// `target` is a discriminated union:
///   `{ kind: "item", dataId: string, dataset: { name: string } | { id: string } }`
///   `{ kind: "dataset", dataset: { name: string } | { id: string } }`
///   `{ kind: "all" }`
///
/// Hand-built JSON: `{ target: string, deleteResult: {...} }`.
pub fn cognee_forget(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let target_arg = cx.argument::<JsValue>(1)?;
    let target_json = js_to_value(&mut cx, target_arg)?;
    let opts = read_opts(&mut cx, 2)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_forget(&state, target_json, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_forget(
    state: &crate::sdk::HandleState,
    target_json: serde_json::Value,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let _ = opts; // forget() does not take a tenant_id; opts reserved for future use
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let target = marshal_forget_target(&target_json)?;

    let db_ref: &dyn IngestDb = svc.database.as_ref();

    let result = forget(target, owner_id, svc.delete_service.as_ref(), Some(db_ref))
        .await
        .map_err(|e| SdkError::Runtime(format!("forget failed: {e}")))?;

    let delete_result_json = serde_json::to_value(&result.delete_result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize DeleteResult: {e}")))?;

    let forget_json = json!({
        "target": result.target,
        "deleteResult": delete_result_json,
    });

    serde_json::to_string(&forget_json)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize ForgetResult: {e}")))
}

/// Marshal the JS `target` discriminated union into a `ForgetTarget`.
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

/// Marshal `{ name: string }` or `{ id: string }` into a `DatasetRef`.
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
// cogneeUpdate
// ---------------------------------------------------------------------------

/// `cogneeUpdate(handle, dataId, newData, datasetName, opts?) -> Promise<UpdateResult>`
///
/// Hand-built JSON: `{ deletedDataId, deleteResult, newData, cognifyResult }`.
pub fn cognee_update(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let data_id_str = cx.argument::<JsString>(1)?.value(&mut cx);
    let new_data_arg = cx.argument::<JsValue>(2)?;
    let new_data_json = js_to_value(&mut cx, new_data_arg)?;
    let dataset_name = cx.argument::<JsString>(3)?.value(&mut cx);
    let opts = read_opts(&mut cx, 4)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_update(&state, &data_id_str, new_data_json, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_update(
    state: &crate::sdk::HandleState,
    data_id_str: &str,
    new_data_json: serde_json::Value,
    dataset_name: &str,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
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
    let new_data_json = serde_json::to_value(&result.new_data)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize new_data: {e}")))?;
    let cognify_result_json_val = result
        .cognify_result
        .as_ref()
        .map(cognify_result_json)
        .unwrap_or(serde_json::Value::Null);

    let update_json = json!({
        "deletedDataId": result.deleted_data_id.to_string(),
        "deleteResult": delete_result_json,
        "newData": new_data_json,
        "cognifyResult": cognify_result_json_val,
    });

    serde_json::to_string(&update_json)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize UpdateResult: {e}")))
}

// ---------------------------------------------------------------------------
// cogneePruneData / cogneePruneSystem
// ---------------------------------------------------------------------------

/// `cogneePruneData(handle) -> Promise<void>`
///
/// Removes all files from data storage (`prune_data`).
pub fn cognee_prune_data(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_prune_data(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(()) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_prune_data(state: &crate::sdk::HandleState) -> Result<(), SdkError> {
    let svc = state.services().await?;
    prune_data(svc.storage.as_ref())
        .await
        .map_err(|e| SdkError::Runtime(format!("prune_data failed: {e}")))
}

/// `cogneePruneSystem(handle, opts?) -> Promise<PruneResult>`
///
/// opts: `{ pruneGraph?, pruneVector?, pruneMetadata?, pruneCache? }`
/// Defaults: graph=true, vector=true, metadata=false, cache=true (Python defaults).
///
/// Hand-built JSON: `{ dataPruned, graphPruned, vectorPruned, metadataPruned, cachePruned }`.
pub fn cognee_prune_system(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let opts = read_opts(&mut cx, 1)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_prune_system(&state, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_prune_system(
    state: &crate::sdk::HandleState,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let svc = state.services().await?;

    // Build PruneTarget from opts, defaulting to Python's default_system().
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

    let prune_json = json!({
        "dataPruned": result.data_pruned,
        "graphPruned": result.graph_pruned,
        "vectorPruned": result.vector_pruned,
        "metadataPruned": result.metadata_pruned,
        "cachePruned": result.cache_pruned,
    });

    serde_json::to_string(&prune_json)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize PruneResult: {e}")))
}
