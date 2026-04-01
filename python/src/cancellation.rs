use pyo3::prelude::*;

use cognee_core::CancellationHandle;

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
