//! Single canonical JSON marshalling path for the Neon bindings.
//!
//! Every JS ↔ `serde_json::Value` conversion in the binding layer must go
//! through these helpers — **no private copies** should exist in the
//! individual `sdk_*.rs` or `config.rs` modules.
//!
//! ## Design
//!
//! All conversions round-trip through the JS engine's own `JSON.stringify` /
//! `JSON.parse` so they never diverge from how JavaScript itself would
//! serialise a value.  A recursive Neon value-tree walk would be faster but
//! is brittle (special-cases for `undefined`, `Date`, `BigInt`, …); the
//! string round-trip is simpler and correct.
//!
//! ## Shared result-building helpers
//!
//! The neon-free helpers (`cognify_result_json`, `marshal_inputs`, `marshal_one`,
//! `marshal_bytes`) have moved to `cognee_bindings_common::wire` so they can be
//! shared with the C API binding. They are re-exported here for backwards
//! compatibility.

use neon::prelude::*;

// Re-export neon-free helpers from the shared crate. All existing call-sites
// in sdk_*.rs / sdk_ops.rs continue to work unchanged.
pub(crate) use cognee_bindings_common::wire::{cognify_result_json, marshal_inputs};

// ---------------------------------------------------------------------------
// Core JSON helpers (neon-specific — require Context/Handle params).
// These stay in this module because they depend on neon::prelude::*.
// ---------------------------------------------------------------------------

/// Stringify a JS value via the global `JSON.stringify`.
///
/// Used to convert an arbitrary JS value into a Rust `String` before
/// deserialising into `serde_json::Value`.
pub(crate) fn stringify_js<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<String> {
    let global = cx.global_object();
    let json: Handle<JsObject> = global.get(cx, "JSON")?;
    let stringify: Handle<JsFunction> = json.get(cx, "stringify")?;
    let result: Handle<JsValue> = stringify.call_with(cx).arg(val).apply(cx)?;
    let s = result.downcast_or_throw::<JsString, _>(cx)?;
    Ok(s.value(cx))
}

/// Parse a JSON string into a JS value via the global `JSON.parse`.
///
/// Generic over `Context` so it works both in a `FunctionContext` and inside a
/// promise's `settle_with` callback (which provides a `TaskContext`).
pub(crate) fn parse_js<'cx, C: Context<'cx>>(cx: &mut C, json: &str) -> JsResult<'cx, JsValue> {
    let global = cx.global_object();
    let json_obj: Handle<JsObject> = global.get(cx, "JSON")?;
    let parse: Handle<JsFunction> = json_obj.get(cx, "parse")?;
    let arg = cx.string(json);
    parse.call_with(cx).arg(arg).apply(cx)
}

/// Convert a JS value into a `serde_json::Value` via `JSON.stringify`.
pub(crate) fn js_to_serde<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<serde_json::Value> {
    let json = stringify_js(cx, val)?;
    serde_json::from_str::<serde_json::Value>(&json)
        .or_else(|e| cx.throw_error(format!("invalid JSON value: {e}")))
}

/// Read an optional JS argument at position `idx` into a `serde_json::Value`.
///
/// Returns `serde_json::Value::Null` when the argument is absent, `undefined`,
/// or `null`.
pub(crate) fn read_opts(cx: &mut FunctionContext<'_>, idx: usize) -> NeonResult<serde_json::Value> {
    match cx.argument_opt(idx) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(cx) && !arg.is_a::<JsNull, _>(cx) => {
            js_to_serde(cx, arg)
        }
        _ => Ok(serde_json::Value::Null),
    }
}

// ---------------------------------------------------------------------------
// Backwards-compat aliases (used in sdk_memory.rs public API).
// ---------------------------------------------------------------------------

/// Alias for [`js_to_serde`] — kept for call-sites that already used the old name.
#[inline]
pub(crate) fn js_to_value<'cx>(
    cx: &mut FunctionContext<'cx>,
    val: Handle<'cx, JsValue>,
) -> NeonResult<serde_json::Value> {
    js_to_serde(cx, val)
}
