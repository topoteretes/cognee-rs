//! Python-facing wrappers for the seven dataset-management operations:
//! `list`, `list_data`, `has`, `status`, `empty`, `delete_data`, `delete_all`.
//!
//! Exposes a `PyCogneeDatasets` sub-object (accessible as `cognee.datasets`)
//! with async methods for each operation.  Each method converts Python
//! arguments to `serde_json::Value` via the shared `crate::json` helpers,
//! delegates to the shared async ops in
//! `cognee_bindings_common::ops::datasets`, and converts the result back to a
//! Python object.  The async bridge uses `pyo3_async_runtimes::tokio` (same
//! pattern as `sdk_ops.rs`, `sdk_retrieval.rs`, and `sdk_data.rs`).

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::datasets;
use pyo3::prelude::*;
use uuid::Uuid;

use crate::json::{opts_to_camel_json, py_to_serde, serde_to_py};
use crate::sdk_error::{CogneeValidationError, sdk_error_to_py};

// ---------------------------------------------------------------------------
// UUID validation helper.
// ---------------------------------------------------------------------------

/// Parse and validate a UUID string, raising `CogneeValidationError` on failure.
fn validate_uuid(s: &str, field: &str) -> PyResult<Uuid> {
    Uuid::parse_str(s).map_err(|_| {
        CogneeValidationError::new_err(format!("'{}' is not a valid UUID: {}", field, s))
    })
}

// ---------------------------------------------------------------------------
// `PyCogneeDatasets` sub-object.
// ---------------------------------------------------------------------------

/// Dataset management sub-object.
///
/// Accessible as ``cognee.datasets`` on every :class:`Cognee` instance.
///
/// .. code-block:: python
///
///     datasets = await cognee.datasets.list()
///     has = await cognee.datasets.has(dataset_id)
///     status = await cognee.datasets.status([dataset_id])
///     await cognee.datasets.empty(dataset_id)
///     await cognee.datasets.delete_all()
#[pyclass(name = "CogneeDatasets")]
pub struct PyCogneeDatasets {
    pub(crate) inner: Arc<HandleState>,
}

#[pymethods]
impl PyCogneeDatasets {
    /// List all datasets for the current owner.
    ///
    /// .. code-block:: python
    ///
    ///     datasets = await cognee.datasets.list()
    ///     for ds in datasets:
    ///         print(ds["id"], ds["name"])
    ///
    /// Returns a list of dataset dicts.
    fn list<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = datasets::list_datasets(&handle)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// List all data items in a dataset.
    ///
    /// .. code-block:: python
    ///
    ///     items = await cognee.datasets.list_data(dataset_id)
    ///
    /// ``dataset_id`` must be a valid UUID string.
    /// Returns a list of data item dicts.
    /// Raises ``CogneeValidationError`` for an invalid UUID.
    fn list_data<'py>(&self, py: Python<'py>, dataset_id: String) -> PyResult<Bound<'py, PyAny>> {
        validate_uuid(&dataset_id, "dataset_id")?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = datasets::list_data(&handle, &dataset_id)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Check whether a dataset has any data items.
    ///
    /// .. code-block:: python
    ///
    ///     has = await cognee.datasets.has(dataset_id)
    ///
    /// ``dataset_id`` must be a valid UUID string.
    /// Returns ``True`` if the dataset contains at least one data item,
    /// ``False`` otherwise (including for a non-existent dataset UUID).
    /// Raises ``CogneeValidationError`` for an invalid UUID.
    fn has<'py>(&self, py: Python<'py>, dataset_id: String) -> PyResult<Bound<'py, PyAny>> {
        validate_uuid(&dataset_id, "dataset_id")?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = datasets::has_data(&handle, &dataset_id)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Get the pipeline run status for a list of dataset UUIDs.
    ///
    /// .. code-block:: python
    ///
    ///     status = await cognee.datasets.status([dataset_id1, dataset_id2])
    ///     # {"<uuid>": "COMPLETED", "<uuid>": "INITIATED", ...}
    ///
    /// ``dataset_ids`` is a Python list of UUID strings.
    /// Returns a dict mapping each dataset UUID string to its pipeline status
    /// string (``"INITIATED"``, ``"STARTED"``, ``"COMPLETED"``, ``"ERRORED"``).
    /// Raises ``CogneeValidationError`` for invalid UUID strings.
    fn status<'py>(
        &self,
        py: Python<'py>,
        dataset_ids: Bound<'py, PyAny>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let ids_val = py_to_serde(&dataset_ids)?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = datasets::dataset_status(&handle, ids_val)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Remove all data items from a dataset and delete the dataset record.
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.datasets.empty(dataset_id)
    ///
    /// ``dataset_id`` must be a valid UUID string.
    /// Returns a delete result dict.
    /// Raises ``CogneeValidationError`` for an invalid UUID.
    fn empty<'py>(&self, py: Python<'py>, dataset_id: String) -> PyResult<Bound<'py, PyAny>> {
        validate_uuid(&dataset_id, "dataset_id")?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = datasets::empty_dataset(&handle, &dataset_id)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Delete a single data item from a dataset.
    ///
    /// .. code-block:: python
    ///
    ///     result = await cognee.datasets.delete_data(dataset_id, data_id)
    ///     result = await cognee.datasets.delete_data(
    ///         dataset_id, data_id,
    ///         {"soft_delete": True, "delete_dataset_if_empty": True},
    ///     )
    ///
    /// ``dataset_id`` and ``data_id`` must be valid UUID strings.
    /// ``opts`` is an optional dict with boolean keys (both ``snake_case`` and
    /// ``camelCase`` accepted):
    ///
    /// - ``soft_delete`` / ``softDelete`` — bool (default ``False``)
    /// - ``delete_dataset_if_empty`` / ``deleteDatasetIfEmpty`` — bool (default ``False``)
    ///
    /// Returns a delete result dict.
    /// Raises ``CogneeValidationError`` for an invalid UUID or bad opts.
    #[pyo3(signature = (dataset_id, data_id, opts=None))]
    fn delete_data<'py>(
        &self,
        py: Python<'py>,
        dataset_id: String,
        data_id: String,
        opts: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        validate_uuid(&dataset_id, "dataset_id")?;
        validate_uuid(&data_id, "data_id")?;
        let opts_val = opts_to_camel_json(opts)?;
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = datasets::delete_data(&handle, &dataset_id, &data_id, &opts_val)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }

    /// Delete all datasets for the current owner.
    ///
    /// .. code-block:: python
    ///
    ///     results = await cognee.datasets.delete_all()
    ///
    /// Returns a list of delete result dicts (one per deleted dataset).
    fn delete_all<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let result = datasets::delete_all_datasets(&handle)
                .await
                .map_err(sdk_error_to_py)?;
            Python::with_gil(|py| serde_to_py(py, &result))
        })
    }
}
