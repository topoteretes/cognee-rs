use pyo3::prelude::*;

mod cancellation;
mod default_subscriber;
mod error;
mod logging;
mod pipeline;
mod progress;
mod task;
mod task_context;
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

    // Register exception types.
    error::register(m)?;

    Ok(())
}
