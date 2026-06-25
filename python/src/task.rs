use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyList};

use cognee_core::{Task, TaskContext, TaskError, Value, ValueIter, ValueStream};

use crate::value::{PyValue, arc_to_py, py_to_arc};

/// Detected Python callable kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyCallableKind {
    SyncFunction,
    AsyncFunction,
    Generator,
    AsyncGenerator,
}

/// Detect the kind of a Python callable using the `inspect` module.
///
/// Order matters: check async generator before coroutine function because
/// async generators also pass `iscoroutinefunction` in some contexts.
pub fn detect_callable_kind(
    py: Python<'_>,
    callable: &Bound<'_, PyAny>,
) -> PyResult<PyCallableKind> {
    let inspect = py.import("inspect")?;

    if inspect
        .call_method1("isasyncgenfunction", (callable,))?
        .is_truthy()?
    {
        return Ok(PyCallableKind::AsyncGenerator);
    }
    if inspect
        .call_method1("iscoroutinefunction", (callable,))?
        .is_truthy()?
    {
        return Ok(PyCallableKind::AsyncFunction);
    }
    if inspect
        .call_method1("isgeneratorfunction", (callable,))?
        .is_truthy()?
    {
        return Ok(PyCallableKind::Generator);
    }
    Ok(PyCallableKind::SyncFunction)
}

/// Create the appropriate `Task` variant from a Python callable and flags.
pub fn make_task(py: Python<'_>, callable: &Bound<'_, PyAny>, batch: bool) -> PyResult<Task> {
    let kind = detect_callable_kind(py, callable)?;
    // Wrap in Arc so we can share across closures without needing the GIL to clone.
    let py_callable = Arc::new(callable.clone().unbind());

    Ok(match (kind, batch) {
        (PyCallableKind::SyncFunction, false) => make_sync_task(py_callable),
        (PyCallableKind::SyncFunction, true) => make_sync_batch_task(py_callable),
        (PyCallableKind::AsyncFunction, false) => make_async_task(py_callable),
        (PyCallableKind::AsyncFunction, true) => make_async_batch_task(py_callable),
        (PyCallableKind::Generator, false) => make_sync_iter_task(py_callable),
        (PyCallableKind::Generator, true) => make_sync_iter_batch_task(py_callable),
        (PyCallableKind::AsyncGenerator, false) => make_async_stream_task(py_callable),
        (PyCallableKind::AsyncGenerator, true) => make_async_stream_batch_task(py_callable),
    })
}

// ---------------------------------------------------------------------------
// Single-value task wrappers
// ---------------------------------------------------------------------------

fn make_sync_task(callable: Arc<Py<PyAny>>) -> Task {
    Task::sync(move |input: Arc<dyn Value>, _ctx: Arc<TaskContext>| {
        Python::with_gil(|py| {
            let py_input = arc_to_py(py, &input)?;
            let result = callable.call1(py, (py_input,))?;
            Ok(py_to_arc(result.bind(py)))
        })
        .map_err(|e: PyErr| -> TaskError { Box::new(e) })
    })
}

fn make_async_task(callable: Arc<Py<PyAny>>) -> Task {
    Task::async_fn(move |input: Arc<dyn Value>, _ctx: Arc<TaskContext>| {
        let callable = Arc::clone(&callable);
        Box::pin(async move {
            let future = Python::with_gil(|py| {
                let py_input = arc_to_py(py, &input)?;
                let coro = callable.call1(py, (py_input,))?;
                pyo3_async_runtimes::tokio::into_future(coro.bind(py).clone())
            })
            .map_err(|e: PyErr| -> TaskError { Box::new(e) })?;

            let result = future
                .await
                .map_err(|e: PyErr| -> TaskError { Box::new(e) })?;

            Python::with_gil(|py| Ok(py_to_arc(result.bind(py))))
        })
    })
}

