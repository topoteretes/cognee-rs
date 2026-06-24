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

// Cloud ops (`serve` / `disconnect`) live in the closed Python cdylib
// `cognee-py-cloud` (T15e) which wraps `cognee-bindings-cloud`. The OSS
// `cognee-pipeline` package does not expose them.

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

    // Cloud ops (`serve` / `disconnect`) are registered by the closed
    // `cognee-py-cloud` cdylib (T15e), not by the OSS `cognee-pipeline`.

    Ok(())
}
