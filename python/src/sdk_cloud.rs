//! Python-facing wrappers for the two cloud operations: `serve` and `disconnect`.
//!
//! Both operations are feature-gated on `cloud`. When the feature is not
//! compiled in, the functions return `CogneeFeatureNotBuiltError` (matching
//! the C API and Neon behaviour).
//!
//! The async bridge uses `pyo3_async_runtimes::tokio` (same pattern as other
//! `sdk_*.rs` modules). Key normalisation via `opts_to_camel_json` ensures
//! Python `"wipe_credentials"` reaches the shared Rust op as
//! `"wipeCredentials"`.
//!
//! Both operations are **module-level** (not methods on `PyCognee`) because
//! they operate on the process-wide `CloudClient` singleton — matching the
//! C API and Neon pattern.

use cognee_bindings_common::ops::cloud;
use pyo3::prelude::*;

use crate::json::{opts_to_camel_json, serde_to_py};
use crate::sdk_error::sdk_error_to_py;

// ---------------------------------------------------------------------------
// Public async bodies (called by the `#[pyfunction]` wrappers in lib.rs).
// ---------------------------------------------------------------------------

/// Async body for the module-level `serve` function.
///
/// Connects to a Cognee Cloud instance. When `opts["url"]` (or
/// `opts["url"]` after snake→camel normalisation) is set, **direct mode** is
/// used (headless, suitable for CI / tests). Otherwise the Auth0 device-code
/// flow is run (requires a TTY).
///
/// Returns a dict `{"connected": True, "serviceUrl": "…"}` on success.
///
/// When the `cloud` feature is not compiled in, raises
/// `CogneeFeatureNotBuiltError`.
pub fn py_serve<'py>(
    py: Python<'py>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_val = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = cloud::run_serve(opts_val).await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for the module-level `disconnect` function.
///
/// Disconnects from Cognee Cloud and reverts to local-execution mode.
/// `opts["wipe_credentials"]` (or `opts["wipeCredentials"]`) controls whether
/// the on-disk credential cache is deleted (default `False`).
///
/// Returns `None` on success.
///
/// When the `cloud` feature is not compiled in, raises
/// `CogneeFeatureNotBuiltError`.
pub fn py_disconnect<'py>(
    py: Python<'py>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_val = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        cloud::run_disconnect(opts_val)
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| Ok(py.None()))
    })
}
