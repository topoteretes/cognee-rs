use std::sync::OnceLock;

use cognee_core::AsyncRuntime;

use crate::error::{CgErrorCode, set_last_error};
use crate::panic_hook;

static GLOBAL_RUNTIME: OnceLock<AsyncRuntime> = OnceLock::new();

/// Arm product-analytics emission so the `COGNEE_HOST_SDK` clause inside
/// `cognee_telemetry::env::is_disabled` becomes authoritative for any
/// binding-hosted `send_telemetry` call (decision 10). Idempotent.
///
/// Mirrors the auto-arm the PyO3/Neon bindings perform at module load,
/// so a C embedder gets uniform on-by-default analytics (Python-SDK
/// parity) with `COGNEE_HOST_SDK` deferral working even without an
/// explicit `cognee_init_telemetry()` call. Arming only ever *adds*
/// suppression — emission is still gated per event by `is_disabled()`
/// (`TELEMETRY_DISABLED` / `ENV` / `COGNEE_HOST_SDK`). No-op when the
/// `telemetry` feature is disabled.
#[cfg(feature = "telemetry")]
#[inline]
fn arm_telemetry_analytics() {
    cognee_telemetry::env::arm_binding_emission();
}

#[cfg(not(feature = "telemetry"))]
#[inline]
fn arm_telemetry_analytics() {}

fn init_runtime(rt: AsyncRuntime) -> CgErrorCode {
    match GLOBAL_RUNTIME.set(rt) {
        Ok(()) => CgErrorCode::Ok,
        Err(_) => {
            set_last_error("runtime already initialized");
            CgErrorCode::InvalidConfig
        }
    }
}

/// Initialize the global async runtime with default settings.
///
/// Also installs a process-wide panic hook (one-shot) that writes
/// `[cognee-capi panic]` records to stderr. Subsequent calls do
/// not replace the hook.
///
/// Must be called before `cg_pipeline_execute_in_background` or
/// `cg_pipeline_execute_async`. Safe to call multiple times (second call
/// returns an error but is harmless; the panic hook is only installed
/// on the first successful call).
#[unsafe(no_mangle)]
pub extern "C" fn cg_init() -> CgErrorCode {
    panic_hook::install_once();
    arm_telemetry_analytics();
    match AsyncRuntime::new() {
        Ok(rt) => init_runtime(rt),
        Err(e) => {
            set_last_error(e.to_string());
            CgErrorCode::RuntimeError
        }
    }
}

/// Initialize the global async runtime with `n` worker threads.
///
/// Also installs a process-wide panic hook (one-shot) that writes
/// `[cognee-capi panic]` records to stderr. Subsequent calls do
/// not replace the hook.
#[unsafe(no_mangle)]
pub extern "C" fn cg_init_with_threads(n: usize) -> CgErrorCode {
    panic_hook::install_once();
    arm_telemetry_analytics();
    if n == 0 {
        set_last_error("thread count must be > 0");
        return CgErrorCode::InvalidArgument;
    }
    match AsyncRuntime::multi_thread(n) {
        Ok(rt) => init_runtime(rt),
        Err(e) => {
            set_last_error(e.to_string());
            CgErrorCode::RuntimeError
        }
    }
}

/// Shut down the global runtime. After this, no async operations can be
/// performed. Currently a no-op (the runtime is dropped when the process exits).
#[unsafe(no_mangle)]
pub extern "C" fn cg_shutdown() {
    // OnceLock doesn't support taking the value out, but the runtime will be
    // dropped at process exit. This function exists for API completeness.
}

/// Get a reference to the global runtime. Returns `None` if `cg_init` was not
/// called.
pub(crate) fn global_runtime() -> Option<&'static AsyncRuntime> {
    GLOBAL_RUNTIME.get()
}

/// Ensure the global runtime is initialised, initialising it now if needed.
///
/// Idempotent: if the runtime is already running this is a no-op that returns
/// `CgErrorCode::Ok`. Used by `cg_sdk_new` so callers do not need to call
/// `cg_init` explicitly before constructing a handle (R7 ordering footgun:
/// `cg_init_with_threads` must still be called **before** `cg_sdk_new` when a
/// custom thread count is desired, since the OnceLock no-ops on the second
/// call).
pub(crate) fn ensure_runtime() -> CgErrorCode {
    if GLOBAL_RUNTIME.get().is_some() {
        return CgErrorCode::Ok;
    }
    cg_init()
}
