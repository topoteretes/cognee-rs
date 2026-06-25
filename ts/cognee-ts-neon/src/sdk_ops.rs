//! Phase 3 — pipeline ops: `add`, `cognify`, `add-and-cognify`.
//!
//! Each export follows the Phase-1 canonical pattern: clone the
//! `Arc<HandleState>` into `runtime().spawn`, settle the promise from a
//! tokio worker thread.
//!
//! ## Shared async logic
//!
//! The pure-Rust async op bodies and all their helpers now live in
//! `cognee_bindings_common::ops::pipeline`. This file contains only the
//! Neon-specific wrappers: argument extraction from `FunctionContext`, promise
//! creation, and result delivery via `deferred.settle_with`.

use std::sync::Arc;

use neon::prelude::*;

use cognee_bindings_common::ops::pipeline;

use crate::errors::throw_sdk_error;
use crate::json::{js_to_value, parse_js};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;

/// `cogneeAdd(handle, dataInput, datasetName, opts?) -> Promise<AddResult>`
pub fn cognee_add(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let data_arg = cx.argument::<JsValue>(1)?;
    let inputs_json = js_to_value(&mut cx, data_arg)?;
    let dataset_name = cx.argument::<JsString>(2)?.value(&mut cx);
    let opts = match cx.argument_opt(3) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = pipeline::add(&state, inputs_json, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(value) => parse_js(&mut cx, &value.to_string()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeCognify(handle, dataset, opts?) -> Promise<CognifyResult>`
pub fn cognee_cognify(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let dataset_name = cx.argument::<JsString>(1)?.value(&mut cx);
    let opts = match cx.argument_opt(2) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = pipeline::cognify(&state, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(value) => parse_js(&mut cx, &value.to_string()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeAddAndCognify(handle, dataInput, datasetName, opts?) -> Promise<{ add, cognify }>`
///
/// One native call: add first, then cognify the just-added `Vec<Data>` directly
/// (mirroring `commands/add_and_cognify.rs`). If `add` returns an empty vec
/// (everything was a duplicate), cognify is skipped and a zeroed summary is
/// returned.
pub fn cognee_add_and_cognify(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let data_arg = cx.argument::<JsValue>(1)?;
    let inputs_json = js_to_value(&mut cx, data_arg)?;
    let dataset_name = cx.argument::<JsString>(2)?.value(&mut cx);
    let opts = match cx.argument_opt(3) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = pipeline::add_and_cognify(&state, inputs_json, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(value) => parse_js(&mut cx, &value.to_string()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}