fn make_sync_iter_task(callable: Arc<Py<PyAny>>) -> Task {
    Task::sync_iter(move |input: Arc<dyn Value>, _ctx: Arc<TaskContext>| {
        Python::with_gil(|py| {
            let py_input = arc_to_py(py, &input)?;
            let generator = callable.call1(py, (py_input,))?;
            // Eagerly collect the generator to avoid holding the GIL across
            // iterator boundaries (the executor may consume on another thread).
            let mut items: Vec<Box<dyn Value>> = Vec::new();
            let iter = generator.bind(py).try_iter()?;
            for item in iter {
                let item = item?;
                items.push(Box::new(PyValue {
                    inner: item.clone().unbind(),
                }));
            }
            Ok(Box::new(items.into_iter()) as ValueIter)
        })
        .map_err(|e: PyErr| -> TaskError { Box::new(e) })
    })
}

fn make_async_stream_task(callable: Arc<Py<PyAny>>) -> Task {
    Task::async_stream(move |input: Arc<dyn Value>, _ctx: Arc<TaskContext>| {
        let callable = Arc::clone(&callable);

        // Call the async generator function to get the async iterator object.
        let agen = Python::with_gil(|py| {
            let py_input = arc_to_py(py, &input)?;
            callable.call1(py, (py_input,))
        })
        .map_err(|e: PyErr| -> TaskError { Box::new(e) })?;

        // Use `unfold` to lazily drive __anext__ inline (same tokio task),
        // which preserves the Python event loop context needed by `into_future`.
        let stream = futures::stream::unfold(agen, |agen| async move {
            drive_async_gen_next(&agen).await.map(|item| (item, agen))
        });

        Ok(Box::pin(stream) as ValueStream)
    })
}

// ---------------------------------------------------------------------------
// Batch task wrappers
// ---------------------------------------------------------------------------

fn make_sync_batch_task(callable: Arc<Py<PyAny>>) -> Task {
    Task::sync_batch(move |items: &[Box<dyn Value>], _ctx: Arc<TaskContext>| {
        Python::with_gil(|py| {
            let py_list = items_to_py_list(py, items)?;
            let result = callable.call1(py, (py_list,))?;
            Ok(py_to_arc(result.bind(py)))
        })
        .map_err(|e: PyErr| -> TaskError { Box::new(e) })
    })
}

fn make_async_batch_task(callable: Arc<Py<PyAny>>) -> Task {
    Task::async_batch(move |items: &[Box<dyn Value>], _ctx: Arc<TaskContext>| {
        let callable = Arc::clone(&callable);
        // Clone items into owned PyObjects so we can move into the future.
        let owned_items: Vec<PyObject> = Python::with_gil(|py| {
            items
                .iter()
                .map(|v| item_to_py(py, v.as_ref()))
                .collect::<PyResult<Vec<_>>>()
        })
        .unwrap_or_default();

        Box::pin(async move {
            let future = Python::with_gil(|py| {
                let py_list = PyList::new(py, &owned_items)?;
                let coro = callable.call1(py, (py_list,))?;
                pyo3_async_runtimes::tokio::into_future(coro.bind(py).clone())
            })
            .map_err(|e: PyErr| -> TaskError { Box::new(e) })?;

            let result = future
                .await
                .map_err(|e: PyErr| -> TaskError { Box::new(e) })?;

            Python::with_gil(|py| Ok(py_to_arc(result.bind(py))))
        })
    })
}

fn make_sync_iter_batch_task(callable: Arc<Py<PyAny>>) -> Task {
    Task::sync_iter_batch(move |items: &[Box<dyn Value>], _ctx: Arc<TaskContext>| {
        Python::with_gil(|py| {
            let py_list = items_to_py_list(py, items)?;
            let generator = callable.call1(py, (py_list,))?;
            let mut collected: Vec<Box<dyn Value>> = Vec::new();
            let iter = generator.bind(py).try_iter()?;
            for item in iter {
                let item = item?;
                collected.push(Box::new(PyValue {
                    inner: item.clone().unbind(),
                }));
            }
            Ok(Box::new(collected.into_iter()) as ValueIter)
        })
        .map_err(|e: PyErr| -> TaskError { Box::new(e) })
    })
}

