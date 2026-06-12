//! `PyCogneeConfig` — configuration surface for the Python SDK handle.
//!
//! Wraps `ConfigManager` (reached through `HandleState.cm`) and exposes it as
//! a PyO3 class named `CogneeConfig`.  An instance is pre-built inside
//! `PyCognee` and returned via the `config` property.
//!
//! # Error mapping
//!
//! `ConfigError::UnknownKey`   → `CogneeUnknownConfigKeyError`
//! `ConfigError::TypeMismatch` → `CogneeConfigTypeMismatchError`
//!
//! Both exception classes are defined and registered in `sdk_error.rs`.

use std::sync::Arc;

use cognee_bindings_common::HandleState;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use crate::json::{py_to_serde, py_to_serde_map, serde_to_py};
use crate::sdk_error::config_error_to_py;

// ── Secret-field redaction ────────────────────────────────────────────────────
//
// This list mirrors `SECRET_FIELDS` in `capi/cognee-capi/src/sdk_config.rs` and
// `js/cognee-neon/src/config.rs`.  A third copy lives here for now; hoisting it
// into `cognee-bindings-common` is tracked as a follow-up cleanup.

const SECRET_FIELDS: &[&str] = &[
    "llm_api_key",
    "embedding_api_key",
    "vector_db_key",
    "vector_db_password",
    "graph_database_key",
    "graph_database_password",
    "db_password",
    "cache_password",
    "default_user_password",
    "otel_exporter_otlp_headers",
];

const REDACTED: &str = "***REDACTED***";

fn redact_secrets(value: &mut serde_json::Value) {
    if let serde_json::Value::Object(map) = value {
        for field in SECRET_FIELDS {
            if let Some(slot) = map.get_mut(*field) {
                *slot = serde_json::Value::String(REDACTED.to_string());
            }
        }
    }
}

// ── PyCogneeConfig ────────────────────────────────────────────────────────────

/// Configuration surface for a `Cognee` handle.
///
/// Obtain via the ``config`` property on a :class:`Cognee` instance:
///
/// .. code-block:: python
///
///     cognee = Cognee()
///     cognee.config.set_str("llm_api_key", "sk-...")
///     cognee.config.set_llm_config({"llm_model": "gpt-4o", "llm_temperature": 0.0})
///     cfg = cognee.config.get()
#[pyclass(name = "CogneeConfig")]
pub struct PyCogneeConfig {
    pub(crate) inner: Arc<HandleState>,
}

#[pymethods]
impl PyCogneeConfig {
    /// Set a single configuration key to an arbitrary Python value.
    ///
    /// ``key`` is a snake_case ``Settings`` field name (e.g. ``"llm_model"``).
    /// ``value`` can be a ``str``, ``int``, ``float``, ``bool``, ``list``, or
    /// ``dict``.
    ///
    /// Raises :exc:`CogneeUnknownConfigKeyError` for unrecognised keys.
    /// Raises :exc:`CogneeConfigTypeMismatchError` when the value type does not
    /// match the field type.
    fn set(&self, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let json_value = py_to_serde(value)?;
        self.inner
            .cm
            .config()
            .set(key, json_value)
            .map_err(config_error_to_py)
    }

    /// Set a string-typed configuration key from a plain Python ``str``.
    ///
    /// Convenience wrapper around :meth:`set` — equivalent to calling
    /// ``set(key, value)`` with a string argument.
    ///
    /// Raises :exc:`CogneeUnknownConfigKeyError` for unrecognised keys.
    /// Raises :exc:`CogneeConfigTypeMismatchError` when the field is not string-typed.
    fn set_str(&self, key: &str, value: &str) -> PyResult<()> {
        self.inner
            .cm
            .config()
            .set(key, serde_json::Value::String(value.to_owned()))
            .map_err(config_error_to_py)
    }

    /// Read back the current configuration as a Python ``dict``.
    ///
    /// Secret fields (``llm_api_key``, ``embedding_api_key``, etc.) are replaced
    /// with ``"***REDACTED***"`` before being returned.
    fn get(&self, py: Python<'_>) -> PyResult<PyObject> {
        let settings = self.inner.cm.config().read().clone();
        let mut value = serde_json::to_value(&settings)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to serialize settings: {e}")))?;
        redact_secrets(&mut value);
        serde_to_py(py, &value)
    }

    /// Bulk-update LLM configuration from a Python ``dict``.
    ///
    /// ``values`` must be a ``dict`` whose keys are any subset of the LLM config
    /// field names: ``llm_provider``, ``llm_model``, ``llm_api_key``,
    /// ``llm_endpoint``, ``llm_api_version``, ``llm_temperature``,
    /// ``llm_streaming``, ``llm_max_completion_tokens``, ``llm_max_retries``,
    /// ``llm_max_parallel_requests``.
    fn set_llm_config(&self, values: &Bound<'_, PyAny>) -> PyResult<()> {
        let map = py_to_serde_map(values)?;
        self.inner
            .cm
            .config()
            .set_llm_config(&map)
            .map_err(config_error_to_py)
    }

    /// Bulk-update embedding configuration from a Python ``dict``.
    ///
    /// ``values`` must be a ``dict`` whose keys are any subset of:
    /// ``embedding_provider``, ``embedding_model``, ``embedding_dimensions``,
    /// ``embedding_endpoint``, ``embedding_api_key``, ``embedding_model_path``,
    /// ``embedding_tokenizer_path``.
    fn set_embedding_config(&self, values: &Bound<'_, PyAny>) -> PyResult<()> {
        let map = py_to_serde_map(values)?;
        self.inner
            .cm
            .config()
            .set_embedding_config(&map)
            .map_err(config_error_to_py)
    }

    /// Bulk-update vector DB configuration from a Python ``dict``.
    ///
    /// ``values`` must be a ``dict`` whose keys are any subset of:
    /// ``vector_db_provider``, ``vector_db_url``, ``vector_db_key``,
    /// ``vector_db_host``, ``vector_db_port``, ``vector_db_name``.
    fn set_vector_db_config(&self, values: &Bound<'_, PyAny>) -> PyResult<()> {
        let map = py_to_serde_map(values)?;
        self.inner
            .cm
            .config()
            .set_vector_db_config(&map)
            .map_err(config_error_to_py)
    }

    /// Bulk-update graph DB configuration from a Python ``dict``.
    ///
    /// ``values`` must be a ``dict`` whose keys are any subset of:
    /// ``graph_database_provider``, ``graph_model``, ``graph_file_path``.
    fn set_graph_db_config(&self, values: &Bound<'_, PyAny>) -> PyResult<()> {
        let map = py_to_serde_map(values)?;
        self.inner
            .cm
            .config()
            .set_graph_db_config(&map)
            .map_err(config_error_to_py)
    }
}
