use std::sync::Arc;

use neon::prelude::*;
use neon::types::buffer::TypedArray;

use cognee_core::Value;

/// Opaque Rust value stored in a `JsBox`.
pub struct NeonValue {
    pub inner: Arc<dyn Value>,
}

impl Finalize for NeonValue {}

/// Wraps an arbitrary JS object as a `Value` so it can travel through the pipeline.
///
/// We store a `Root<JsObject>` because `Root<JsValue>` doesn't support `to_inner`
/// in Neon (JsValue doesn't implement the Object trait). Opaque JS values must
/// therefore be objects (not primitives).
pub struct JsObjectHolder {
    pub root: Root<JsObject>,
}

// Root<JsObject> is Send. We add Sync so the blanket Value impl applies.
unsafe impl Sync for JsObjectHolder {}

/// Convert a JS value to `Arc<dyn Value>`.
///
/// Handles: number -> f64, boolean -> bool, string -> String,
/// Buffer -> Vec<u8>, object -> JsObjectHolder, null/undefined -> error.
pub fn js_to_value(
    cx: &mut FunctionContext,
    handle: Handle<JsValue>,
) -> NeonResult<Arc<dyn Value>> {
    // number
    if let Ok(n) = handle.downcast::<JsNumber, _>(cx) {
        return Ok(Arc::new(n.value(cx)));
    }
    // boolean
    if let Ok(b) = handle.downcast::<JsBoolean, _>(cx) {
        return Ok(Arc::new(b.value(cx)));
    }
    // string
    if let Ok(s) = handle.downcast::<JsString, _>(cx) {
        return Ok(Arc::new(s.value(cx)));
    }
    // Buffer (Uint8Array / Buffer)
    if let Ok(buf) = handle.downcast::<JsBuffer, _>(cx) {
        let bytes: Vec<u8> = buf.as_slice(cx).to_vec();
        return Ok(Arc::new(bytes));
    }
    // null / undefined
    if handle.is_a::<JsNull, _>(cx) || handle.is_a::<JsUndefined, _>(cx) {
        return cx.throw_type_error("null and undefined cannot be converted to a Value");
    }
    // Opaque JS object
    if let Ok(obj) = handle.downcast::<JsObject, _>(cx) {
        let root = obj.root(cx);
        return Ok(Arc::new(JsObjectHolder { root }));
    }
    cx.throw_type_error("unsupported JS type for Value conversion")
}

/// Convert an `Arc<dyn Value>` back to a JS value.
///
/// Tries the downcast chain: f64, bool, String, Vec<u8>, JsObjectHolder.
/// Returns `undefined` if the type is unknown.
pub fn value_to_js<'cx>(cx: &mut impl Context<'cx>, val: &dyn Value) -> JsResult<'cx, JsValue> {
    let any = val.as_any();

    if let Some(&v) = any.downcast_ref::<f64>() {
        return Ok(cx.number(v).upcast());
    }
    if let Some(&v) = any.downcast_ref::<bool>() {
        return Ok(cx.boolean(v).upcast());
    }
    if let Some(v) = any.downcast_ref::<String>() {
        return Ok(cx.string(v).upcast());
    }
    if let Some(v) = any.downcast_ref::<Vec<u8>>() {
        let mut buf = cx.buffer(v.len())?;
        buf.as_mut_slice(cx).copy_from_slice(v);
        return Ok(buf.upcast());
    }
    if let Some(holder) = any.downcast_ref::<JsObjectHolder>() {
        return Ok(holder.root.to_inner(cx).upcast());
    }

    // Unknown type — return undefined
    Ok(cx.undefined().upcast())
}

// ── Exported functions ──────────────────────────────────────────────────────

pub fn value_from_number(mut cx: FunctionContext) -> JsResult<JsBox<NeonValue>> {
    let n = cx.argument::<JsNumber>(0)?.value(&mut cx);
    Ok(cx.boxed(NeonValue { inner: Arc::new(n) }))
}

pub fn value_from_bool(mut cx: FunctionContext) -> JsResult<JsBox<NeonValue>> {
    let b = cx.argument::<JsBoolean>(0)?.value(&mut cx);
    Ok(cx.boxed(NeonValue { inner: Arc::new(b) }))
}

pub fn value_from_string(mut cx: FunctionContext) -> JsResult<JsBox<NeonValue>> {
    let s = cx.argument::<JsString>(0)?.value(&mut cx);
    Ok(cx.boxed(NeonValue { inner: Arc::new(s) }))
}

pub fn value_from_buffer(mut cx: FunctionContext) -> JsResult<JsBox<NeonValue>> {
    let buf = cx.argument::<JsBuffer>(0)?;
    let bytes: Vec<u8> = buf.as_slice(&cx).to_vec();
    Ok(cx.boxed(NeonValue {
        inner: Arc::new(bytes),
    }))
}

pub fn value_as_number(mut cx: FunctionContext) -> JsResult<JsNumber> {
    let val = cx.argument::<JsBox<NeonValue>>(0)?;
    match val.inner.as_any().downcast_ref::<f64>() {
        Some(&v) => Ok(cx.number(v)),
        None => cx.throw_type_error("value is not a number"),
    }
}

pub fn value_as_bool(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let val = cx.argument::<JsBox<NeonValue>>(0)?;
    match val.inner.as_any().downcast_ref::<bool>() {
        Some(&v) => Ok(cx.boolean(v)),
        None => cx.throw_type_error("value is not a boolean"),
    }
}

pub fn value_as_string(mut cx: FunctionContext) -> JsResult<JsString> {
    let val = cx.argument::<JsBox<NeonValue>>(0)?;
    match val.inner.as_any().downcast_ref::<String>() {
        Some(v) => Ok(cx.string(v)),
        None => cx.throw_type_error("value is not a string"),
    }
}

pub fn value_as_buffer(mut cx: FunctionContext) -> JsResult<JsBuffer> {
    let val = cx.argument::<JsBox<NeonValue>>(0)?;
    match val.inner.as_any().downcast_ref::<Vec<u8>>() {
        Some(v) => {
            let mut buf = cx.buffer(v.len())?;
            buf.as_mut_slice(&mut cx).copy_from_slice(v);
            Ok(buf)
        }
        None => cx.throw_type_error("value is not a buffer"),
    }
}

pub fn value_clone(mut cx: FunctionContext) -> JsResult<JsBox<NeonValue>> {
    let val = cx.argument::<JsBox<NeonValue>>(0)?;
    Ok(cx.boxed(NeonValue {
        inner: Arc::clone(&val.inner),
    }))
}
