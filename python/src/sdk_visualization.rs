//! Python-facing wrappers for the two visualization operations:
//! `visualize` and `visualize_to_file`.
//!
//! Both operations are feature-gated on `visualization`. When the feature is
//! not compiled in, the methods are still present but return
//! `CogneeFeatureNotBuiltError` (matching the C API and Neon behaviour).
//!
//! The async bridge uses `pyo3_async_runtimes::tokio` (same pattern as other
//! `sdk_*.rs` modules). Key normalisation via `opts_to_camel_json` ensures
//! Python `"destination_path"` reaches the shared Rust op as
//! `"destinationPath"`.
//!
//! The methods are exposed via a separate `#[pymethods]` block on `PyCognee`
//! at the bottom of this file, keeping `sdk.rs` focused on handle construction
//! and lifecycle methods.

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::visualization;
use pyo3::prelude::*;

use crate::json::opts_to_camel_json;
use crate::sdk::PyCognee;
use crate::sdk_error::sdk_error_to_py;

// ---------------------------------------------------------------------------
// Free functions (called by the `#[pymethods]` impl below).
// ---------------------------------------------------------------------------

/// Async body for `PyCognee.visualize`.
///
/// Returns the full self-contained HTML document as a Python `str`.
/// `opts` is accepted for API symmetry but no keys are currently consumed.
///
/// When the `visualization` feature is not compiled in, raises
/// `CogneeFeatureNotBuiltError`.
pub fn py_sdk_visualize<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_val = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let html: String = visualization::visualize(&handle, Some(&opts_val))
            .await
            .map_err(sdk_error_to_py)?;
        Ok(html)
    })
}

/// Async body for `PyCognee.visualize_to_file`.
///
/// Writes the HTML visualization to disk and returns the absolute path as a
/// Python `str`. `opts["destination_path"]` (or `opts["destinationPath"]`)
/// overrides the default `~/graph_visualization.html` destination.
///
/// When the `visualization` feature is not compiled in, raises
/// `CogneeFeatureNotBuiltError`.
pub fn py_sdk_visualize_to_file<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_val = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let path: String = visualization::visualize_to_file(&handle, Some(&opts_val))
            .await
            .map_err(sdk_error_to_py)?;
        Ok(path)
    })
}

// ---------------------------------------------------------------------------
// `#[pymethods]` impl block — wired onto `PyCognee`.
// ---------------------------------------------------------------------------

#[pymethods]
impl PyCognee {
    /// Render the knowledge graph as a self-contained d3.js HTML document.
    ///
    /// .. code-block:: python
    ///
    ///     html = await cognee.visualize()
    ///     assert "<!DOCTYPE html>" in html or "<html" in html
    ///
    /// Returns the full HTML document as a ``str``. For large graphs, prefer
    /// :meth:`visualize_to_file` to avoid holding the full HTML in memory.
    ///
    /// ``opts`` is accepted for forward-compatibility; no keys are currently
    /// consumed.
    ///
    /// Raises ``CogneeFeatureNotBuiltError`` when the ``visualization`` Cargo
    /// feature was not compiled in.
    #[pyo3(signature = (opts=None))]
    fn visualize<'py>(
        &self,
        py: Python<'py>,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_visualize(py, Arc::clone(&self.inner), opts)
    }

    /// Render the knowledge graph to a file and return the written path.
    ///
    /// .. code-block:: python
    ///
    ///     path = await cognee.visualize_to_file({"destination_path": "/tmp/graph.html"})
    ///     assert path.endswith(".html")
    ///     import os; assert os.path.isfile(path)
    ///
    /// Returns the absolute path of the written file as a ``str``.
    ///
    /// Supported ``opts`` keys (both ``snake_case`` and ``camelCase`` accepted):
    ///
    /// - ``destination_path`` / ``destinationPath`` — override the default
    ///   ``~/graph_visualization.html`` output path.
    ///
    /// Raises ``CogneeFeatureNotBuiltError`` when the ``visualization`` Cargo
    /// feature was not compiled in.
    #[pyo3(signature = (opts=None))]
    fn visualize_to_file<'py>(
        &self,
        py: Python<'py>,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_visualize_to_file(py, Arc::clone(&self.inner), opts)
    }
}
