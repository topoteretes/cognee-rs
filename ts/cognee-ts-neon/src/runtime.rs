use std::sync::OnceLock;

use neon::prelude::*;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Get a reference to the global tokio runtime.
/// Panics if [`init`], [`init_with_threads`], or [`ensure_runtime`] has not run.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME
        .get()
        .expect("cognee-ts-neon: runtime not initialised – call init() first")
}

/// The global tokio runtime if one has been initialised, else `None`.
///
/// Unlike [`runtime`] this never panics, and unlike [`ensure_runtime`] it never
/// builds one. Teardown paths (`cogneeClose`, the `Finalize` release) use it:
/// they need the runtime only to drive a teardown, so if there is none there was
/// also nothing to tear down, and spinning one up just to shut down would be
/// backwards.
pub fn try_runtime() -> Option<&'static tokio::runtime::Runtime> {
    RUNTIME.get()
}

/// Initialise the global tokio runtime on first use and return a reference to
/// it. Unlike [`init`], this is idempotent and never errors when the runtime is
/// already set — it is the entry point the `CogneeHandle` path uses so callers
/// never have to call `init()` explicitly.
pub fn ensure_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    // Fast path: already initialised.
    if let Some(rt) = RUNTIME.get() {
        return Ok(rt);
    }
    // Build a candidate; `set` may lose the race with another thread, in which
    // case we drop ours and use the winner via `get`.
    let candidate = build_runtime(None)?;
    let _ = RUNTIME.set(candidate);
    Ok(RUNTIME
        .get()
        .expect("runtime is set: either by this call or a concurrent ensure_runtime/init"))
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
    // Idempotent: an already-initialised runtime is treated as success so the
    // `CogneeHandle` path (which lazily ensures the runtime) and an explicit
    // `init()` call can coexist without throwing "already initialised".
    if RUNTIME.get().is_none() {
        let _ = RUNTIME.set(build_runtime(None).or_else(|e| cx.throw_error(e))?);
    }
    Ok(cx.undefined())
}

pub fn init_with_threads(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let n = cx.argument::<JsNumber>(0)?.value(&mut cx) as usize;
    // Idempotent like `init`. If the runtime already exists the requested
    // thread count is ignored (the first initialisation wins).
    if RUNTIME.get().is_none() {
        let _ = RUNTIME.set(build_runtime(Some(n)).or_else(|e| cx.throw_error(e))?);
    }
    Ok(cx.undefined())
}

pub fn shutdown(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    // No-op — runtime lives until process exit (same as capi).
    Ok(cx.undefined())
}
