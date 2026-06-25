use pyo3::prelude::*;
use pyo3::types::PyTuple;

use cognee_core::{CancellationHandle, CancellationToken};

#[pyclass(name = "CancellationHandle")]
pub struct PyCancellationHandle {
    pub(crate) inner: CancellationHandle,
}

#[pymethods]
impl PyCancellationHandle {
    /// Signal cancellation.
    fn cancel(&self) {
        self.inner.cancel();
    }

    /// Whether cancellation has been requested.
    #[getter]
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

/// The observe-only side of a cancellation pair.
///
/// Obtained from :func:`cancellation_pair`. Can be cloned and shared with
/// tasks that need to *observe* cancellation without holding the authority
/// to trigger it.
#[pyclass(name = "CancellationToken")]
pub struct PyCancellationToken {
    pub(crate) inner: CancellationToken,
}

#[pymethods]
impl PyCancellationToken {
    /// Whether cancellation has been requested.
    #[getter]
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Clone this token so multiple observers can share the same cancellation state.
    fn clone_token(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Create a linked (:class:`CancellationHandle`, :class:`CancellationToken`) pair.
///
/// The handle is given to the *owner* of a task; the token is passed into the
/// task itself. Call ``handle.cancel()`` to signal cancellation; observe it
/// via ``token.is_cancelled``.
///
/// Returns a 2-tuple ``(handle, token)``.
#[pyfunction]
pub fn cancellation_pair(py: Python<'_>) -> PyResult<Bound<'_, PyTuple>> {
    let (handle, token) = cognee_core::cancellation_pair();
    let py_handle = Py::new(py, PyCancellationHandle { inner: handle })?;
    let py_token = Py::new(py, PyCancellationToken { inner: token })?;
    PyTuple::new(py, [py_handle.into_any(), py_token.into_any()])
}
