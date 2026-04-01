use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use cognee_core::{
    NoopWatcher, Pipeline, RetryDelay, RetryPolicy, Task, TaskInfo, execute, execute_blocking,
    execute_in_background,
};

use crate::error::execution_error_to_pyerr;
use crate::task::make_task;
use crate::task_context::PyTaskContext;
use crate::value::{py_to_arc, results_to_py};
use crate::watcher::PyWatcherBridge;

#[pyclass(name = "Pipeline")]
pub struct PyPipeline {
    inner: Pipeline,
}

#[pymethods]
impl PyPipeline {
    #[new]
    fn new(description: String) -> Self {
        Self {
            inner: Pipeline::new(description),
        }
    }

    /// Set the pipeline name.
    fn with_name(mut slf: PyRefMut<'_, Self>, name: String) -> PyRefMut<'_, Self> {
        slf.inner.name = Some(name);
        slf
    }

    /// Add a task. The callable type is auto-detected.
    ///
    /// Args:
    ///     callable: A Python function, coroutine function, generator function,
    ///               or async generator function.
    ///     name: Optional human-readable name for the task.
    ///     batch: If True, the callable receives a list of items instead of a
    ///            single item.
    ///     batch_size: Override the pipeline-level batch_size for this task.
    ///     weight: Relative weight for progress allocation (default 1).
    #[pyo3(signature = (callable, *, name=None, batch=false, batch_size=None, weight=1))]
    fn add_task(
        mut slf: PyRefMut<'_, Self>,
        py: Python<'_>,
        callable: &Bound<'_, PyAny>,
        name: Option<String>,
        batch: bool,
        batch_size: Option<usize>,
        weight: u32,
    ) -> PyResult<()> {
        let task = make_task(py, callable, batch)?;

        let mut info = TaskInfo::new(task);
        if let Some(n) = name {
            info = info.with_name(n);
        }
        if let Some(bs) = batch_size {
            info = info.with_batch_size(bs);
        }
        info = info.with_weight(weight);

        slf.inner.tasks.push(info);
        Ok(())
    }

    /// Set a constant retry policy.
    #[pyo3(signature = (max_attempts, delay_ms))]
    fn with_retry(
        mut slf: PyRefMut<'_, Self>,
        max_attempts: u32,
        delay_ms: u64,
    ) -> PyResult<PyRefMut<'_, Self>> {
        let ma = NonZeroU32::new(max_attempts)
            .ok_or_else(|| PyRuntimeError::new_err("max_attempts must be > 0"))?;
        slf.inner.retry_policy = RetryPolicy::Limited {
            max_attempts: ma,
            delay: RetryDelay::Constant(Duration::from_millis(delay_ms)),
        };
        Ok(slf)
    }

