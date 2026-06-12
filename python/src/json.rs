//! Python ↔ `serde_json::Value` conversion helpers for the PyO3 binding.
//!
//! Every Python ↔ JSON value conversion in the binding layer must go through
//! these helpers — **no private copies** should exist in individual modules.
//!
//! ## Design
//!
//! All conversions walk the Python object tree directly using
//! `pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString}`.
//! This is faster and more correct than a `JSON.stringify` round-trip and
//! avoids pulling in the `pythonize` crate.

use std::collections::HashMap;

use base64::Engine as _;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyByteArray, PyBytes, PyDict, PyFloat, PyInt, PyList, PyString};

/// Maximum container nesting accepted by [`py_to_serde`].
///
/// Matches `serde_json`'s default recursion limit. Without a cap, a
/// self-referential container (`d = {}; d["x"] = d`) or a pathologically deep
/// list overflows the Rust stack and aborts the whole Python process.
const MAX_DEPTH: usize = 128;

/// Convert any Python primitive to a `serde_json::Value`.
///
/// Supported types: `str`, `int`, `float`, `bool`, `bytes`/`bytearray`
/// (encoded as a base64 string, matching the wire format used by the other
/// bindings), `list`, `dict`, `None`. Returns `PyTypeError` for any other
/// Python type and `PyValueError` for values nested deeper than 128 levels
/// (including reference cycles).
pub(crate) fn py_to_serde(val: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    py_to_serde_inner(val, 0)
}

fn py_to_serde_inner(val: &Bound<'_, PyAny>, depth: usize) -> PyResult<serde_json::Value> {
    if depth > MAX_DEPTH {
        return Err(PyValueError::new_err(
            "value is nested too deeply for JSON conversion (or contains a reference cycle)",
        ));
    }

    if val.is_none() {
        return Ok(serde_json::Value::Null);
    }

    // bool must be checked before int because `bool` is a subclass of `int` in Python.
    if val.is_instance_of::<PyBool>() {
        let b: bool = val.extract()?;
        return Ok(serde_json::Value::Bool(b));
    }

    if val.is_instance_of::<PyInt>() {
        // i64 first, then u64 so ints in (i64::MAX, u64::MAX] survive instead
        // of surfacing a raw OverflowError; serde_json numbers support both.
        if let Ok(n) = val.extract::<i64>() {
            return Ok(serde_json::Value::Number(n.into()));
        }
        if let Ok(n) = val.extract::<u64>() {
            return Ok(serde_json::Value::Number(n.into()));
        }
        return Err(PyValueError::new_err(
            "integer is out of the range representable in JSON (i64/u64)",
        ));
    }

    if val.is_instance_of::<PyFloat>() {
        let f: f64 = val.extract()?;
        let num = serde_json::Number::from_f64(f).ok_or_else(|| {
            PyValueError::new_err(format!(
                "float value {f} cannot be represented as JSON number"
            ))
        })?;
        return Ok(serde_json::Value::Number(num));
    }

    if val.is_instance_of::<PyString>() {
        let s: String = val.extract()?;
        return Ok(serde_json::Value::String(s));
    }

    // bytes/bytearray → base64 string: JSON has no binary type, and the shared
    // wire layer (`wire::marshal_bytes`) expects base64-encoded payloads.
    if let Ok(b) = val.downcast::<PyBytes>() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b.as_bytes());
        return Ok(serde_json::Value::String(encoded));
    }
    if let Ok(b) = val.downcast::<PyByteArray>() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b.to_vec());
        return Ok(serde_json::Value::String(encoded));
    }

    if let Ok(list) = val.downcast::<PyList>() {
        let mut arr = Vec::with_capacity(list.len());
        for item in list.iter() {
            arr.push(py_to_serde_inner(&item, depth + 1)?);
        }
        return Ok(serde_json::Value::Array(arr));
    }

    if let Ok(dict) = val.downcast::<PyDict>() {
        let mut map = serde_json::Map::new();
        for (k, v) in dict.iter() {
            let key: String = k.extract().map_err(|_| {
                PyTypeError::new_err("dict keys must be strings for JSON conversion")
            })?;
            map.insert(key, py_to_serde_inner(&v, depth + 1)?);
        }
        return Ok(serde_json::Value::Object(map));
    }

    let type_name = val
        .get_type()
        .qualname()
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    Err(PyTypeError::new_err(format!(
        "cannot convert Python type '{type_name}' to a JSON value"
    )))
}

/// Convert a `serde_json::Value` to a Python object.
///
/// - `Null`   → `None`
/// - `Bool`   → `bool`
/// - `Number` → `int` (when representable as i64) or `float`
/// - `String` → `str`
/// - `Array`  → `list`
/// - `Object` → `dict`
pub(crate) fn serde_to_py(py: Python<'_>, val: &serde_json::Value) -> PyResult<PyObject> {
    match val {
        serde_json::Value::Null => Ok(py.None()),
        serde_json::Value::Bool(b) => {
            let obj = b.into_pyobject(py)?;
            Ok(Bound::clone(&obj).into_any().unbind())
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)?.into_any().unbind())
            } else if let Some(f) = n.as_f64() {
                Ok(f.into_pyobject(py)?.into_any().unbind())
            } else {
                Err(PyValueError::new_err(format!(
                    "JSON number {n} cannot be represented as a Python int or float"
                )))
            }
        }
        serde_json::Value::String(s) => Ok(s.into_pyobject(py)?.into_any().unbind()),
        serde_json::Value::Array(arr) => {
            let list = PyList::empty(py);
            for item in arr {
                list.append(serde_to_py(py, item)?)?;
            }
            Ok(list.into_any().unbind())
        }
        serde_json::Value::Object(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                dict.set_item(k, serde_to_py(py, v)?)?;
            }
            Ok(dict.into_any().unbind())
        }
    }
}

/// Convert a Python dict (or any Python value that is a JSON object) to a
/// `HashMap<String, serde_json::Value>`.
///
/// Returns `PyValueError` if the result is not a JSON object.
pub(crate) fn py_to_serde_map(
    val: &Bound<'_, PyAny>,
) -> PyResult<HashMap<String, serde_json::Value>> {
    let serde_val = py_to_serde(val)?;
    match serde_val {
        serde_json::Value::Object(map) => Ok(map.into_iter().collect()),
        _ => Err(PyValueError::new_err(
            "expected a Python dict (JSON object) for bulk config setter",
        )),
    }
}