fn make_async_stream_batch_task(callable: Arc<Py<PyAny>>) -> Task {
    Task::async_stream_batch(move |items: &[Box<dyn Value>], _ctx: Arc<TaskContext>| {
        let callable = Arc::clone(&callable);
        let owned_items: Vec<PyObject> = Python::with_gil(|py| {
            items
                .iter()
                .map(|v| item_to_py(py, v.as_ref()))
                .collect::<PyResult<Vec<_>>>()
        })
        .unwrap_or_default();

        let agen = Python::with_gil(|py| {
            let py_list = PyList::new(py, &owned_items)?;
            callable.call1(py, (py_list,))
        })
        .map_err(|e: PyErr| -> TaskError { Box::new(e) })?;

        let stream = futures::stream::unfold(agen, |agen| async move {
            drive_async_gen_next(&agen).await.map(|item| (item, agen))
        });

        Ok(Box::pin(stream) as ValueStream)
    })
}

// ---------------------------------------------------------------------------
// Async generator helper
// ---------------------------------------------------------------------------

/// Drive one `__anext__` call on a Python async generator.
///
/// Returns `Some(item)` on success, `None` on `StopAsyncIteration` (end of
/// stream), and `None` on other errors (silently dropped for now).
async fn drive_async_gen_next(agen: &Py<PyAny>) -> Option<Box<dyn Value>> {
    let next_future = Python::with_gil(|py| {
        let coro = agen.call_method0(py, "__anext__").ok()?;
        pyo3_async_runtimes::tokio::into_future(coro.bind(py).clone()).ok()
    })?;

    match next_future.await {
        Ok(val) => {
            let boxed: Box<dyn Value> = Python::with_gil(|py| {
                Box::new(PyValue {
                    inner: val.clone_ref(py),
                })
            });
            Some(boxed)
        }
        Err(e) => {
            // StopAsyncIteration signals end of stream; other errors stop silently.
            let _is_stop = Python::with_gil(|py| {
                e.is_instance_of::<pyo3::exceptions::PyStopAsyncIteration>(py)
            });
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a `&[Box<dyn Value>]` batch into a Python list.
fn items_to_py_list<'py>(
    py: Python<'py>,
    items: &[Box<dyn Value>],
) -> PyResult<Bound<'py, PyList>> {
    let py_items: Vec<PyObject> = items
        .iter()
        .map(|v| item_to_py(py, v.as_ref()))
        .collect::<PyResult<_>>()?;
    PyList::new(py, &py_items)
}

/// Convert a single `&dyn Value` to a PyObject.
fn item_to_py(py: Python<'_>, val: &dyn Value) -> PyResult<PyObject> {
    let any = val.as_any();
    if let Some(pv) = any.downcast_ref::<PyValue>() {
        return Ok(pv.inner.clone_ref(py));
    }
    if let Some(&v) = any.downcast_ref::<i64>() {
        return Ok(v.into_pyobject(py)?.into_any().unbind());
    }
    if let Some(&v) = any.downcast_ref::<i32>() {
        return Ok(v.into_pyobject(py)?.into_any().unbind());
    }
    if let Some(&v) = any.downcast_ref::<f64>() {
        return Ok(v.into_pyobject(py)?.into_any().unbind());
    }
    if let Some(&v) = any.downcast_ref::<bool>() {
        return Ok(PyBool::new(py, v).to_owned().into_any().unbind());
    }
    if let Some(v) = any.downcast_ref::<String>() {
        return Ok(v.into_pyobject(py)?.into_any().unbind());
    }
    Ok(py.None())
}
