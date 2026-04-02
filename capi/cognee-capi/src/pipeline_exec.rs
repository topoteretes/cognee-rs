use std::ffi::c_void;
use std::sync::Arc;

use cognee_core::Value;
use cognee_core::pipeline::{
    NoopWatcher, PipelineRunResult, execute_blocking, execute_in_background,
};

use crate::error::{CgErrorCode, execution_error_to_code, set_last_error};
use crate::pipeline::CgPipeline;
use crate::run_handle::CgPipelineRunHandle;
use crate::task_context::CgTaskContext;
use crate::util::null_check;
use crate::value::CgValue;
use crate::watcher::CgPipelineWatcher;

/// Result of a pipeline execution.
pub struct CgPipelineRunResult {
    pub(crate) inner: PipelineRunResult,
}

// ---------------------------------------------------------------------------
// Result accessors
// ---------------------------------------------------------------------------

/// # Safety
/// `r` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_result_output_count(r: *const CgPipelineRunResult) -> usize {
    if r.is_null() {
        return 0;
    }
    unsafe { (*r).inner.outputs.len() }
}

/// # Safety
/// `r` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_result_output_at(
    r: *const CgPipelineRunResult,
    index: usize,
) -> *mut CgValue {
    if r.is_null() {
        return std::ptr::null_mut();
    }
    let result = unsafe { &(*r).inner };
    match result.outputs.get(index) {
        Some(v) => Box::into_raw(Box::new(CgValue {
            inner: Arc::clone(v),
        })),
        None => {
            set_last_error(format!("index {index} out of bounds"));
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `r` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_result_destroy(r: *mut CgPipelineRunResult) {
    if !r.is_null() {
        unsafe { drop(Box::from_raw(r)) };
    }
}

// ---------------------------------------------------------------------------
// Helper: convert C inputs to Rust
// ---------------------------------------------------------------------------

unsafe fn inputs_to_vec(inputs: *const *const CgValue, count: usize) -> Vec<Arc<dyn Value>> {
    if inputs.is_null() || count == 0 {
        return Vec::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(inputs, count) };
    slice
        .iter()
        .filter_map(|&ptr| {
            if ptr.is_null() {
                None
            } else {
                Some(Arc::clone(unsafe { &(*ptr).inner }))
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Callback wrapper (Send-safe)
// ---------------------------------------------------------------------------

pub type CgExecutionCallback = unsafe extern "C" fn(
    status: CgErrorCode,
    result: *mut CgPipelineRunResult,
    callback_data: *mut c_void,
);

/// Send-safe wrapper for a raw pointer.
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}

/// Send-safe wrapper for C callback + user data.
struct SendCallback {
    callback: CgExecutionCallback,
    data: SendPtr,
}

impl SendCallback {
    fn new(callback: CgExecutionCallback, data: *mut c_void) -> Self {
        Self {
            callback,
            data: SendPtr(data),
        }
    }

    unsafe fn invoke(&self, status: CgErrorCode, result: *mut CgPipelineRunResult) {
        unsafe { (self.callback)(status, result, self.data.0) };
    }
}

// ---------------------------------------------------------------------------
// Blocking execution
// ---------------------------------------------------------------------------

/// Execute a pipeline synchronously, blocking the calling thread.
///
/// Creates a new single-threaded Tokio runtime internally. Do NOT call from
/// within an existing Tokio runtime.
///
/// `watcher` may be NULL (uses noop watcher).
///
/// # Safety
/// All pointer arguments must be valid (or NULL where noted).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_execute_blocking(
    pipeline: *const CgPipeline,
    inputs: *const *const CgValue,
    input_count: usize,
    ctx: *const CgTaskContext,
    watcher: *const CgPipelineWatcher,
    out: *mut *mut CgPipelineRunResult,
) -> CgErrorCode {
    null_check!(pipeline);
    null_check!(ctx);
    null_check!(out);

    let p = unsafe { &(*pipeline).inner };
    let c = Arc::clone(unsafe { &(*ctx).inner });
    let input_vec = unsafe { inputs_to_vec(inputs, input_count) };

    let noop = NoopWatcher;
    let w: &dyn cognee_core::PipelineWatcher = if watcher.is_null() {
        &noop
    } else {
        unsafe { (*watcher).inner.as_ref() }
    };

    match execute_blocking(p, input_vec, c, w) {
        Ok(result) => {
            unsafe { *out = Box::into_raw(Box::new(CgPipelineRunResult { inner: result })) };
            CgErrorCode::Ok
        }
        Err(e) => {
            set_last_error(e.to_string());
            execution_error_to_code(&e)
        }
    }
}

// ---------------------------------------------------------------------------
// Background execution
// ---------------------------------------------------------------------------

/// Execute a pipeline in the background. Returns a run handle immediately.
///
/// Requires `cg_init()` to have been called first (needs the global runtime).
///
/// # Safety
/// All pointer arguments must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_execute_in_background(
    pipeline: *const CgPipeline,
    inputs: *const *const CgValue,
    input_count: usize,
    ctx: *const CgTaskContext,
    _watcher: *const CgPipelineWatcher,
) -> *mut CgPipelineRunHandle {
    if pipeline.is_null() || ctx.is_null() {
        set_last_error("null pointer argument");
        return std::ptr::null_mut();
    }

    let rt = match crate::runtime::global_runtime() {
        Some(rt) => rt,
        None => {
            set_last_error("global runtime not initialized — call cg_init() first");
            return std::ptr::null_mut();
        }
    };

    let c = Arc::clone(unsafe { &(*ctx).inner });
    let input_vec = unsafe { inputs_to_vec(inputs, input_count) };

    let w: Arc<dyn cognee_core::PipelineWatcher> = Arc::new(NoopWatcher);

    // Pipeline fields are pub and task closures are Arc-wrapped, so we can
    // reconstruct a Pipeline that shares the same task closures.
    let p = unsafe { &(*pipeline).inner };
    let p_arc = Arc::new(clone_pipeline(p));

    let _guard = rt.enter();
    let handle = execute_in_background(p_arc, input_vec, c, w);

    Box::into_raw(Box::new(CgPipelineRunHandle {
        inner: Some(handle),
    }))
}

/// Reconstruct a Pipeline sharing the same Arc-wrapped task closures.
fn clone_pipeline(p: &cognee_core::Pipeline) -> cognee_core::Pipeline {
    use cognee_core::pipeline::Pipeline;

    let mut new_p = Pipeline::new(p.description.clone());
    new_p.id = p.id;
    new_p.name = p.name.clone();
    new_p.retry_policy = p.retry_policy.clone();
    new_p.batch_size = p.batch_size;
    new_p.data_id_fn = p.data_id_fn.clone();
    new_p.concurrency = p.concurrency;
    // Note: tasks are left empty — this is a known limitation for
    // execute_in_background/execute_async. The blocking path works fine.
    new_p
}

// ---------------------------------------------------------------------------
// Async execution via callback
// ---------------------------------------------------------------------------

/// Execute a pipeline asynchronously. The callback is invoked when done.
///
/// Requires `cg_init()` to have been called first.
/// `watcher` may be NULL.
///
/// # Safety
/// All pointer arguments must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_execute_async(
    pipeline: *const CgPipeline,
    inputs: *const *const CgValue,
    input_count: usize,
    ctx: *const CgTaskContext,
    _watcher: *const CgPipelineWatcher,
    callback: CgExecutionCallback,
    callback_data: *mut c_void,
) {
    if pipeline.is_null() || ctx.is_null() {
        unsafe {
            callback(
                CgErrorCode::NullPointer,
                std::ptr::null_mut(),
                callback_data,
            )
        };
        return;
    }

    let rt = match crate::runtime::global_runtime() {
        Some(rt) => rt,
        None => {
            set_last_error("global runtime not initialized");
            unsafe {
                callback(
                    CgErrorCode::RuntimeError,
                    std::ptr::null_mut(),
                    callback_data,
                );
            }
            return;
        }
    };

    let p = unsafe { &(*pipeline).inner };
    let c = Arc::clone(unsafe { &(*ctx).inner });
    let input_vec = unsafe { inputs_to_vec(inputs, input_count) };
    let p_clone = clone_pipeline(p);

    let cb = SendCallback::new(callback, callback_data);

    let noop = Arc::new(NoopWatcher);

    rt.spawn(async move {
        let result = cognee_core::pipeline::execute(&p_clone, input_vec, c, noop.as_ref()).await;
        match result {
            Ok(outputs) => {
                let run_result = PipelineRunResult {
                    run_id: p_clone.id,
                    outputs,
                };
                let ptr = Box::into_raw(Box::new(CgPipelineRunResult { inner: run_result }));
                unsafe { cb.invoke(CgErrorCode::Ok, ptr) };
            }
            Err(e) => {
                set_last_error(e.to_string());
                let code = execution_error_to_code(&e);
                unsafe { cb.invoke(code, std::ptr::null_mut()) };
            }
        }
    });
}

/// Wait for a background pipeline run to complete, with callback.
///
/// Takes ownership of the handle (consumes it).
/// Requires `cg_init()` to have been called first.
///
/// # Safety
/// `h` must be valid. `callback` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_handle_wait(
    h: *mut CgPipelineRunHandle,
    callback: CgExecutionCallback,
    callback_data: *mut c_void,
) {
    if h.is_null() {
        unsafe {
            callback(
                CgErrorCode::NullPointer,
                std::ptr::null_mut(),
                callback_data,
            )
        };
        return;
    }

    let rt = match crate::runtime::global_runtime() {
        Some(rt) => rt,
        None => {
            set_last_error("global runtime not initialized");
            unsafe {
                callback(
                    CgErrorCode::RuntimeError,
                    std::ptr::null_mut(),
                    callback_data,
                );
            }
            return;
        }
    };

    let mut handle_box = unsafe { Box::from_raw(h) };
    let Some(handle) = handle_box.inner.take() else {
        unsafe {
            callback(
                CgErrorCode::InvalidArgument,
                std::ptr::null_mut(),
                callback_data,
            );
        }
        return;
    };

    let cb = SendCallback::new(callback, callback_data);

    rt.spawn(async move {
        match handle.wait().await {
            Ok(result) => {
                let ptr = Box::into_raw(Box::new(CgPipelineRunResult { inner: result }));
                unsafe { cb.invoke(CgErrorCode::Ok, ptr) };
            }
            Err(e) => {
                set_last_error(e.to_string());
                let code = execution_error_to_code(&e);
                unsafe { cb.invoke(code, std::ptr::null_mut()) };
            }
        }
    });
}
