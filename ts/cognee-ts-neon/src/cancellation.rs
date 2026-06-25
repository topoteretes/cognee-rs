use neon::prelude::*;

use cognee_core::{CancellationHandle, CancellationToken, cancellation_pair};

pub struct NeonCancellationHandle {
    pub inner: CancellationHandle,
}

impl Finalize for NeonCancellationHandle {}

pub struct NeonCancellationToken {
    pub inner: CancellationToken,
}

impl Finalize for NeonCancellationToken {}

/// Create a linked (handle, token) pair.
///
/// Returns `{ handle: CancellationHandle, token: CancellationToken }`.
pub fn cancellation_pair_new(mut cx: FunctionContext) -> JsResult<JsObject> {
    let (handle, token) = cancellation_pair();

    let result = cx.empty_object();
    let js_handle = cx.boxed(NeonCancellationHandle { inner: handle });
    let js_token = cx.boxed(NeonCancellationToken { inner: token });
    result.set(&mut cx, "handle", js_handle)?;
    result.set(&mut cx, "token", js_token)?;
    Ok(result)
}

pub fn cancellation_handle_cancel(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let handle = cx.argument::<JsBox<NeonCancellationHandle>>(0)?;
    handle.inner.cancel();
    Ok(cx.undefined())
}

pub fn cancellation_handle_is_cancelled(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let handle = cx.argument::<JsBox<NeonCancellationHandle>>(0)?;
    Ok(cx.boolean(handle.inner.is_cancelled()))
}

pub fn cancellation_token_is_cancelled(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let token = cx.argument::<JsBox<NeonCancellationToken>>(0)?;
    Ok(cx.boolean(token.inner.is_cancelled()))
}

pub fn cancellation_handle_clone(
    mut cx: FunctionContext,
) -> JsResult<JsBox<NeonCancellationHandle>> {
    let handle = cx.argument::<JsBox<NeonCancellationHandle>>(0)?;
    Ok(cx.boxed(NeonCancellationHandle {
        inner: handle.inner.clone(),
    }))
}

pub fn cancellation_token_clone(mut cx: FunctionContext) -> JsResult<JsBox<NeonCancellationToken>> {
    let token = cx.argument::<JsBox<NeonCancellationToken>>(0)?;
    Ok(cx.boxed(NeonCancellationToken {
        inner: token.inner.clone(),
    }))
}
