//! Python-facing wrappers for admin operations:
//! `reset_pipeline_run_status`, `reset_dataset_pipeline_run_status` and
//! `get_or_create_default_user`, exposed as direct methods on `PyCognee`
//! (not a sub-object).
//!
//! Each method converts Python arguments to `serde_json::Value` via the shared
//! `crate::json` helpers, delegates to the shared async ops in
//! `cognee_bindings_common::ops::admin`, and converts the result back to a
//! Python object. The async bridge uses `pyo3_async_runtimes::tokio`.

use std::sync::Arc;

use cognee_bindings_common::ops::admin;
use pyo3::prelude::*;

use crate::json::serde_to_py;
use crate::sdk::PyCognee;
use crate::sdk_error::sdk_error_to_py;

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
            crate::gil::attach_or_err(|py| serde_to_py(py, &result))
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
            crate::gil::attach_or_err(|py| serde_to_py(py, &result))
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
            crate::gil::attach_or_err(|py| serde_to_py(py, &result))
        })
    }
}
