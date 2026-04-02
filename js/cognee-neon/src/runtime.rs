use std::sync::OnceLock;

use neon::prelude::*;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Get a reference to the global tokio runtime.
/// Panics if [`init`] or [`init_with_threads`] has not been called.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME
        .get()
        .expect("cognee-neon: runtime not initialised – call init() first")
}

fn build_runtime(threads: Option<usize>) -> Result<tokio::runtime::Runtime, String> {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = threads {
        builder.worker_threads(n);
    }
    builder
        .enable_all()
        .build()
        .map_err(|e| format!("failed to build tokio runtime: {e}"))
}

pub fn init(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    RUNTIME
        .set(build_runtime(None).or_else(|e| cx.throw_error(e))?)
        .map_err(|_| ())
        .or_else(|_| cx.throw_error("runtime already initialised"))?;
    Ok(cx.undefined())
}

pub fn init_with_threads(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let n = cx.argument::<JsNumber>(0)?.value(&mut cx) as usize;
    RUNTIME
        .set(build_runtime(Some(n)).or_else(|e| cx.throw_error(e))?)
        .map_err(|_| ())
        .or_else(|_| cx.throw_error("runtime already initialised"))?;
    Ok(cx.undefined())
}

pub fn shutdown(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    // No-op — runtime lives until process exit (same as capi).
    Ok(cx.undefined())
}
