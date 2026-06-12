//! Phase 4 — retrieval ops: `search`, `recall`.
//!
//! The core async logic lives in
//! [`cognee_bindings_common::ops::retrieval`]; this module only contains the
//! Neon JS entry points that convert JS arguments and settle promises.
//!
//! ## Input marshalling
//!
//! `cogneeSearch` opts fields are camelCase JS keys that map to snake_case
//! `SearchRequest` fields; we hand-populate the struct rather than trying to
//! use `serde_json::from_value` on the whole opts object (which would require
//! matching serde names on both sides).
//!
//! `SearchType` is parsed from its SCREAMING_SNAKE_CASE serde wire name via
//! `serde_json::from_value(Value::String(s))` — the same path the HTTP server
//! uses, guaranteed to match the serde attribute.
//!
//! ## Result marshalling
//!
//! `SearchResponse` IS `Serialize` — pass through `serde_json::to_string` +
//! `parse_js`.
//!
//! `RecallResult` is NOT `Serialize` (derives only `Debug, Clone`) — hand-build
//! JSON from its fields: `items` (IS Serialize), `search_type_used` (IS
//! Serialize), `auto_routed` (bool), `search_response` (IS Serialize).

use std::sync::Arc;

use neon::prelude::*;

use cognee_bindings_common::ops::retrieval;
use cognee_bindings_common::SdkError;

use crate::errors::throw_sdk_error;
use crate::json::{js_to_value, parse_js};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;

// ---------------------------------------------------------------------------
// cogneeSearch
// ---------------------------------------------------------------------------

/// `cogneeSearch(handle, query, opts?) -> Promise<SearchResponse>`
pub fn cognee_search(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let query = cx.argument::<JsString>(1)?.value(&mut cx);
    let opts = match cx.argument_opt(2) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = retrieval::search(&state, &query, &opts)
            .await
            .map(|v| {
                serde_json::to_string(&v)
                    .map_err(|e| SdkError::Runtime(format!("failed to serialize search result: {e}")))
            });
        let result = match result {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e),
        };
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// cogneeRecall
// ---------------------------------------------------------------------------

/// `cogneeRecall(handle, query, opts?) -> Promise<RecallResult>`
///
/// `RecallResult` is NOT `Serialize` (derives only `Debug, Clone`); JSON is
/// hand-built from its fields.
pub fn cognee_recall(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let query = cx.argument::<JsString>(1)?.value(&mut cx);
    let opts = match cx.argument_opt(2) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = retrieval::recall(&state, &query, &opts)
            .await
            .map(|v| {
                serde_json::to_string(&v)
                    .map_err(|e| SdkError::Runtime(format!("failed to serialize recall result: {e}")))
            });
        let result = match result {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e),
        };
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}
