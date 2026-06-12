//! Python-facing wrappers for the three core pipeline operations:
//! `add`, `cognify`, and `add_and_cognify`.
//!
//! Each function converts Python arguments to `serde_json::Value` via the
//! shared `crate::json` helpers, delegates to the shared async op in
//! `cognee_bindings_common::ops::pipeline`, and converts the result back to
//! a Python dict.  The async bridge uses `pyo3_async_runtimes::tokio` (same
//! pattern as `warm()` in `sdk.rs`).
//!
//! The methods are exposed via a separate `#[pymethods]` block on `PyCognee`
//! at the bottom of this file, keeping `sdk.rs` focused on handle construction
//! and lifecycle methods.

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::pipeline;
use pyo3::prelude::*;

use crate::json::{normalise_inputs, opts_to_camel_json, serde_to_py};
use crate::sdk::PyCognee;
use crate::sdk_error::sdk_error_to_py;

// ---------------------------------------------------------------------------
// Free functions (called by the `#[pymethods]` impl below).
// ---------------------------------------------------------------------------

/// Async body for `PyCognee.add`.
pub fn py_sdk_add<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    inputs: Bound<'py, PyAny>,
    dataset_name: &str,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let inputs_json = normalise_inputs(&inputs)?;
    let opts_json = opts_to_camel_json(opts)?;
    let dataset = dataset_name.to_owned();

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = pipeline::add(&handle, inputs_json, &dataset, &opts_json)
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for `PyCognee.cognify`.
pub fn py_sdk_cognify<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    dataset_name: &str,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_json = opts_to_camel_json(opts)?;
    let dataset = dataset_name.to_owned();

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = pipeline::cognify(&handle, &dataset, &opts_json)
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for `PyCognee.add_and_cognify`.
pub fn py_sdk_add_and_cognify<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    inputs: Bound<'py, PyAny>,
    dataset_name: &str,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let inputs_json = normalise_inputs(&inputs)?;
    let opts_json = opts_to_camel_json(opts)?;
    let dataset = dataset_name.to_owned();

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = pipeline::add_and_cognify(&handle, inputs_json, &dataset, &opts_json)
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

// ---------------------------------------------------------------------------
// `#[pymethods]` impl block — wired onto `PyCognee`.
// ---------------------------------------------------------------------------

#[pymethods]
impl PyCognee {
    /// Ingest one or more inputs into a named dataset.
    ///
    /// ``inputs`` can be a single dict or a list of dicts, each with a
    /// ``type`` key:
    ///
    /// .. code-block:: python
    ///
    ///     await cognee.add({"type": "text", "text": "Hello world"}, "demo")
    ///     await cognee.add([
    ///         {"type": "text", "text": "A"},
    ///         {"type": "text", "text": "B"},
    ///     ], "demo")
    ///     await cognee.add(
    ///         {"type": "binary", "bytes": b"...", "name": "f.txt"}, "demo"
    ///     )  # bytes/bytearray are base64-encoded automatically
    ///
    /// Returns a dict with keys ``datasetName``, ``added``, ``addedCount``,
    /// ``deduplicated``, and ``deduplicatedCount`` (all camelCase, matching
    /// the TS / C API surface).
    ///
    /// ``s3`` and ``dataItem`` input types raise ``CogneeUnsupportedError``.
    #[pyo3(signature = (inputs, dataset_name, opts=None))]
    fn add<'py>(
        &self,
        py: Python<'py>,
        inputs: Bound<'py, PyAny>,
        dataset_name: &str,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_add(py, Arc::clone(&self.inner), inputs, dataset_name, opts)
    }

    /// Extract a knowledge graph from a previously-ingested dataset.
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.cognify("demo")
    ///     print(result["entities"], result["edges"])
    ///
    /// Returns a dict with keys ``chunks``, ``entities``, ``edges``,
    /// ``summaries``, ``embeddings``, ``alreadyCompleted``, and
    /// ``priorPipelineRunId`` (all camelCase).
    ///
    /// Supported ``opts`` keys:
    /// ``tenant``, ``chunkSize``, ``chunkOverlap``, ``summarization``,
    /// ``temporalCognify``, ``triplet`` — ``snake_case`` spellings
    /// (``chunk_size``, …) are accepted too and normalised automatically.
    #[pyo3(signature = (dataset_name, opts=None))]
    fn cognify<'py>(
        &self,
        py: Python<'py>,
        dataset_name: &str,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_cognify(py, Arc::clone(&self.inner), dataset_name, opts)
    }

    /// Convenience method: ingest inputs and immediately cognify them.
    ///
    /// Equivalent to calling ``add()`` followed by ``cognify()``, but runs in
    /// a single native call and only cognifies the *newly-added* items (skips
    /// duplicates).
    ///
    /// Returns a dict with two top-level keys: ``add`` (same shape as
    /// ``add()`` result) and ``cognify`` (same shape as ``cognify()`` result).
    #[pyo3(signature = (inputs, dataset_name, opts=None))]
    fn add_and_cognify<'py>(
        &self,
        py: Python<'py>,
        inputs: Bound<'py, PyAny>,
        dataset_name: &str,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_add_and_cognify(py, Arc::clone(&self.inner), inputs, dataset_name, opts)
    }
}
