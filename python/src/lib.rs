use pyo3::prelude::*;

mod cancellation;
mod config;
mod default_subscriber;
mod error;
mod json;
mod logging;
mod pipeline;
mod progress;
mod sdk;
mod sdk_admin;
mod sdk_cloud;
mod sdk_data;
mod sdk_datasets;
mod sdk_error;
mod sdk_memory;
mod sdk_ops;
mod sdk_retrieval;
mod sdk_sessions;
mod sdk_visualization;
mod task;
mod task_context;
mod telemetry_analytics;
mod telemetry_otlp;
mod value;
mod watcher;

/// Connect the SDK to a Cognee Cloud instance (process-wide singleton).
///
/// When ``opts["url"]`` is set, **direct mode** is used — no Auth0 flow,
/// suitable for CI / testing with a local Cognee HTTP server.  When absent,
/// the Auth0 device-code flow is run (requires a TTY).
///
/// Optional opts keys (both ``snake_case`` and ``camelCase`` accepted):
///
/// - ``url`` — direct server URL
/// - ``api_key`` / ``apiKey``
/// - ``cloud_url`` / ``cloudUrl``
/// - ``auth0_domain`` / ``auth0Domain``
/// - ``auth0_client_id`` / ``auth0ClientId``
/// - ``auth0_audience`` / ``auth0Audience``
///
/// Returns ``{"connected": True, "serviceUrl": "…"}`` on success.
///
/// Raises ``CogneeFeatureNotBuiltError`` when the ``cloud`` Cargo feature was
/// not compiled in.
#[pyfunction]
#[pyo3(signature = (opts=None))]
fn serve<'py>(py: Python<'py>, opts: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
    sdk_cloud::py_serve(py, opts)
}

/// Disconnect from Cognee Cloud and revert to local-execution mode.
///
/// Optional opts keys (both ``snake_case`` and ``camelCase`` accepted):
///
/// - ``wipe_credentials`` / ``wipeCredentials`` — when ``True``, the on-disk
///   credential cache is deleted so the next :func:`serve` must
///   re-authenticate (default ``False``).
///
/// Returns ``None`` on success.
///
/// Raises ``CogneeFeatureNotBuiltError`` when the ``cloud`` Cargo feature was
/// not compiled in.
#[pyfunction]
#[pyo3(signature = (opts=None))]
fn disconnect<'py>(
    py: Python<'py>,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    sdk_cloud::py_disconnect(py, opts)
}

/// Python bindings for the cognee-core pipeline engine.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Gap 07 task 02 — install the default tracing → Python `logging`
    // bridge before any class registration so events emitted during
    // module init are captured. Idempotent; honours
    // `COGNEE_BINDING_SUPPRESS_LOGS`.
    default_subscriber::install(m.py());

    m.add_class::<sdk::PyCognee>()?;
    m.add_class::<config::PyCogneeConfig>()?;
    m.add_class::<sdk_datasets::PyCogneeDatasets>()?;
    m.add_class::<sdk_sessions::PyCogneeSessions>()?;
    m.add_class::<sdk_admin::PyCogneeNotebooks>()?;
    m.add_class::<pipeline::PyPipeline>()?;
    m.add_class::<pipeline::PyPipelineRunHandle>()?;
    m.add_class::<task_context::PyTaskContext>()?;
    m.add_class::<cancellation::PyCancellationHandle>()?;
    m.add_class::<cancellation::PyCancellationToken>()?;
    m.add_function(wrap_pyfunction!(cancellation::cancellation_pair, m)?)?;
    m.add_class::<progress::PyProgressToken>()?;

    // Logging entrypoint (gap-06): argument-less, idempotent.
    m.add_function(wrap_pyfunction!(logging::setup_logging, m)?)?;

    // Telemetry (OTLP) entrypoint (gap-07 task 05): argument-less,
    // idempotent. Composes the OTEL layer on top of the default
    // tracing → Python `logging` bridge installed above.
    m.add_function(wrap_pyfunction!(telemetry_otlp::setup_telemetry, m)?)?;

    // Analytics entrypoint (gap-07 task 06): argument-less, idempotent.
    // Arms `send_telemetry` per the Python-SDK parity policy (ON unless
    // TELEMETRY_DISABLED / ENV in {test,dev} / COGNEE_HOST_SDK is set).
    // Decisions 10, 12.
    m.add_function(wrap_pyfunction!(
        telemetry_analytics::setup_telemetry_analytics,
        m
    )?)?;
    // Arm analytics automatically on import so telemetry is ON by default
    // (Python-SDK parity) without requiring an explicit
    // `setup_telemetry_analytics()` call. Idempotent; honours the
    // standard opt-out env vars at emission time via `is_disabled()`.
    let _ = telemetry_analytics::arm();

    // Register engine-tier exception types (PipelineError hierarchy).
    error::register(m)?;

    // Register SDK-tier exception types (CogneeError hierarchy).
    sdk_error::register(m)?;

    // Cloud ops: module-level serve / disconnect (process-wide singleton).
    m.add_function(wrap_pyfunction!(serve, m)?)?;
    m.add_function(wrap_pyfunction!(disconnect, m)?)?;

    Ok(())
}
