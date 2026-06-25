//! Python-facing wrappers for the four data-management operations:
//! `forget`, `update`, `prune_data`, `prune_system`.
//!
//! Each function converts Python arguments to `serde_json::Value` via the
//! shared `crate::json` helpers (with `snake_case` → `camelCase` key
//! normalisation so Python callers can pass either style), delegates to the
//! shared async op in `cognee_bindings_common::ops::data`, and converts the
//! result back to a Python object.  The async bridge uses
//! `pyo3_async_runtimes::tokio` (same pattern as `sdk_ops.rs` and
//! `sdk_retrieval.rs`).
//!
//! The methods are exposed via a separate `#[pymethods]` block on `PyCognee`
//! at the bottom of this file, keeping `sdk.rs` focused on handle construction
//! and lifecycle methods.

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::data;
use pyo3::prelude::*;

use crate::json::{
    normalise_inputs, opts_to_camel_json, py_to_serde, serde_to_py, snake_to_camel_keys,
};
use crate::sdk::PyCognee;
use crate::sdk_error::sdk_error_to_py;

// ---------------------------------------------------------------------------
// Internal helpers.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Free functions (called by the `#[pymethods]` impl below).
// ---------------------------------------------------------------------------

/// Async body for `PyCognee.forget`.
///
/// `target` top-level keys are normalised from `snake_case` to `camelCase`
/// before dispatch so Python callers can use either `data_id` or `dataId`.
pub fn py_sdk_forget<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    target: Bound<'py, PyAny>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let mut target_value = py_to_serde(&target)?;
    snake_to_camel_keys(&mut target_value);
    let opts_value = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = data::forget(&handle, target_value, &opts_value)
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for `PyCognee.update`.
///
/// `new_data` may be a single input dict or a list; single dicts are wrapped
/// in an array before dispatch.
pub fn py_sdk_update<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    data_id: String,
    new_data: Bound<'py, PyAny>,
    dataset_name: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let new_data_value = normalise_inputs(&new_data)?;
    let opts_value = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = data::update(
            &handle,
            &data_id,
            new_data_value,
            &dataset_name,
            &opts_value,
        )
        .await
        .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

/// Async body for `PyCognee.prune_data`.
pub fn py_sdk_prune_data<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
) -> PyResult<Bound<'py, PyAny>> {
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        data::prune_data(&handle).await.map_err(sdk_error_to_py)?;
        Python::with_gil(|py| Ok(py.None()))
    })
}

/// Async body for `PyCognee.prune_system`.
///
/// `opts` keys are normalised from `snake_case` to `camelCase` so Python
/// callers can use either `prune_graph` or `pruneGraph`.
pub fn py_sdk_prune_system<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_value = opts_to_camel_json(opts)?;

    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = data::prune_system(&handle, &opts_value)
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
    /// Delete data from the knowledge graph.
    ///
    /// .. code-block:: python
    ///
    ///     await cognee.forget({"kind": "all"})
    ///     await cognee.forget({"kind": "dataset", "dataset": {"name": "my_ds"}})
    ///     await cognee.forget({
    ///         "kind": "item",
    ///         "data_id": "<uuid>",   # also accepted: "dataId"
    ///         "dataset": {"name": "my_ds"},
    ///     })
    ///
    /// ``target`` is a discriminated union on ``kind``:
    ///
    /// - ``{"kind": "all"}`` — delete everything for this owner
    /// - ``{"kind": "dataset", "dataset": {"name": str} | {"id": str}}`` — delete a dataset
    /// - ``{"kind": "item", "dataId": str, "dataset": ...}`` — delete one item
    ///
    /// Both ``snake_case`` and ``camelCase`` top-level keys are accepted in
    /// ``target`` (e.g. both ``data_id`` and ``dataId`` work).
    ///
    /// Returns a dict with keys ``target`` (string) and ``deleteResult`` (dict).
    /// Raises ``CogneeValidationError`` for an unknown ``kind`` value.
    #[pyo3(signature = (target, opts=None))]
    fn forget<'py>(
        &self,
        py: Python<'py>,
        target: Bound<'py, PyAny>,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_forget(py, Arc::clone(&self.inner), target, opts)
    }

    /// Replace a data item with new content and re-cognify.
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.update(
    ///         data_id,
    ///         {"type": "text", "text": "Updated content"},
    ///         "my_dataset",
    ///     )
    ///     print(result["deletedDataId"], result["newData"])
    ///
    /// Returns a dict with keys ``deletedDataId``, ``deleteResult``,
    /// ``newData``, and ``cognifyResult`` (all camelCase).
    #[pyo3(signature = (data_id, new_data, dataset_name, opts=None))]
    fn update<'py>(
        &self,
        py: Python<'py>,
        data_id: String,
        new_data: Bound<'py, PyAny>,
        dataset_name: String,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_update(
            py,
            Arc::clone(&self.inner),
            data_id,
            new_data,
            dataset_name,
            opts,
        )
    }

    /// Remove all files from data storage.
    ///
    /// .. code-block:: python
    ///
    ///     await cognee.prune_data()
    ///
    /// Returns ``None``.
    fn prune_data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_prune_data(py, Arc::clone(&self.inner))
    }

    /// Selective backend cleanup (graph, vector, session cache, optional metadata).
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.prune_system()
    ///     result = await cognee.prune_system({"prune_graph": True, "prune_vector": True})
    ///
    /// Supported ``opts`` keys (both ``snake_case`` and ``camelCase`` accepted):
    ///
    /// - ``prune_graph`` / ``pruneGraph`` — bool (default ``True``)
    /// - ``prune_vector`` / ``pruneVector`` — bool (default ``True``)
    /// - ``prune_metadata`` / ``pruneMetadata`` — bool (default ``False``, destructive)
    /// - ``prune_cache`` / ``pruneCache`` — bool (default ``True``)
    ///
    /// Returns a dict with camelCase keys: ``graphPruned``, ``vectorPruned``,
    /// ``metadataPruned``, ``cachePruned``, ``dataPruned``.
    #[pyo3(signature = (opts=None))]
    fn prune_system<'py>(
        &self,
        py: Python<'py>,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        py_sdk_prune_system(py, Arc::clone(&self.inner), opts)
    }
}
