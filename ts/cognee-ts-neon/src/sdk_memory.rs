//! Phase 5 — memory ops: remember / remember_entry / memify / improve.
//!
//! Each export follows the canonical pattern:
//!   `Arc::clone(&handle.state)` → `runtime().spawn` → delegate to
//!   `cognee_bindings_common::ops::memory` shared async bodies
//!   → `settle_with`.
//!
//! The shared async bodies (run_remember / run_remember_entry / run_memify_op /
//! run_improve) now live in `cognee-bindings-common` so they can be reused by
//! the Python and C API bindings without duplication.

use std::sync::Arc;

use neon::prelude::*;

use cognee_bindings_common::ops::memory;

use crate::errors::throw_sdk_error;
use crate::json::{js_to_value, parse_js, read_opts};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;

// ---------------------------------------------------------------------------
// cogneeRemember
// ---------------------------------------------------------------------------

/// `cogneeRemember(handle, dataInput, datasetName, opts?) -> Promise<RememberResult>`
///
/// opts: `{ sessionId?, selfImprovement?, tenant? }`
///
/// `RememberResult` derives `Serialize` — result is passed through
/// `serde_json::to_value` directly (cognify_result / memify_result fields are
/// `#[serde(skip)]` so they are invisible to JS).
pub fn cognee_remember(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let data_arg = cx.argument::<JsValue>(1)?;
    let inputs_json = js_to_value(&mut cx, data_arg)?;
    let dataset_name = cx.argument::<JsString>(2)?.value(&mut cx);
    let opts = read_opts(&mut cx, 3)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = memory::run_remember(&state, inputs_json, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = match serde_json::to_string(&val) {
                    Ok(s) => s,
                    Err(e) => {
                        return cx.throw_error(format!("serialization error: {e}"));
                    }
                };
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeRememberEntry
// ---------------------------------------------------------------------------

/// `cogneeRememberEntry(handle, entry, datasetName, sessionId, opts?) -> Promise<RememberResult>`
///
/// `entry` is a discriminated union:
///   `{ type: "qa", question, answer, context?, feedbackText?, feedbackScore?, usedGraphElementIds? }`
///   `{ type: "trace", originFunction, status, memoryQuery?, memoryContext?, methodParams?,
///      methodReturnValue?, errorMessage?, generateFeedbackWithLlm? }`
///   `{ type: "feedback", qaId, feedbackText?, feedbackScore? }`
pub fn cognee_remember_entry(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let entry_arg = cx.argument::<JsValue>(1)?;
    let entry_json = js_to_value(&mut cx, entry_arg)?;
    let dataset_name = cx.argument::<JsString>(2)?.value(&mut cx);
    let session_id = cx.argument::<JsString>(3)?.value(&mut cx);
    let opts = read_opts(&mut cx, 4)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result =
            memory::run_remember_entry(&state, entry_json, &dataset_name, &session_id, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = match serde_json::to_string(&val) {
                    Ok(s) => s,
                    Err(e) => {
                        return cx.throw_error(format!("serialization error: {e}"));
                    }
                };
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeMemify
// ---------------------------------------------------------------------------

/// `cogneeMemify(handle, opts?) -> Promise<MemifyResult>`
///
/// opts: `{ tripletBatchSize?, nodeTypeFilter?, nodeNameFilter?, nodeNameFilterOperator? }`
///
/// NOTE: `extraction_tasks`, `enrichment_tasks`, `custom_data` fields in
/// `MemifyConfig` are `#[serde(skip)]` closures and cannot be passed from JS.
pub fn cognee_memify(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let opts = read_opts(&mut cx, 1)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = memory::run_memify_op(&state, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = match serde_json::to_string(&val) {
                    Ok(s) => s,
                    Err(e) => {
                        return cx.throw_error(format!("serialization error: {e}"));
                    }
                };
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeImprove
// ---------------------------------------------------------------------------

/// `cogneeImprove(handle, opts) -> Promise<ImproveResult>`
///
/// opts: `{ datasetName, sessionIds?, nodeName?, feedbackAlpha?, tenant? }`
///
/// `ImproveResult` does NOT derive `Serialize` → hand-built JSON (done in the
/// shared `run_improve` in bindings-common).
pub fn cognee_improve(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let opts = read_opts(&mut cx, 1)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = memory::run_improve(&state, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = match serde_json::to_string(&val) {
                    Ok(s) => s,
                    Err(e) => {
                        return cx.throw_error(format!("serialization error: {e}"));
                    }
                };
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}
