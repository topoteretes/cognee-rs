use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use cognee_core::thread_pool::RayonThreadPool;
use cognee_core::{CancellationHandle, TaskContext, TaskContextBuilder};

use crate::cancellation::PyCancellationHandle;
use crate::progress::PyProgressToken;

#[pyclass(name = "TaskContext")]
pub struct PyTaskContext {
    pub(crate) inner: Arc<TaskContext>,
    cancellation_handle: CancellationHandle,
}

#[pymethods]
impl PyTaskContext {
    /// Create a mock context with in-memory stubs.
    ///
    /// Suitable for pipelines where tasks are pure Python callables that
    /// do not use the Rust database / graph / vector backends.
    #[staticmethod]
    fn mock() -> PyResult<Self> {
        let pool = RayonThreadPool::with_default_threads()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        let (handle, ctx) = TaskContextBuilder::new()
            .thread_pool(Arc::new(pool))
            .database(Arc::new(cognee_database::MockDatabase::new()))
            .graph_db(Arc::new(cognee_graph::MockGraphDB::new()))
            .vector_db(Arc::new(cognee_vector::MockVectorDB::new()))
            .build()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

        Ok(PyTaskContext {
            inner: Arc::new(ctx),
            cancellation_handle: handle,
        })
    }

    /// Get the cancellation handle for this context.
    #[getter]
    fn cancellation_handle(&self) -> PyCancellationHandle {
        PyCancellationHandle {
            inner: self.cancellation_handle.clone(),
        }
    }

    /// Get the progress token for this context.
    #[getter]
    fn progress(&self) -> PyProgressToken {
        PyProgressToken {
            inner: self.inner.progress.clone(),
        }
    }
}
