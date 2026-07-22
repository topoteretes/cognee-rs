//! Process-wide tokio runtime for the Java binding (mirrors cognee-ts-neon).

use std::sync::OnceLock;

use cognee_bindings_common::SdkError;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Return the global runtime, building it on first use. Race-safe: a lost
/// `set` race drops the loser and returns the winner.
///
/// Building the runtime can legitimately fail at runtime (file-descriptor or
/// thread exhaustion), so the failure is surfaced as `SdkError::Runtime`
/// instead of panicking across the JNI boundary.
pub(crate) fn runtime() -> Result<&'static tokio::runtime::Runtime, SdkError> {
    if let Some(rt) = RUNTIME.get() {
        return Ok(rt);
    }
    let candidate = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| SdkError::Runtime(format!("failed to build the tokio runtime: {e}")))?;
    let _ = RUNTIME.set(candidate);
    Ok(RUNTIME
        .get()
        .expect("runtime is set: either by this call or a concurrent initializer"))
}
