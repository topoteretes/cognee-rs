use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use cognee_core::ProgressToken;

#[pyclass(name = "ProgressToken")]
#[derive(Clone)]
pub struct PyProgressToken {
    pub(crate) inner: ProgressToken,
}

#[pymethods]
impl PyProgressToken {
    #[new]
    fn new() -> Self {
        Self {
            inner: ProgressToken::new(),
        }
    }

    /// Set this token's progress fraction (clamped to [0.0, 1.0]).
    fn set(&self, fraction: f64) {
        self.inner.set(fraction);
    }

    /// This token's progress fraction in [0.0, 1.0].
    #[getter]
    fn fraction(&self) -> f64 {
        self.inner.fraction()
    }

    /// Overall progress across the entire tree.
    #[getter]
    fn root_fraction(&self) -> f64 {
        self.inner.root_fraction()
    }

    /// Whether this token's progress is >= 1.0.
    #[getter]
    fn is_complete(&self) -> bool {
        self.inner.is_complete()
    }

    /// Split into subtokens by relative weights.
    fn split(&self, weights: Vec<u32>) -> PyResult<Vec<PyProgressToken>> {
        self.inner
            .split(&weights)
            .map(|tokens| {
                tokens
                    .into_iter()
                    .map(|t| PyProgressToken { inner: t })
                    .collect()
            })
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }
}
