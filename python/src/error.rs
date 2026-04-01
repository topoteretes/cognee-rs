use cognee_core::ExecutionError;
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(cognee_pipeline, PipelineError, PyException);
create_exception!(cognee_pipeline, TaskFailedError, PipelineError);
create_exception!(cognee_pipeline, CancelledError, PipelineError);
create_exception!(cognee_pipeline, NoTasksError, PipelineError);
create_exception!(cognee_pipeline, InvalidConfigError, PipelineError);

pub fn execution_error_to_pyerr(e: ExecutionError) -> PyErr {
    match e {
        ExecutionError::TaskFailed {
            task_index,
            attempts,
            source,
        } => TaskFailedError::new_err(format!(
            "task {task_index} failed after {attempts} attempt(s): {source}"
        )),
        ExecutionError::Cancelled => CancelledError::new_err("pipeline was cancelled"),
        ExecutionError::NoTasks => NoTasksError::new_err("pipeline has no tasks"),
        ExecutionError::InvalidConfig { reason } => InvalidConfigError::new_err(reason),
    }
}

/// Register exception classes on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("PipelineError", m.py().get_type::<PipelineError>())?;
    m.add("TaskFailedError", m.py().get_type::<TaskFailedError>())?;
    m.add("CancelledError", m.py().get_type::<CancelledError>())?;
    m.add("NoTasksError", m.py().get_type::<NoTasksError>())?;
    m.add(
        "InvalidConfigError",
        m.py().get_type::<InvalidConfigError>(),
    )?;
    Ok(())
}
