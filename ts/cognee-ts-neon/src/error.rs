use cognee_core::{CoreError, ExecutionError};
use neon::prelude::*;

/// Throw a JS Error from an `ExecutionError`.
pub fn throw_execution_error<'cx, T>(
    cx: &mut impl Context<'cx>,
    err: ExecutionError,
) -> NeonResult<T> {
    let code = match &err {
        ExecutionError::TaskFailed { .. } => "TASK_FAILED",
        ExecutionError::Cancelled => "CANCELLED",
        ExecutionError::NoTasks => "NO_TASKS",
        ExecutionError::InvalidConfig { .. } => "INVALID_CONFIG",
    };
    let msg = err.to_string();
    let js_err = cx.error(msg)?;
    let code_val = cx.string(code);
    js_err
        .downcast_or_throw::<JsObject, _>(cx)?
        .set(cx, "code", code_val)?;
    cx.throw(js_err)
}

/// Throw a JS Error from a `CoreError`.
pub fn throw_core_error<'cx, T>(cx: &mut impl Context<'cx>, err: CoreError) -> NeonResult<T> {
    let code = match &err {
        CoreError::Runtime(_) => "RUNTIME_ERROR",
        CoreError::ThreadPoolBuild(_) => "RUNTIME_ERROR",
        CoreError::TaskAborted { .. } => "TASK_ABORTED",
        CoreError::MissingContextField { .. } => "MISSING_FIELD",
        CoreError::InvalidProgressSplit { .. } => "INVALID_ARGUMENT",
    };
    let msg = err.to_string();
    let js_err = cx.error(msg)?;
    let code_val = cx.string(code);
    js_err
        .downcast_or_throw::<JsObject, _>(cx)?
        .set(cx, "code", code_val)?;
    cx.throw(js_err)
}
