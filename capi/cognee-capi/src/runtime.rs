use std::sync::OnceLock;

use cognee_core::AsyncRuntime;

use crate::error::{CgErrorCode, set_last_error};
use crate::panic_hook;

static GLOBAL_RUNTIME: OnceLock<AsyncRuntime> = OnceLock::new();

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
