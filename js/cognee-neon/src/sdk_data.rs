//! Phase 5 — data ops: forget / update / prune.
//!
//! Each export follows the canonical pattern:
//!   `Arc::clone(&handle.state)` → `runtime().spawn` → shared op in
//!   `cognee_bindings_common::ops::data` → `settle_with`.
//!
//! The shared async logic lives in `cognee_bindings_common::ops::data`;
//! this module only handles JS argument extraction and promise dispatch.

use std::sync::Arc;

use neon::prelude::*;

use cognee_bindings_common::ops::data;

use crate::errors::throw_sdk_error;
use crate::json::{js_to_value, parse_js, read_opts};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;

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
        let result = data::forget(&state, target_json, &opts)
            .await
            .and_then(|v| {
                serde_json::to_string(&v).map_err(|e| {
                    cognee_bindings_common::SdkError::Runtime(format!(
                        "failed to serialize ForgetResult: {e}"
                    ))
                })
            });
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
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
        let result = data::update(&state, &data_id_str, new_data_json, &dataset_name, &opts)
            .await
            .and_then(|v| {
                serde_json::to_string(&v).map_err(|e| {
                    cognee_bindings_common::SdkError::Runtime(format!(
                        "failed to serialize UpdateResult: {e}"
                    ))
                })
            });
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
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
        let result = data::prune_data(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(_null_val) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
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
        let result = data::prune_system(&state, &opts)
            .await
            .and_then(|v| {
                serde_json::to_string(&v).map_err(|e| {
                    cognee_bindings_common::SdkError::Runtime(format!(
                        "failed to serialize PruneResult: {e}"
                    ))
                })
            });
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}
