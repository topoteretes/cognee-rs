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
mod sdk_error;
mod sdk_ops;
mod task;
mod task_context;
mod telemetry_analytics;
mod telemetry_otlp;
mod value;
mod watcher;

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
    m.add_class::<pipeline::PyPipeline>()?;
    m.add_class::<pipeline::PyPipelineRunHandle>()?;
    m.add_class::<task_context::PyTaskContext>()?;
    m.add_class::<cancellation::PyCancellationHandle>()?;
    m.add_class::<progress::PyProgressToken>()?;

    // Logging entrypoint (gap-06): argument-less, idempotent.
    m.add_function(wrap_pyfunction!(logging::setup_logging, m)?)?;

    // Telemetry (OTLP) entrypoint (gap-07 task 05): argument-less,
    // idempotent. Composes the OTEL layer on top of the default
    // tracing → Python `logging` bridge installed above.
    m.add_function(wrap_pyfunction!(telemetry_otlp::setup_telemetry, m)?)?;

    // Analytics entrypoint (gap-07 task 06): argument-less, idempotent.
    // Arms `send_telemetry` only when the per-binding policy permits
    // (PyO3 default OFF unless `COGNEE_RUST_TELEMETRY=1` and
    // `COGNEE_HOST_SDK` is unset). Decisions 10, 11, 12.
    m.add_function(wrap_pyfunction!(
        telemetry_analytics::setup_telemetry_analytics,
        m
    )?)?;

    // Register engine-tier exception types (PipelineError hierarchy).
    error::register(m)?;

    // Register SDK-tier exception types (CogneeError hierarchy).
    sdk_error::register(m)?;

    Ok(())
}
