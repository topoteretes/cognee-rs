use pyo3::prelude::*;

mod cancellation;
mod error;
mod pipeline;
mod progress;
mod task;
mod task_context;
mod value;
mod watcher;

/// Python bindings for the cognee-core pipeline engine.
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<pipeline::PyPipeline>()?;
    m.add_class::<pipeline::PyPipelineRunHandle>()?;
    m.add_class::<task_context::PyTaskContext>()?;
    m.add_class::<cancellation::PyCancellationHandle>()?;
    m.add_class::<progress::PyProgressToken>()?;

    // Register exception types.
    error::register(m)?;

    Ok(())
}
