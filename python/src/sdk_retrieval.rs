//! Python-facing wrappers for the two retrieval operations:
//! `search` and `recall`.
//!
//! Each function converts Python arguments to `serde_json::Value` via the
//! shared `crate::json` helpers (with `snake_case` → `camelCase` key
//! normalisation so Python callers can pass either style), delegates to the
//! shared async op in `cognee_bindings_common::ops::retrieval`, and converts
//! the result back to a Python object.  The async bridge uses
//! `pyo3_async_runtimes::tokio` (same pattern as `sdk_ops.rs`).
//!
//! The methods are exposed via a separate `#[pymethods]` block on `PyCognee`
//! at the bottom of this file, keeping `sdk.rs` focused on handle construction
//! and lifecycle methods.

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::retrieval;
use pyo3::prelude::*;

use crate::json::{opts_to_camel_json, serde_to_py};
use crate::sdk::PyCognee;
use crate::sdk_error::sdk_error_to_py;

// ---------------------------------------------------------------------------
// Free functions (called by the `#[pymethods]` impl below).
// ---------------------------------------------------------------------------

/// Async body for `PyCognee.search`.
///
/// `opts` keys are normalised from `snake_case` to `camelCase` before being
/// passed to the shared op (so both `search_type` and `searchType` work).
pub fn py_sdk_search<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    query: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_json = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = retrieval::search(&handle, &query, &opts_json)
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for `PyCognee.recall`.
///
/// `opts` keys are normalised from `snake_case` to `camelCase` before being
/// passed to the shared op.
pub fn py_sdk_recall<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    query: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_json = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = retrieval::recall(&handle, &query, &opts_json)
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
    /// Query the knowledge graph.
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.search("What is X?")
    ///     result = await cognee.search("What is X?", {"search_type": "CHUNKS", "top_k": 5})
    ///
    /// Returns a list or dict matching the ``SearchResponse`` shape.
    ///
    /// Supported ``opts`` keys (both ``snake_case`` and ``camelCase`` accepted):
    ///
    /// - ``search_type`` / ``searchType`` — one of the 15 search strategy strings
    ///   (default ``"GRAPH_COMPLETION"``).  An unknown value raises
    ///   ``CogneeValidationError``.
    /// - ``datasets`` — list of dataset name strings to restrict the search
    /// - ``dataset_ids`` / ``datasetIds`` — list of dataset UUID strings
    /// - ``top_k`` / ``topK`` — integer result limit
    /// - ``system_prompt`` / ``systemPrompt`` — override system prompt string
    /// - ``session_id`` / ``sessionId`` — session UUID string
    /// - ``node_type`` / ``nodeType`` — filter by node type string
    /// - ``node_name`` / ``nodeName`` — list of node name strings to filter
    /// - ``only_context`` / ``onlyContext`` — bool; return raw context only
    /// - ``use_combined_context`` / ``useCombinedContext`` — bool
    /// - ``verbose`` — bool
    /// - ``save_interaction`` / ``saveInteraction`` — bool (default ``True``)
    /// - ``auto_feedback_detection`` / ``autoFeedbackDetection`` — bool
    ///
    /// ``userId`` / ``user_id`` in opts is ignored; the owner is always taken
    /// from the handle so dataset-name resolution works correctly.
    #[pyo3(signature = (query, opts=None))]
    fn search<'py>(
        &self,
        py: Python<'py>,
        query: String,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_search(py, Arc::clone(&self.inner), query, opts)
    }

    /// Recall from memory using the session-aware routing pipeline.
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.recall("What did we discuss about X?")
    ///     print(result["items"], result["autoRouted"])
    ///
    /// Returns a dict with keys:
    ///
    /// - ``items`` — list of recall result objects
    /// - ``searchTypeUsed`` — ``None`` or the SCREAMING_SNAKE_CASE search type string
    /// - ``autoRouted`` — bool indicating whether auto-routing was used
    /// - ``searchResponse`` — ``None`` or the raw ``SearchResponse`` value
    ///
    /// Supported ``opts`` keys (both ``snake_case`` and ``camelCase`` accepted):
    ///
    /// - ``search_type`` / ``searchType`` — force a specific search type
    /// - ``datasets`` — list of dataset name strings
    /// - ``top_k`` / ``topK`` — integer (default ``10``)
    /// - ``auto_route`` / ``autoRoute`` — bool (default ``False``)
    /// - ``session_id`` / ``sessionId`` — session UUID string
    /// - ``scope`` — ``str`` or ``list[str]``:
    ///   ``"auto"`` | ``"graph"`` | ``"session"`` | ``"trace"`` | ``"graph_context"``
    #[pyo3(signature = (query, opts=None))]
    fn recall<'py>(
        &self,
        py: Python<'py>,
        query: String,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_recall(py, Arc::clone(&self.inner), query, opts)
    }
}
