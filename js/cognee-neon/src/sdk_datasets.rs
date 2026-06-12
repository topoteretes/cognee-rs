//! Phase 5 — DatasetManager ops (#12).
//!
//! All exports follow the canonical pattern:
//!   `Arc::clone(&handle.state)` → `runtime().spawn` → shared op in
//!   `cognee_bindings_common::ops::datasets` → `settle_with`.
//!
//! The async business logic has been hoisted into
//! `cognee_bindings_common::ops::datasets`; this file contains only the
//! Neon JS promise-settling shim layer.

use std::sync::Arc;

use neon::prelude::*;

use cognee_bindings_common::ops::datasets;
use cognee_bindings_common::SdkError;

use crate::errors::{throw_sdk_error};
use crate::json::{js_to_value, parse_js, read_opts};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;

// ---------------------------------------------------------------------------
// cogneeListDatasets
// ---------------------------------------------------------------------------

/// `cogneeListDatasets(handle) -> Promise<Dataset[]>`
pub fn cognee_list_datasets(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = datasets::list_datasets(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).map_err(|e| {
                    cx.throw_error::<_, ()>(format!("serialization error: {e}"))
                        .unwrap_err()
                })?;
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeListData
// ---------------------------------------------------------------------------

/// `cogneeListData(handle, datasetId) -> Promise<Data[]>`
pub fn cognee_list_data(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = datasets::list_data(&state, &dataset_id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).map_err(|e| {
                    cx.throw_error::<_, ()>(format!("serialization error: {e}"))
                        .unwrap_err()
                })?;
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeHasData
// ---------------------------------------------------------------------------

/// `cogneeHasData(handle, datasetId) -> Promise<boolean>`
pub fn cognee_has_data(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = datasets::has_data(&state, &dataset_id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let b = val.as_bool().ok_or_else(|| {
                    SdkError::Runtime("has_data returned non-bool".to_string())
                });
                match b {
                    Ok(b) => Ok(cx.boolean(b)),
                    Err(e) => throw_sdk_error(&mut cx, e),
                }
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeDatasetStatus
// ---------------------------------------------------------------------------

/// `cogneeDatasetStatus(handle, datasetIds) -> Promise<Record<string, string>>`
///
/// `datasetIds` is a JSON array of UUID strings.
/// Returns `{ [uuidStr]: "INITIATED" | "STARTED" | "COMPLETED" | "ERRORED" }`.
pub fn cognee_dataset_status(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let ids_arg = cx.argument::<JsValue>(1)?;
    let ids_json = js_to_value(&mut cx, ids_arg)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = datasets::dataset_status(&state, ids_json).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).map_err(|e| {
                    cx.throw_error::<_, ()>(format!("serialization error: {e}"))
                        .unwrap_err()
                })?;
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeEmptyDataset
// ---------------------------------------------------------------------------

/// `cogneeEmptyDataset(handle, datasetId) -> Promise<DeleteResult>`
pub fn cognee_empty_dataset(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = datasets::empty_dataset(&state, &dataset_id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).map_err(|e| {
                    cx.throw_error::<_, ()>(format!("serialization error: {e}"))
                        .unwrap_err()
                })?;
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeDeleteData
// ---------------------------------------------------------------------------

/// `cogneeDeleteData(handle, datasetId, dataId, opts?) -> Promise<DeleteResult>`
///
/// opts: `{ softDelete?: boolean, deleteDatasetIfEmpty?: boolean }`
pub fn cognee_delete_data(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);
    let data_id_str = cx.argument::<JsString>(2)?.value(&mut cx);
    let opts = read_opts(&mut cx, 3)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = datasets::delete_data(&state, &dataset_id_str, &data_id_str, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).map_err(|e| {
                    cx.throw_error::<_, ()>(format!("serialization error: {e}"))
                        .unwrap_err()
                })?;
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeDeleteAllDatasets
// ---------------------------------------------------------------------------

/// `cogneeDeleteAllDatasets(handle) -> Promise<DeleteResult[]>`
pub fn cognee_delete_all_datasets(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = datasets::delete_all_datasets(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).map_err(|e| {
                    cx.throw_error::<_, ()>(format!("serialization error: {e}"))
                        .unwrap_err()
                })?;
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}
