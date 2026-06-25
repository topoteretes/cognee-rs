use std::sync::Mutex;

use neon::prelude::*;

use cognee_core::PipelineRunHandle;

use crate::error::throw_execution_error;
use crate::value::value_to_js;

/// Wrapper around `PipelineRunHandle`.
///
/// Uses `Mutex<Option<...>>` because `wait()` consumes the handle.
pub struct NeonRunHandle {
    pub inner: Mutex<Option<PipelineRunHandle>>,
}

impl Finalize for NeonRunHandle {}

pub fn run_handle_is_finished(mut cx: FunctionContext) -> JsResult<JsBoolean> {
    let handle = cx.argument::<JsBox<NeonRunHandle>>(0)?;
    let guard = handle.inner.lock().unwrap(); // lock poison is unrecoverable
    let finished = match guard.as_ref() {
        Some(h) => h.is_finished(),
        None => true, // Already consumed = finished.
    };
    Ok(cx.boolean(finished))
}

pub fn run_handle_abort(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let handle = cx.argument::<JsBox<NeonRunHandle>>(0)?;
    let guard = handle.inner.lock().unwrap(); // lock poison is unrecoverable
    if let Some(h) = guard.as_ref() {
        h.abort();
    }
    Ok(cx.undefined())
}

/// Wait for the background run to complete. Returns a Promise.
///
/// Consumes the handle — subsequent calls will reject.
pub fn run_handle_wait(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle_box = cx.argument::<JsBox<NeonRunHandle>>(0)?;
    let handle = handle_box.inner.lock().unwrap().take(); // lock poison is unrecoverable

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    match handle {
        None => {
            deferred.settle_with(&channel, |mut cx| {
                cx.throw_error::<_, Handle<JsValue>>("run handle already consumed")
            });
        }
        Some(h) => {
            let rt = crate::runtime::runtime();
            rt.spawn(async move {
                let result = h.wait().await;
                deferred.settle_with(&channel, move |mut cx| match result {
                    Ok(run_result) => {
                        let arr = JsArray::new(&mut cx, run_result.outputs.len());
                        for (i, val) in run_result.outputs.iter().enumerate() {
                            let js_val = value_to_js(&mut cx, val.as_ref())?;
                            arr.set(&mut cx, i as u32, js_val)?;
                        }
                        Ok(arr)
                    }
                    Err(e) => throw_execution_error(&mut cx, e),
                });
            });
        }
    }

    Ok(promise)
}
