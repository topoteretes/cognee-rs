//! `PyCognee` — the PyO3 SDK handle, entry point for all SDK-tier operations.
//!
//! Wraps `Arc<HandleState>` from `cognee-bindings-common`.  The constructor
//! applies the **3-way overlay** (`defaults < env < JSON object`), mirroring
//! `apply_settings_json_patch` from `capi/cognee-capi/src/sdk.rs`.
//!
//! `inner` is `pub(crate)` so sibling modules (add, cognify, search, …) can
//! call the shared op functions on `HandleState` without going through the
//! Python object.

use std::sync::Arc;

use cognee::config::ConfigManager;
use cognee_bindings_common::HandleState;
use pyo3::prelude::*;

use crate::config::PyCogneeConfig;
use crate::sdk_admin::PyCogneeNotebooks;
use crate::sdk_datasets::PyCogneeDatasets;
use crate::sdk_error::{sdk_error_to_py, validation_err};
use crate::sdk_sessions::PyCogneeSessions;

// ── Settings overlay helper ───────────────────────────────────────────────────

/// Apply a JSON object patch on top of `base` settings.
///
/// Delegates every key to `ConfigManager::set(key, value)`, which handles all
/// known `Settings` fields with type checking. Unknown keys are silently
/// ignored for forward-compatibility.
///
/// Mirrors `apply_settings_json_patch` in `capi/cognee-capi/src/sdk.rs`.
fn apply_settings_json_patch(
    base: cognee::config::Settings,
    json: &str,
) -> Result<cognee::config::Settings, String> {
    let patch: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("settings_json parse error: {e}"))?;

    let obj = patch
        .as_object()
        .ok_or_else(|| "settings_json must be a JSON object".to_string())?;

    // Wrap the base settings in a temporary ConfigManager so we can use the
    // generic `set(key, value)` dispatcher for all known keys.
    let cm = ConfigManager::new(base);
    for (key, value) in obj {
        // Unknown keys are silently ignored (forward-compatibility). Type
        // mismatches are reported as errors since they indicate caller bugs.
        match cm.set(key, value.clone()) {
            Ok(()) => {}
            Err(cognee::config::ConfigError::UnknownKey(_)) => {
                // Silently skip unrecognised keys — new fields added to Settings
                // in future versions will not break older JSON overlays.
            }
            Err(e) => {
                return Err(format!("settings_json key '{key}': {e}"));
            }
        }
    }

    Ok(cm.read().clone())
}

// ── PyCognee ──────────────────────────────────────────────────────────────────

/// SDK handle. Entry point for all SDK-tier operations.
///
/// ``Cognee()`` with no arguments reads configuration from the environment
/// (defaults overlaid by env vars). Pass a JSON object string to override
/// specific settings on top of the env-derived defaults:
///
/// .. code-block:: python
///
///     cognee = Cognee('{"llm_model": "gpt-4o", "embedding_provider": "openai"}')
///     await cognee.warm()
#[pyclass(name = "Cognee")]
pub struct PyCognee {
    pub(crate) inner: Arc<HandleState>,
    /// Pre-built config handle that shares `inner` — returned by the `config` property.
    config: Py<PyCogneeConfig>,
    /// Pre-built datasets handle that shares `inner` — returned by the `datasets` property.
    datasets: Py<PyCogneeDatasets>,
    /// Pre-built sessions handle that shares `inner` — returned by the `sessions` property.
    sessions: Py<PyCogneeSessions>,
    /// Pre-built notebooks handle that shares `inner` — returned by the `notebooks` property.
    notebooks: Py<PyCogneeNotebooks>,
}

#[pymethods]
impl PyCognee {
    /// Create a new SDK handle.
    ///
    /// ``settings`` is an optional JSON object string whose keys (snake_case
    /// ``Settings`` field names) override the env-derived defaults.  Pass
    /// ``None`` or omit the argument to use environment defaults only.
    #[new]
    #[pyo3(signature = (settings=None))]
    fn new(py: Python<'_>, settings: Option<&str>) -> PyResult<Self> {
        // 3-way overlay: defaults < env < JSON object.
        let base = ConfigManager::from_env().read().clone();
        let resolved = match settings {
            None => base,
            Some(json) => apply_settings_json_patch(base, json).map_err(validation_err)?,
        };
        let inner = Arc::new(HandleState::from_settings(resolved));
        let config = Py::new(
            py,
            PyCogneeConfig {
                inner: Arc::clone(&inner),
            },
        )?;
        let datasets = Py::new(
            py,
            PyCogneeDatasets {
                inner: Arc::clone(&inner),
            },
        )?;
        let sessions = Py::new(
            py,
            PyCogneeSessions {
                inner: Arc::clone(&inner),
            },
        )?;
        let notebooks = Py::new(
            py,
            PyCogneeNotebooks {
                inner: Arc::clone(&inner),
            },
        )?;
        Ok(Self {
            inner,
            config,
            datasets,
            sessions,
            notebooks,
        })
    }

    /// The configuration surface for this handle.
    ///
    /// Use this to set or read back configuration keys:
    ///
    /// .. code-block:: python
    ///
    ///     cognee.config.set_str("llm_api_key", "sk-...")
    ///     cfg = cognee.config.get()
    #[getter]
    fn config(&self, py: Python<'_>) -> Py<PyCogneeConfig> {
        self.config.clone_ref(py)
    }

    /// The dataset management surface for this handle.
    ///
    /// Use this to list, inspect, and delete datasets and their data:
    ///
    /// .. code-block:: python
    ///
    ///     datasets = await cognee.datasets.list()
    ///     has = await cognee.datasets.has(dataset_id)
    ///     await cognee.datasets.empty(dataset_id)
    #[getter]
    fn datasets(&self, py: Python<'_>) -> Py<PyCogneeDatasets> {
        self.datasets.clone_ref(py)
    }

    /// The session management surface for this handle.
    ///
    /// Use this to read and write QA sessions, feedback, and graph context:
    ///
    /// .. code-block:: python
    ///
    ///     entries = await cognee.sessions.get("session-id")
    ///     await cognee.sessions.set_graph_context("session-id", "ctx")
    ///     ctx = await cognee.sessions.get_graph_context("session-id")
    #[getter]
    fn sessions(&self, py: Python<'_>) -> Py<PyCogneeSessions> {
        self.sessions.clone_ref(py)
    }

    /// The notebook management surface for this handle.
    ///
    /// Use this to create, list, update, and delete notebooks:
    ///
    /// .. code-block:: python
    ///
    ///     notebooks = await cognee.notebooks.list()
    ///     nb = await cognee.notebooks.create("My Notebook")
    ///     await cognee.notebooks.delete(nb["id"])
    #[getter]
    fn notebooks(&self, py: Python<'_>) -> Py<PyCogneeNotebooks> {
        self.notebooks.clone_ref(py)
    }

    /// Build engines and resolve the default user.
    ///
    /// Awaitable — returns ``None`` on success.  Calling this explicitly
    /// before the first ``add()`` / ``cognify()`` / ``search()`` avoids
    /// a large cold-start latency on the first operation.
    fn warm<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            handle.services().await.map_err(sdk_error_to_py)?;
            // Return Python None (not the empty tuple that Ok(()) would produce).
            Python::with_gil(|py| Ok(py.None()))
        })
    }

    /// Return the owner UUID string.
    ///
    /// Awaitable — warms the handle lazily if services have not yet been
    /// built, then returns the UUID as a ``str``.
    fn owner_id<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            let id = handle.owner_id().await.map_err(sdk_error_to_py)?;
            Ok(id.to_string())
        })
    }
}
