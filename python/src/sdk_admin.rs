//! Python-facing wrappers for admin and notebook operations:
//! `reset_pipeline_run_status`, `reset_dataset_pipeline_run_status`,
//! `get_or_create_default_user` (wired directly on `PyCognee`), and
//! `list`, `create`, `update`, `delete` notebooks (via `PyCogneeNotebooks`).
//!
//! The admin ops are exposed as direct methods on `PyCognee` (not a sub-object).
//! The notebook ops are exposed via a `PyCogneeNotebooks` sub-object accessible
//! as `cognee.notebooks`.
//!
//! Each method converts Python arguments to `serde_json::Value` via the shared
//! `crate::json` helpers, delegates to the shared async ops in
//! `cognee_bindings_common::ops::admin`, and converts the result back to a
//! Python object. The async bridge uses `pyo3_async_runtimes::tokio`.

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::admin;
use pyo3::prelude::*;

use crate::json::{py_to_serde, serde_to_py};
use crate::sdk::PyCognee;
use crate::sdk_error::sdk_error_to_py;

// ---------------------------------------------------------------------------
// `PyCogneeNotebooks` sub-object.
// ---------------------------------------------------------------------------

/// Notebook management sub-object.
///
/// Accessible as ``cognee.notebooks`` on every :class:`Cognee` instance.
///
/// .. code-block:: python
///
///     notebooks = await cognee.notebooks.list()
///     nb = await cognee.notebooks.create("My Notebook")
///     await cognee.notebooks.delete(nb["id"])
#[pyclass(name = "CogneeNotebooks")]
pub struct PyCogneeNotebooks {
    pub(crate) inner: Arc<HandleState>,
}

#[pymethods]
impl PyCogneeNotebooks {
    /// List all notebooks for the current owner.
    ///
    /// .. code-block:: python
    ///
    ///     notebooks = await cognee.notebooks.list()
    ///     for nb in notebooks:
    ///         print(nb["id"], nb["name"])
    ///
    /// Returns a list of ``Notebook`` dicts.
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = admin::run_list_notebooks(&handle)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Create a new notebook.
    ///
    /// .. code-block:: python
    ///
    ///     nb = await cognee.notebooks.create("My Notebook")
    ///     nb = await cognee.notebooks.create("My Notebook", cells=[...])
    ///
    /// ``name`` is the notebook name.
    /// ``cells`` is an optional list of cell objects (defaults to empty list).
    /// ``deletable`` is always forced to ``True`` (Python lib parity).
    /// Returns the created ``Notebook`` as a dict.
    #[pyo3(signature = (name, cells=None, deletable=true))]
    fn create<'py>(
        &self,
        py: Python<'py>,
        name: String,
        cells: Option<Bound<'py, PyAny>>,
        deletable: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        // Always force deletable=true (Python lib parity).
        let _ = deletable;
        let cells_val = match cells {
            None => serde_json::Value::Array(vec![]),
            Some(c) if c.is_none() => serde_json::Value::Array(vec![]),
            Some(c) => py_to_serde(&c)?,
        };
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = admin::run_create_notebook(&handle, name, cells_val, true)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Update a notebook's name and/or cells.
    ///
    /// .. code-block:: python
    ///
    ///     nb = await cognee.notebooks.update(notebook_id, {"name": "New Name"})
    ///     nb = await cognee.notebooks.update(notebook_id, {"cells": [...]})
    ///
    /// ``id`` is the notebook UUID string.
    /// ``patch`` is a dict with optional ``"name"`` (str) and/or ``"cells"`` (list).
    /// Returns the updated ``Notebook`` as a dict, or ``None`` if not found.
    fn update<'py>(
        &self,
        py: Python<'py>,
        id: String,
        patch: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let patch_val = py_to_serde(&patch)?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = admin::run_update_notebook(&handle, &id, patch_val)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Delete a notebook by UUID.
    ///
    /// .. code-block:: python
    ///
    ///     deleted = await cognee.notebooks.delete(notebook_id)
    ///     # True if deleted, False if not found.
    ///
    /// ``id`` is the notebook UUID string.
    /// Returns ``True`` if the notebook was deleted, ``False`` if not found.
    fn delete<'py>(&self, py: Python<'py>, id: String) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = admin::run_delete_notebook(&handle, &id)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }
}

// ---------------------------------------------------------------------------
// Admin methods wired directly onto `PyCognee`.
// ---------------------------------------------------------------------------

/// Additional `#[pymethods]` block on `PyCognee` for admin ops.
#[pymethods]
impl PyCognee {
    /// Reset the pipeline run status for a specific pipeline within a dataset.
    ///
    /// ``dataset_id`` is a UUID string. ``pipeline_name`` is the pipeline name.
    /// Returns ``None`` on success.
    ///
    /// Raises ``CogneeValidationError`` if ``dataset_id`` is not a valid UUID.
    fn reset_pipeline_run_status<'py>(
        &self,
        py: Python<'py>,
        dataset_id: String,
        pipeline_name: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = admin::run_reset_pipeline_run_status(&handle, &dataset_id, &pipeline_name)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Reset all pipeline run statuses for a dataset.
    ///
    /// ``dataset_id`` is a UUID string.
    /// Returns ``None`` on success.
    ///
    /// Raises ``CogneeValidationError`` if ``dataset_id`` is not a valid UUID.
    fn reset_dataset_pipeline_run_status<'py>(
        &self,
        py: Python<'py>,
        dataset_id: String,
    ) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = admin::run_reset_dataset_pipeline_run_status(&handle, &dataset_id)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Get or create the default user account.
    ///
    /// .. code-block:: python
    ///
    ///     user = await cognee.get_or_create_default_user()
    ///     print(user["id"], user["email"])
    ///
    /// Returns a ``User`` dict with at least ``"id"`` and ``"email"`` fields.
    fn get_or_create_default_user<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = admin::run_get_or_create_default_user(&handle)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }
}
