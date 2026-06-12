//! Python-facing wrappers for memory operations:
//! `remember`, `remember_entry`, `memify`, and `improve`.
//!
//! Each function converts Python arguments to `serde_json::Value` via the
//! shared `crate::json` helpers, delegates to the shared async op in
//! `cognee_bindings_common::ops::memory`, and converts the result back to
//! a Python dict.  The async bridge uses `pyo3_async_runtimes::tokio` (same
//! pattern as the other `sdk_*.rs` modules).
//!
//! The methods are exposed via a separate `#[pymethods]` block on `PyCognee`
//! at the bottom of this file.

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::memory;
use pyo3::prelude::*;

use crate::json::{
    normalise_inputs, opts_to_camel_json, py_to_serde, serde_to_py, snake_to_camel_keys,
};
use crate::sdk::PyCognee;
use crate::sdk_error::sdk_error_to_py;

// ---------------------------------------------------------------------------
// Free functions (called by the `#[pymethods]` impl below).
// ---------------------------------------------------------------------------

/// Async body for `PyCognee.remember`.
pub fn py_sdk_remember<'py>(
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
        let result = memory::run_remember(&handle, inputs_json, &dataset, &opts_json)
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for `PyCognee.remember_entry`.
pub fn py_sdk_remember_entry<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    entry: Bound<'py, PyAny>,
    dataset_name: &str,
    session_id: &str,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let entry_json = py_to_serde(&entry)?;
    let opts_json = opts_to_camel_json(opts)?;
    let dataset = dataset_name.to_owned();
    let session = session_id.to_owned();

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result =
            memory::run_remember_entry(&handle, entry_json, &dataset, &session, &opts_json)
                .await
                .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for `PyCognee.memify`.
pub fn py_sdk_memify<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_json = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = memory::run_memify_op(&handle, &opts_json)
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for `PyCognee.improve`.
pub fn py_sdk_improve<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    opts: Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let mut opts_json = py_to_serde(&opts)?;
    snake_to_camel_keys(&mut opts_json);

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = memory::run_improve(&handle, &opts_json)
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
    /// One-call add + cognify + optional self-improvement.
    ///
    /// ``inputs`` can be a single dict or a list of dicts, each with a
    /// ``type`` key:
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.remember({"type": "text", "text": "Fact A"}, "mem_ds")
    ///
    /// Supported ``opts`` keys (snake_case spellings accepted too):
    /// ``sessionId``, ``selfImprovement``, ``tenant``.
    ///
    /// Returns the ``RememberResult`` as a dict.
    #[pyo3(signature = (inputs, dataset_name, opts=None))]
    fn remember<'py>(
        &self,
        py: Python<'py>,
        inputs: Bound<'py, PyAny>,
        dataset_name: &str,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_remember(py, Arc::clone(&self.inner), inputs, dataset_name, opts)
    }

    /// Store a single typed memory entry (QA, trace, or feedback).
    ///
    /// ``entry`` is a discriminated-union dict:
    ///
    /// .. code-block:: python
    ///
    ///     await cognee.remember_entry(
    ///         {"type": "qa", "question": "What?", "answer": "This."},
    ///         "ds", "session-1",
    ///     )
    ///
    /// Supported entry types:
    ///
    /// - ``qa``: ``question``, ``answer``, ``context``, ``feedbackText``,
    ///   ``feedbackScore``, ``usedGraphElementIds`` (all optional except type).
    /// - ``trace``: ``originFunction`` (required), ``status``, ``methodParams``,
    ///   ``methodReturnValue``, ``memoryQuery``, ``memoryContext``,
    ///   ``errorMessage``, ``generateFeedbackWithLlm``.
    /// - ``feedback``: ``qaId`` (required), ``feedbackText``, ``feedbackScore``.
    ///
    /// Unknown ``type`` values raise ``CogneeValidationError``.
    ///
    /// Supported ``opts`` keys (snake_case spellings accepted too): ``tenant``.
    #[pyo3(signature = (entry, dataset_name, session_id, opts=None))]
    fn remember_entry<'py>(
        &self,
        py: Python<'py>,
        entry: Bound<'py, PyAny>,
        dataset_name: &str,
        session_id: &str,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_remember_entry(
            py,
            Arc::clone(&self.inner),
            entry,
            dataset_name,
            session_id,
            opts,
        )
    }

    /// Build triplet embeddings over the entire knowledge graph.
    ///
    /// Idempotent — safe to re-run. Needed before ``SearchType.TRIPLET_COMPLETION``
    /// will return results.
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.memify()
    ///     print(result["tripletCount"])
    ///
    /// Supported ``opts`` keys (snake_case spellings accepted too):
    /// ``tripletBatchSize``, ``nodeTypeFilter``, ``nodeNameFilter`` (list of str),
    /// ``nodeNameFilterOperator`` (``"AND"`` | ``"OR"``).
    ///
    /// Returns a dict with camelCase keys:
    /// ``tripletCount``, ``indexedCount``, ``batchCount``,
    /// ``alreadyCompleted``, ``priorPipelineRunId``.
    #[pyo3(signature = (opts=None))]
    fn memify<'py>(
        &self,
        py: Python<'py>,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_memify(py, Arc::clone(&self.inner), opts)
    }

    /// Apply graph improvement based on session feedback.
    ///
    /// ``opts`` must be a dict containing at least ``"datasetName"``
    /// (camelCase). Missing ``datasetName`` raises ``CogneeValidationError``.
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.improve({"datasetName": "my_ds"})
    ///     print(result["stagesRun"])
    ///
    /// Supported ``opts`` keys (snake_case spellings accepted too):
    /// ``datasetName`` (required), ``sessionIds`` (list of str),
    /// ``nodeName`` (list of str), ``feedbackAlpha`` (float, default 0.1),
    /// ``tenant`` (UUID str).
    ///
    /// Returns a dict with camelCase keys:
    /// ``stagesRun``, ``memifyResult``, ``feedbackEntriesProcessed``,
    /// ``feedbackEntriesApplied``, ``sessionsPersisted``, ``edgesSynced``.
    fn improve<'py>(
        &self,
        py: Python<'py>,
        opts: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_improve(py, Arc::clone(&self.inner), opts)
    }
}
