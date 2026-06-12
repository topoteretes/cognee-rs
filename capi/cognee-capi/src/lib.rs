//! C FFI bindings for the cognee-core pipeline engine.
//!
//! This crate exposes the full cognee-core API through C-compatible functions:
//!
//! - **Values**: Type-erased data containers (`CgValue`)
//! - **Tasks**: 8 task types created from C function pointers
//! - **Pipeline**: Builder + blocking/async/background execution
//! - **Context**: Task context with database/graph/vector backends
//! - **Cancellation**: Cooperative cancellation via handle/token pairs
//! - **Progress**: Lock-free progress tracking
//! - **Watcher**: Pipeline event observer via C vtable

pub mod cancellation;
pub mod error;
pub mod exec_status;
pub mod iterator;
pub mod logging;
mod panic_hook;
pub mod pipeline;
pub mod pipeline_exec;
pub mod progress;
pub mod run_handle;
pub mod runtime;
pub mod sdk;
pub mod sdk_config;
pub mod sdk_ops;
pub mod sdk_retrieval;
pub mod task;
pub mod task_context;
pub mod task_info;
pub mod telemetry_analytics;
pub mod telemetry_otlp;
pub mod thread_pool;
#[macro_use]
pub mod util;
pub mod value;
pub mod watcher;

/// Test-only no-return helper. Behind the `testing-panic` cargo
/// feature so it never leaks into release artefacts. Used by the
/// gap-07 capi smoke tests (`capi/examples/panic_hook_smoke.c`) to
/// verify the panic hook installed by `cg_init` writes
/// `[cognee-capi panic]` to stderr before unwind.
#[cfg(feature = "testing-panic")]
#[unsafe(no_mangle)]
pub extern "C" fn cg_test_force_panic() -> ! {
    panic!("test panic from gap 07 task 07-04 smoke test");
}