    /// Set an exponential-backoff retry policy.
    #[pyo3(signature = (max_attempts, base_ms, factor=2))]
    fn with_retry_exponential(
        mut slf: PyRefMut<'_, Self>,
        max_attempts: u32,
        base_ms: u64,
        factor: u32,
    ) -> PyResult<PyRefMut<'_, Self>> {
        let ma = NonZeroU32::new(max_attempts)
            .ok_or_else(|| PyRuntimeError::new_err("max_attempts must be > 0"))?;
        slf.inner.retry_policy = RetryPolicy::Limited {
            max_attempts: ma,
            delay: RetryDelay::Exponential {
                base: Duration::from_millis(base_ms),
                factor,
            },
        };
        Ok(slf)
    }

    /// Set the default batch size for tasks.
    fn with_batch_size(mut slf: PyRefMut<'_, Self>, size: usize) -> PyRefMut<'_, Self> {
        slf.inner.batch_size = size;
        slf
    }

    /// Set the number of data items processed concurrently.
    fn with_concurrency(mut slf: PyRefMut<'_, Self>, n: usize) -> PyRefMut<'_, Self> {
        slf.inner.concurrency = n;
        slf
    }

    /// Execute the pipeline asynchronously. Returns an awaitable.
    #[pyo3(signature = (inputs, ctx, watcher=None))]
    fn execute<'py>(
        &self,
        py: Python<'py>,
        inputs: Vec<Bound<'py, PyAny>>,
        ctx: &PyTaskContext,
        watcher: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let rust_inputs: Vec<Arc<dyn cognee_core::Value>> =
            inputs.iter().map(|obj| py_to_arc(obj)).collect();
        let pipeline = clone_pipeline(&self.inner);
        let ctx_inner = Arc::clone(&ctx.inner);

        let watcher_arc: Arc<dyn cognee_core::PipelineWatcher> = match watcher {
            Some(w) => Arc::new(PyWatcherBridge::new(w.unbind())),
            None => Arc::new(NoopWatcher),
        };

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let results = execute(&pipeline, rust_inputs, ctx_inner, watcher_arc.as_ref())
                .await
                .map_err(execution_error_to_pyerr)?;

            Python::with_gil(|py| results_to_py(py, &results))
        })
    }

    /// Execute the pipeline synchronously (blocks the calling thread).
    ///
    /// Do NOT call this from within a running asyncio event loop — use
    /// `execute()` instead.
    #[pyo3(signature = (inputs, ctx, watcher=None))]
    fn execute_sync(
        &self,
        py: Python<'_>,
        inputs: Vec<Bound<'_, PyAny>>,
        ctx: &PyTaskContext,
        watcher: Option<Bound<'_, PyAny>>,
    ) -> PyResult<PyObject> {
        let rust_inputs: Vec<Arc<dyn cognee_core::Value>> =
            inputs.iter().map(|obj| py_to_arc(obj)).collect();
        let ctx_inner = Arc::clone(&ctx.inner);

        let watcher_box: Box<dyn cognee_core::PipelineWatcher> = match watcher {
            Some(w) => Box::new(PyWatcherBridge::new(w.unbind())),
            None => Box::new(NoopWatcher),
        };

        // Release the GIL during blocking execution so Python task closures
        // can re-acquire it when called by the executor.
        let result = py.allow_threads(|| {
            execute_blocking(&self.inner, rust_inputs, ctx_inner, watcher_box.as_ref())
        });

        let run_result = result.map_err(execution_error_to_pyerr)?;
        results_to_py(py, &run_result.outputs)
    }

    /// Execute the pipeline in the background. Returns a handle whose
    /// `.wait()` method is an awaitable that resolves to the results.
    #[pyo3(signature = (inputs, ctx, watcher=None))]
    fn execute_in_background<'py>(
        &self,
        _py: Python<'py>,
        inputs: Vec<Bound<'py, PyAny>>,
        ctx: &PyTaskContext,
        watcher: Option<Bound<'py, PyAny>>,
    ) -> PyResult<PyPipelineRunHandle> {
        let rust_inputs: Vec<Arc<dyn cognee_core::Value>> =
            inputs.iter().map(|obj| py_to_arc(obj)).collect();
        let pipeline = Arc::new(clone_pipeline(&self.inner));
        let ctx_inner = Arc::clone(&ctx.inner);

        let watcher_arc: Arc<dyn cognee_core::PipelineWatcher> = match watcher {
            Some(w) => Arc::new(PyWatcherBridge::new(w.unbind())),
            None => Arc::new(NoopWatcher),
        };

        // Use the pyo3-async-runtimes managed tokio runtime so that
        // `tokio::spawn` inside `execute_in_background` has a reactor.
        let rt = pyo3_async_runtimes::tokio::get_runtime();
        let _guard = rt.enter();
        let handle = execute_in_background(pipeline, rust_inputs, ctx_inner, watcher_arc);

        Ok(PyPipelineRunHandle {
            inner: Some(handle),
        })
    }
}

// ---------------------------------------------------------------------------
// PipelineRunHandle
// ---------------------------------------------------------------------------

#[pyclass(name = "PipelineRunHandle")]
pub struct PyPipelineRunHandle {
    inner: Option<cognee_core::PipelineRunHandle>,
}

#[pymethods]
impl PyPipelineRunHandle {
    /// Await the pipeline result. Returns a list of output values.
    fn wait<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = self
            .inner
            .take()
            .ok_or_else(|| PyRuntimeError::new_err("handle already consumed"))?;

        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let run_result = handle.wait().await.map_err(execution_error_to_pyerr)?;
            Python::with_gil(|py| results_to_py(py, &run_result.outputs))
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Clone a `Task` by matching on the enum and cloning the inner `Arc`.
fn clone_task(task: &Task) -> Task {
    match task {
        Task::Sync(f) => Task::Sync(Arc::clone(f)),
        Task::Async(f) => Task::Async(Arc::clone(f)),
        Task::SyncIter(f) => Task::SyncIter(Arc::clone(f)),
        Task::AsyncStream(f) => Task::AsyncStream(Arc::clone(f)),
        Task::SyncBatch(f) => Task::SyncBatch(Arc::clone(f)),
        Task::AsyncBatch(f) => Task::AsyncBatch(Arc::clone(f)),
        Task::SyncIterBatch(f) => Task::SyncIterBatch(Arc::clone(f)),
        Task::AsyncStreamBatch(f) => Task::AsyncStreamBatch(Arc::clone(f)),
    }
}

/// Clone a `Pipeline` struct. Tasks are `Arc`-wrapped closures, so cloning is cheap.
fn clone_pipeline(p: &Pipeline) -> Pipeline {
    Pipeline {
        id: p.id,
        name: p.name.clone(),
        description: p.description.clone(),
        tasks: p
            .tasks
            .iter()
            .map(|ti| {
                let mut info = TaskInfo::new(clone_task(&ti.task));
                if let Some(ref n) = ti.name {
                    info = info.with_name(n.clone());
                }
                if let Some(bs) = ti.batch_size {
                    info = info.with_batch_size(bs);
                }
                if let Some(ref st) = ti.summary_template {
                    info = info.with_summary(st.clone());
                }
                info = info.with_weight(ti.weight);
                info
            })
            .collect(),
        retry_policy: p.retry_policy.clone(),
        batch_size: p.batch_size,
        data_id_fn: p.data_id_fn.clone(),
        concurrency: p.concurrency,
    }
}
