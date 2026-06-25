use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyBool;

use cognee_core::Value;

/// A Python object wrapped as a cognee-core `Value`.
///
/// `Py<PyAny>` is `Send + Sync + 'static` by design in PyO3, so the
/// blanket `impl Value for T where T: Any + Send + Sync + 'static`
/// applies automatically.
pub struct PyValue {
    pub inner: Py<PyAny>,
}

/// Wrap a Python object as `Arc<dyn Value>` for passing into the pipeline.
pub fn py_to_arc(obj: &Bound<'_, PyAny>) -> Arc<dyn Value> {
    Arc::new(PyValue {
        inner: obj.clone().unbind(),
    })
}

/// Convert an `Arc<dyn Value>` back to a Python object.
///
/// If the value is a `PyValue` (originated from Python), unwrap it directly.
/// Otherwise, attempt to convert common Rust types to Python equivalents.
pub fn arc_to_py(py: Python<'_>, val: &Arc<dyn Value>) -> PyResult<PyObject> {
    let any = (**val).as_any();

    // Fast path: unwrap PyValue back to the original Python object.
    if let Some(pv) = any.downcast_ref::<PyValue>() {
        return Ok(pv.inner.clone_ref(py));
    }

    // Fallback: convert common Rust primitives to Python objects.
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
        // bool::into_pyobject returns Borrowed, so use PyBool::new + to_owned.
        return Ok(PyBool::new(py, v).to_owned().into_any().unbind());
    }
    if let Some(v) = any.downcast_ref::<String>() {
        return Ok(v.into_pyobject(py)?.into_any().unbind());
    }
    if let Some(v) = any.downcast_ref::<Vec<u8>>() {
        return Ok(v.as_slice().into_pyobject(py)?.into_any().unbind());
    }

    // If we cannot convert, return None.
    Ok(py.None())
}

/// Convert a `Vec<Arc<dyn Value>>` to a Python list.
pub fn results_to_py(py: Python<'_>, values: &[Arc<dyn Value>]) -> PyResult<PyObject> {
    let items: Vec<PyObject> = values
        .iter()
        .map(|v| arc_to_py(py, v))
        .collect::<PyResult<_>>()?;
    Ok(pyo3::types::PyList::new(py, &items)?.into_any().unbind())
}
