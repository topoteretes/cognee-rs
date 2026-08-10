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

    /// Close the handle, releasing the resources it opened.
    ///
    /// Blocking and deterministic: when this returns, the relational connection
    /// pool is closed and a SQLite database's ``-wal``/``-shm`` sidecar files are
    /// gone, so the directory holding them can be deleted. Dropping the handle is
    /// not enough on its own — the pool's destructor lets its connections tear
    /// down concurrently, and SQLite only unlinks the sidecars when the *last*
    /// connection closes, so relying on the drop orphans the files.
    ///
    /// Idempotent, and a no-op on a handle that was never warmed. Any operation
    /// started afterwards raises ``CogneeRuntimeError`` instead of silently
    /// reopening the database — including operations on a surface obtained
    /// earlier, such as ``cognee.datasets``.
    ///
    /// Usable as a context manager, which closes on exit:
    ///
    /// .. code-block:: python
    ///
    ///     with Cognee(settings) as cognee:
    ///         ...
    ///
    ///     async with Cognee(settings) as cognee:
    ///         await cognee.warm()
    fn close(&self, py: Python<'_>) {
        let rt = pyo3_async_runtimes::tokio::get_runtime();
        // Release the GIL while blocking: the teardown is pure Rust and needs no
        // interpreter, and holding the GIL would stall every other Python thread
        // for the duration of the close.
        py.allow_threads(|| self.inner.close_blocking(rt.handle()));
    }

    /// Enter the synchronous context manager (returns ``self``).
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Exit the synchronous context manager: closes the handle.
    ///
    /// Returns ``False`` so an exception raised inside the ``with`` body
    /// propagates.
    #[pyo3(signature = (_exc_type=None, _exc_value=None, _traceback=None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_value: Option<&Bound<'_, PyAny>>,
        _traceback: Option<&Bound<'_, PyAny>>,
    ) -> bool {
        self.close(py);
        false
    }

    /// Enter the async context manager (awaitable, returns ``self``).
    fn __aenter__<'py>(slf: PyRef<'py, Self>, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let this = slf.into_pyobject(py)?.unbind();
        pyo3_async_runtimes::tokio::future_into_py(py, async move { Ok(this) })
    }

    /// Exit the async context manager: closes the handle without blocking the
    /// event loop (the teardown is awaited on the tokio runtime).
    ///
    /// Resolves to ``False`` so an exception raised inside the ``async with``
    /// body propagates.
    #[pyo3(signature = (_exc_type=None, _exc_value=None, _traceback=None))]
    fn __aexit__<'py>(
        &self,
        py: Python<'py>,
        _exc_type: Option<&Bound<'_, PyAny>>,
        _exc_value: Option<&Bound<'_, PyAny>>,
        _traceback: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let handle = Arc::clone(&self.inner);
        pyo3_async_runtimes::tokio::future_into_py(py, async move {
            handle.close().await;
            Ok(false)
        })
    }

    /// Whether :meth:`close` has been called on this handle.
    #[getter]
    fn closed(&self) -> bool {
        self.inner.is_closed()
    }
}

impl Drop for PyCognee {
    /// Give the resources back when the handle is garbage-collected without an
    /// explicit :meth:`close`.
    ///
    /// This is the *implicit* teardown, so it releases without closing: a surface
    /// the caller kept a reference to (``d = cognee.datasets; del cognee``) shares
    /// this handle's state and has to keep working — it simply re-warms against a
    /// fresh connection. Blocking here is what makes the release deterministic:
    /// spawning it would race with interpreter shutdown, which is exactly when a
    /// handle nobody closed tends to be collected, and the sidecars would survive.
    fn drop(&mut self) {
        // Nothing was ever opened (a handle that never warmed) → nothing to give
        // back, and in particular no reason to build a tokio runtime here.
        if !self.inner.has_open_resources() {
            return;
        }
        let rt = pyo3_async_runtimes::tokio::get_runtime();
        self.inner.release_blocking(rt.handle());
    }
}
