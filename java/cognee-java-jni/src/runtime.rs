//! Process-wide tokio runtime for the Java binding (mirrors cognee-ts-neon).

use std::sync::OnceLock;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Return the global runtime, building it on first use. Race-safe: a lost
/// `set` race drops the loser and returns the winner.
pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    if let Some(rt) = RUNTIME.get() {
        return rt;
    }
    let candidate = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("cognee-java: failed to build the tokio runtime");
    let _ = RUNTIME.set(candidate);
    RUNTIME
        .get()
        .expect("runtime is set: either by this call or a concurrent initializer")
}
