//! Python-facing wrappers for session operations:
//! `get`, `add_feedback`, `delete_feedback`, `get_graph_context`,
//! `set_graph_context`.
//!
//! Exposes a `PyCogneeSessions` sub-object (accessible as `cognee.sessions`)
//! with async methods for each operation. Each method converts Python
//! arguments to `serde_json::Value` via the shared `crate::json` helpers,
//! delegates to the shared async ops in
//! `cognee_bindings_common::ops::sessions`, and converts the result back to a
//! Python object. The async bridge uses `pyo3_async_runtimes::tokio` (same
//! pattern as the other `sdk_*.rs` modules).

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::sessions;
use pyo3::prelude::*;

use crate::json::{opts_to_camel_json, serde_to_py};
use crate::sdk_error::sdk_error_to_py;

// ---------------------------------------------------------------------------
// `PyCogneeSessions` sub-object.
// ---------------------------------------------------------------------------

/// Session management sub-object.
///
/// Accessible as ``cognee.sessions`` on every :class:`Cognee` instance.
///
/// .. code-block:: python
///
///     entries = await cognee.sessions.get("session-id")
///     await cognee.sessions.set_graph_context("session-id", "ctx string")
///     ctx = await cognee.sessions.get_graph_context("session-id")
#[pyclass(name = "CogneeSessions")]
pub struct PyCogneeSessions {
    pub(crate) inner: Arc<HandleState>,
}

#[pymethods]
impl PyCogneeSessions {
    /// Retrieve QA history entries for a session.
    ///
    /// .. code-block:: python
    ///
    ///     entries = await cognee.sessions.get("session-id")
    ///     entries = await cognee.sessions.get("session-id", {"lastN": 5})
    ///
    /// ``session_id`` is the session identifier string.
    /// ``opts`` is an optional dict with ``"lastN"`` (int, ``"last_n"``
    /// accepted too) to limit results.
    /// Returns a list of ``SessionQAEntry`` dicts (may be empty).
    #[pyo3(signature = (session_id, opts=None))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts_val = opts_to_camel_json(opts)?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = sessions::run_get_session(&handle, &session_id, &opts_val)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Add feedback to a QA entry.
    ///
    /// .. code-block:: python
    ///
    ///     ok = await cognee.sessions.add_feedback(
    ///         "session-id", "qa-id",
    ///         {"feedbackText": "Great answer!", "feedbackScore": 5},
    ///     )
    ///
    /// ``session_id`` and ``qa_id`` are string identifiers.
    /// ``opts`` is an optional dict with ``"feedbackText"`` (str) and/or
    /// ``"feedbackScore"`` (int) keys — snake_case spellings accepted too.
    /// Returns ``True`` on success, ``False`` otherwise.
    #[pyo3(signature = (session_id, qa_id, opts=None))]
    fn add_feedback<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
        qa_id: String,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let opts_val = opts_to_camel_json(opts)?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = sessions::run_add_feedback(&handle, &session_id, &qa_id, &opts_val)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Remove feedback from a QA entry.
    ///
    /// .. code-block:: python
    ///
    ///     ok = await cognee.sessions.delete_feedback("session-id", "qa-id")
    ///
    /// ``session_id`` and ``qa_id`` are string identifiers.
    /// Returns ``True`` if feedback was removed, ``False`` otherwise.
    fn delete_feedback<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
        qa_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = sessions::run_delete_feedback(&handle, &session_id, &qa_id)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Retrieve the graph context snapshot for a session.
    ///
    /// .. code-block:: python
    ///
    ///     ctx = await cognee.sessions.get_graph_context("session-id")
    ///     # Returns None if not set, or a str if previously stored.
    ///
    /// Returns ``None`` when no context has been stored, or a ``str`` otherwise.
    fn get_graph_context<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = sessions::run_get_graph_context(&handle, &session_id)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Store a graph context snapshot for a session.
    ///
    /// .. code-block:: python
    ///
    ///     await cognee.sessions.set_graph_context("session-id", "some context string")
    ///
    /// Returns ``None`` (void op).
    fn set_graph_context<'py>(
        &self,
        py: Python<'py>,
        session_id: String,
        context: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = sessions::run_set_graph_context(&handle, &session_id, &context)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }
}
