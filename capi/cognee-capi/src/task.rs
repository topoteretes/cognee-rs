use std::ffi::c_void;
use std::sync::Arc;

use cognee_core::{Task, TaskContext, TaskError, Value, ValueIter, ValueStream};
use futures::future::BoxFuture;
use futures::stream::StreamExt;

use crate::error::CgErrorCode;
use crate::iterator::CgValueIter;
use crate::task_context::CgTaskContext;
use crate::value::CgValue;

// ---------------------------------------------------------------------------
// C callback type definitions
// ---------------------------------------------------------------------------

/// Callback for async single-value results.
pub type CgAsyncResultCallback =
    unsafe extern "C" fn(status: CgErrorCode, result: *mut CgValue, callback_data: *mut c_void);

/// Callback invoked for each item in an async stream.
pub type CgStreamYieldFn = unsafe extern "C" fn(item: *mut CgValue, stream_data: *mut c_void);

/// Callback invoked when an async stream completes.
pub type CgStreamCompleteFn = unsafe extern "C" fn(status: CgErrorCode, stream_data: *mut c_void);

// ---------------------------------------------------------------------------
// Single-value C function pointer types
// ---------------------------------------------------------------------------

pub type CgSyncFnPtr = unsafe extern "C" fn(
    input: *const CgValue,
    ctx: *const CgTaskContext,
    user_data: *mut c_void,
    out: *mut *mut CgValue,
) -> CgErrorCode;

pub type CgAsyncFnPtr = unsafe extern "C" fn(
    input: *const CgValue,
    ctx: *const CgTaskContext,
    user_data: *mut c_void,
    callback: CgAsyncResultCallback,
    callback_data: *mut c_void,
);

pub type CgSyncIterFnPtr = unsafe extern "C" fn(
    input: *const CgValue,
    ctx: *const CgTaskContext,
    user_data: *mut c_void,
    out: *mut *mut CgValueIter,
) -> CgErrorCode;

pub type CgAsyncStreamFnPtr = unsafe extern "C" fn(
    input: *const CgValue,
    ctx: *const CgTaskContext,
    user_data: *mut c_void,
    yield_fn: CgStreamYieldFn,
    complete_fn: CgStreamCompleteFn,
    stream_data: *mut c_void,
);

// ---------------------------------------------------------------------------
// Batch C function pointer types
// ---------------------------------------------------------------------------

pub type CgSyncBatchFnPtr = unsafe extern "C" fn(
    items: *const *const CgValue,
    count: usize,
    ctx: *const CgTaskContext,
    user_data: *mut c_void,
    out: *mut *mut CgValue,
) -> CgErrorCode;

pub type CgAsyncBatchFnPtr = unsafe extern "C" fn(
    items: *const *const CgValue,
    count: usize,
    ctx: *const CgTaskContext,
    user_data: *mut c_void,
    callback: CgAsyncResultCallback,
    callback_data: *mut c_void,
);

pub type CgSyncIterBatchFnPtr = unsafe extern "C" fn(
    items: *const *const CgValue,
    count: usize,
    ctx: *const CgTaskContext,
    user_data: *mut c_void,
    out: *mut *mut CgValueIter,
) -> CgErrorCode;

pub type CgAsyncStreamBatchFnPtr = unsafe extern "C" fn(
    items: *const *const CgValue,
    count: usize,
    ctx: *const CgTaskContext,
    user_data: *mut c_void,
    yield_fn: CgStreamYieldFn,
    complete_fn: CgStreamCompleteFn,
    stream_data: *mut c_void,
);

// ---------------------------------------------------------------------------
// Opaque CgTask handle
// ---------------------------------------------------------------------------

pub struct CgTask {
    pub(crate) inner: Task,
}

// ---------------------------------------------------------------------------
// User-data wrapper (shared across calls, dropped when task is dropped)
// ---------------------------------------------------------------------------

struct UserData {
    ptr: *mut c_void,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}

unsafe impl Send for UserData {}
unsafe impl Sync for UserData {}

impl Drop for UserData {
    fn drop(&mut self) {
        if let Some(dtor) = self.destroy {
            unsafe { dtor(self.ptr) };
        }
    }
}

// ---------------------------------------------------------------------------
// Helper: borrow a CgValue from an Arc<dyn Value> for the duration of a call
// ---------------------------------------------------------------------------

fn arc_to_cg_ptr(arc: &Arc<dyn Value>) -> *const CgValue {
    // Create a temporary CgValue on the stack-ish; we need a stable pointer.
    // We'll leak a Box temporarily and recover it after the call.
    let cg = Box::new(CgValue {
        inner: Arc::clone(arc),
    });
    Box::into_raw(cg) as *const CgValue
}

/// Recover and drop a CgValue pointer created by `arc_to_cg_ptr`.
unsafe fn drop_cg_ptr(ptr: *const CgValue) {
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr as *mut CgValue)) };
    }
}

fn ctx_to_cg_ptr(ctx: &Arc<TaskContext>) -> *const CgTaskContext {
    let cg = Box::new(CgTaskContext {
        inner: Arc::clone(ctx),
    });
    Box::into_raw(cg) as *const CgTaskContext
}

unsafe fn drop_ctx_ptr(ptr: *const CgTaskContext) {
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr as *mut CgTaskContext)) };
    }
}

fn cg_result_to_arc(ptr: *mut CgValue) -> Result<Arc<dyn Value>, TaskError> {
    if ptr.is_null() {
        return Err("task returned null output".into());
    }
    let cg = unsafe { Box::from_raw(ptr) };
    Ok(cg.inner)
}

fn error_code_to_task_error(code: CgErrorCode) -> TaskError {
    // Try to read the thread-local last error message
    let msg = unsafe {
        let ptr = crate::error::cg_last_error_message();
        if ptr.is_null() {
            format!("task failed with code {:?}", code)
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    msg.into()
}

/// Build a batch pointer array from `&[Box<dyn Value>]`.
fn batch_to_cg_ptrs(items: &[Box<dyn Value>]) -> (Vec<*const CgValue>, Vec<*mut CgValue>) {
    let mut ptrs = Vec::with_capacity(items.len());
    let mut owned = Vec::with_capacity(items.len());
    for item in items {
        let arc: Arc<dyn Value> = Arc::from(Box::new(clone_box_value(item)) as Box<dyn Value>);
        let cg = Box::new(CgValue { inner: arc });
        let raw = Box::into_raw(cg);
        ptrs.push(raw as *const CgValue);
        owned.push(raw);
    }
    (ptrs, owned)
}

/// Clone a `Box<dyn Value>` item by wrapping an Arc around its Any representation.
fn clone_box_value(item: &dyn Value) -> ArcHolder {
    // We can't clone a dyn Value directly, but we can wrap a reference.
    // Since the item is borrowed for the duration of the C call, we use a raw-ptr trick.
    // Actually, for batch calls, the items are `&[Box<dyn Value>]` and we need
    // pointers that are valid for the call duration. Let's just create CgValue
    // wrappers that borrow via raw pointer.
    //
    // Simpler approach: create CgValue handles pointing into the existing data.
    // We'll use a different strategy — stack-allocate CgValue wrappers.
    ArcHolder(item as *const dyn Value)
}

/// Holds a raw pointer to a dyn Value for the duration of a batch call.
struct ArcHolder(*const dyn Value);
unsafe impl Send for ArcHolder {}
unsafe impl Sync for ArcHolder {}

// ---------------------------------------------------------------------------
// Alternative batch approach: create temporary CgValue wrappers on the stack
// ---------------------------------------------------------------------------

/// Create temporary CgValue pointers from `&[Box<dyn Value>]`.
/// The returned CgValues borrow from the input slice and must not outlive it.
fn batch_to_temp_ptrs(items: &[Box<dyn Value>]) -> Vec<CgValue> {
    items
        .iter()
        .map(|item| {
            // Create a thin Arc wrapping a raw pointer to the item.
            // This is safe because the CgValue won't outlive the slice.
            let ptr = item.as_ref() as *const dyn Value;
            // We need an Arc<dyn Value>, but the item is behind &Box<dyn Value>.
            // The safest approach: create a short-lived Arc from a clone.
            // Since we can't clone dyn Value, wrap the raw pointer.
            CgValue {
                inner: Arc::new(RawPtrValue(ptr)),
            }
        })
        .collect()
}

/// Wraps a raw pointer to a `dyn Value` for short-lived batch FFI calls.
struct RawPtrValue(*const dyn Value);
unsafe impl Send for RawPtrValue {}
unsafe impl Sync for RawPtrValue {}

// ---------------------------------------------------------------------------
// Task constructors
// ---------------------------------------------------------------------------

/// Create a synchronous task from a C function pointer.
///
/// # Safety
/// - `fn_ptr` must be a valid function pointer.
/// - `user_data` must be valid until `destroy_ud` is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_sync(
    fn_ptr: CgSyncFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgTask {
    let ud = Arc::new(UserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let task = Task::sync(move |input: Arc<dyn Value>, ctx: Arc<TaskContext>| {
        let input_ptr = arc_to_cg_ptr(&input);
        let ctx_ptr = ctx_to_cg_ptr(&ctx);
        let mut out: *mut CgValue = std::ptr::null_mut();
        let _ = &ud; // ensure ud is captured and kept alive

        let code = unsafe { fn_ptr(input_ptr, ctx_ptr, ud.ptr, &mut out) };

        unsafe {
            drop_cg_ptr(input_ptr);
            drop_ctx_ptr(ctx_ptr);
        }

        if code != CgErrorCode::Ok {
            return Err(error_code_to_task_error(code));
        }
        cg_result_to_arc(out)
    });

    Box::into_raw(Box::new(CgTask { inner: task }))
}

/// Create an asynchronous task from a C function pointer using callbacks.
///
/// The C function is called immediately. It must call `callback` exactly once
/// (from any thread) when the work is done.
///
/// # Safety
/// See `cg_task_sync` safety requirements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_async(
    fn_ptr: CgAsyncFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgTask {
    let ud = Arc::new(UserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let task = Task::async_fn(move |input: Arc<dyn Value>, ctx: Arc<TaskContext>| {
        let input_ptr = arc_to_cg_ptr(&input);
        let ctx_ptr = ctx_to_cg_ptr(&ctx);
        let ud = Arc::clone(&ud);

        let (tx, rx) = tokio::sync::oneshot::channel::<Result<Arc<dyn Value>, TaskError>>();

        // The callback data is a leaked Box containing the sender.
        let cb_data = Box::into_raw(Box::new(tx)) as *mut c_void;

        unsafe {
            fn_ptr(input_ptr, ctx_ptr, ud.ptr, async_result_trampoline, cb_data);
            drop_cg_ptr(input_ptr);
            drop_ctx_ptr(ctx_ptr);
        }

        Box::pin(async move {
            match rx.await {
                Ok(result) => result,
                Err(_) => Err("async callback was never called (channel dropped)".into()),
            }
        }) as BoxFuture<'static, Result<Arc<dyn Value>, TaskError>>
    });

    Box::into_raw(Box::new(CgTask { inner: task }))
}

/// Trampoline called by C async tasks to deliver the result.
unsafe extern "C" fn async_result_trampoline(
    status: CgErrorCode,
    result: *mut CgValue,
    callback_data: *mut c_void,
) {
    let tx = unsafe {
        Box::from_raw(
            callback_data as *mut tokio::sync::oneshot::Sender<Result<Arc<dyn Value>, TaskError>>,
        )
    };

    let res = if status == CgErrorCode::Ok {
        cg_result_to_arc(result)
    } else {
        // Free the result if non-null
        if !result.is_null() {
            unsafe { drop(Box::from_raw(result)) };
        }
        Err(error_code_to_task_error(status))
    };

    let _ = tx.send(res);
}

/// Create a synchronous iterator-producing task.
///
/// # Safety
/// See `cg_task_sync` safety requirements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_sync_iter(
    fn_ptr: CgSyncIterFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgTask {
    let ud = Arc::new(UserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let task = Task::sync_iter(move |input: Arc<dyn Value>, ctx: Arc<TaskContext>| {
        let input_ptr = arc_to_cg_ptr(&input);
        let ctx_ptr = ctx_to_cg_ptr(&ctx);
        let mut out: *mut CgValueIter = std::ptr::null_mut();
        let _ = &ud;

        let code = unsafe { fn_ptr(input_ptr, ctx_ptr, ud.ptr, &mut out) };

        unsafe {
            drop_cg_ptr(input_ptr);
            drop_ctx_ptr(ctx_ptr);
        }

        if code != CgErrorCode::Ok {
            return Err(error_code_to_task_error(code));
        }
        if out.is_null() {
            return Err("sync_iter task returned null iterator".into());
        }
        Ok(unsafe { crate::iterator::into_value_iter(out) })
    });

    Box::into_raw(Box::new(CgTask { inner: task }))
}

/// Create an async stream-producing task using push callbacks.
///
/// # Safety
/// See `cg_task_sync` safety requirements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_async_stream(
    fn_ptr: CgAsyncStreamFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgTask {
    let ud = Arc::new(UserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let task = Task::async_stream(
        move |input: Arc<dyn Value>, ctx: Arc<TaskContext>| -> Result<ValueStream, TaskError> {
            let input_ptr = arc_to_cg_ptr(&input);
            let ctx_ptr = ctx_to_cg_ptr(&ctx);
            let ud = Arc::clone(&ud);

            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Value>>();

            // Stream data contains the sender and a completion channel
            let (complete_tx, _complete_rx) =
                tokio::sync::oneshot::channel::<Result<(), TaskError>>();

            let stream_data = Box::into_raw(Box::new(StreamCallbackData {
                item_tx: tx,
                complete_tx: Some(complete_tx),
            })) as *mut c_void;

            unsafe {
                fn_ptr(
                    input_ptr,
                    ctx_ptr,
                    ud.ptr,
                    stream_yield_trampoline,
                    stream_complete_trampoline,
                    stream_data,
                );
                drop_cg_ptr(input_ptr);
                drop_ctx_ptr(ctx_ptr);
            }

            let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
                .map(|v| v as Box<dyn Value>);

            Ok(Box::pin(stream) as ValueStream)
        },
    );

    Box::into_raw(Box::new(CgTask { inner: task }))
}

struct StreamCallbackData {
    item_tx: tokio::sync::mpsc::UnboundedSender<Box<dyn Value>>,
    complete_tx: Option<tokio::sync::oneshot::Sender<Result<(), TaskError>>>,
}

unsafe extern "C" fn stream_yield_trampoline(item: *mut CgValue, stream_data: *mut c_void) {
    let data = unsafe { &*(stream_data as *const StreamCallbackData) };
    if !item.is_null() {
        let cg = unsafe { Box::from_raw(item) };
        let boxed: Box<dyn Value> = Box::new(ArcValueWrapper(cg.inner));
        let _ = data.item_tx.send(boxed);
    }
}

unsafe extern "C" fn stream_complete_trampoline(status: CgErrorCode, stream_data: *mut c_void) {
    let mut data = unsafe { Box::from_raw(stream_data as *mut StreamCallbackData) };
    if let Some(tx) = data.complete_tx.take() {
        if status == CgErrorCode::Ok {
            let _ = tx.send(Ok(()));
        } else {
            let _ = tx.send(Err(error_code_to_task_error(status)));
        }
    }
    // Dropping data closes item_tx, which ends the stream.
}

/// Wrapper to hold `Arc<dyn Value>` as a boxed `Value`.
struct ArcValueWrapper(Arc<dyn Value>);

// ---------------------------------------------------------------------------
// Batch task constructors
// ---------------------------------------------------------------------------

/// Create a synchronous batch task.
///
/// # Safety
/// See `cg_task_sync` safety requirements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_sync_batch(
    fn_ptr: CgSyncBatchFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgTask {
    let ud = Arc::new(UserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let task = Task::sync_batch(move |items: &[Box<dyn Value>], ctx: Arc<TaskContext>| {
        let temp_values = batch_to_temp_ptrs(items);
        let ptrs: Vec<*const CgValue> = temp_values.iter().map(|v| v as *const CgValue).collect();
        let ctx_ptr = ctx_to_cg_ptr(&ctx);
        let mut out: *mut CgValue = std::ptr::null_mut();
        let _ = &ud;

        let code = unsafe { fn_ptr(ptrs.as_ptr(), items.len(), ctx_ptr, ud.ptr, &mut out) };

        unsafe { drop_ctx_ptr(ctx_ptr) };
        drop(temp_values);

        if code != CgErrorCode::Ok {
            return Err(error_code_to_task_error(code));
        }
        cg_result_to_arc(out)
    });

    Box::into_raw(Box::new(CgTask { inner: task }))
}

/// Create an asynchronous batch task.
///
/// # Safety
/// See `cg_task_sync` safety requirements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_async_batch(
    fn_ptr: CgAsyncBatchFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgTask {
    let ud = Arc::new(UserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let task = Task::async_batch(move |items: &[Box<dyn Value>], ctx: Arc<TaskContext>| {
        let temp_values = batch_to_temp_ptrs(items);
        let ptrs: Vec<*const CgValue> = temp_values.iter().map(|v| v as *const CgValue).collect();
        let ctx_ptr = ctx_to_cg_ptr(&ctx);
        let ud = Arc::clone(&ud);

        let (tx, rx) = tokio::sync::oneshot::channel::<Result<Arc<dyn Value>, TaskError>>();
        let cb_data = Box::into_raw(Box::new(tx)) as *mut c_void;

        unsafe {
            fn_ptr(
                ptrs.as_ptr(),
                ptrs.len(),
                ctx_ptr,
                ud.ptr,
                async_result_trampoline,
                cb_data,
            );
            drop_ctx_ptr(ctx_ptr);
        }
        drop(temp_values);

        Box::pin(async move {
            match rx.await {
                Ok(result) => result,
                Err(_) => Err("async batch callback was never called".into()),
            }
        }) as BoxFuture<'static, Result<Arc<dyn Value>, TaskError>>
    });

    Box::into_raw(Box::new(CgTask { inner: task }))
}

/// Create a synchronous iterator-producing batch task.
///
/// # Safety
/// See `cg_task_sync` safety requirements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_sync_iter_batch(
    fn_ptr: CgSyncIterBatchFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgTask {
    let ud = Arc::new(UserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let task = Task::sync_iter_batch(
        move |items: &[Box<dyn Value>], ctx: Arc<TaskContext>| -> Result<ValueIter, TaskError> {
            let temp_values = batch_to_temp_ptrs(items);
            let ptrs: Vec<*const CgValue> =
                temp_values.iter().map(|v| v as *const CgValue).collect();
            let ctx_ptr = ctx_to_cg_ptr(&ctx);
            let mut out: *mut CgValueIter = std::ptr::null_mut();
            let _ = &ud;

            let code = unsafe { fn_ptr(ptrs.as_ptr(), items.len(), ctx_ptr, ud.ptr, &mut out) };

            unsafe { drop_ctx_ptr(ctx_ptr) };
            drop(temp_values);

            if code != CgErrorCode::Ok {
                return Err(error_code_to_task_error(code));
            }
            if out.is_null() {
                return Err("sync_iter_batch task returned null iterator".into());
            }
            Ok(unsafe { crate::iterator::into_value_iter(out) })
        },
    );

    Box::into_raw(Box::new(CgTask { inner: task }))
}

/// Create an async stream-producing batch task.
///
/// # Safety
/// See `cg_task_sync` safety requirements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_async_stream_batch(
    fn_ptr: CgAsyncStreamBatchFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgTask {
    let ud = Arc::new(UserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let task = Task::async_stream_batch(
        move |items: &[Box<dyn Value>], ctx: Arc<TaskContext>| -> Result<ValueStream, TaskError> {
            let temp_values = batch_to_temp_ptrs(items);
            let ptrs: Vec<*const CgValue> =
                temp_values.iter().map(|v| v as *const CgValue).collect();
            let ctx_ptr = ctx_to_cg_ptr(&ctx);
            let ud = Arc::clone(&ud);

            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Box<dyn Value>>();
            let (complete_tx, _complete_rx) =
                tokio::sync::oneshot::channel::<Result<(), TaskError>>();

            let stream_data = Box::into_raw(Box::new(StreamCallbackData {
                item_tx: tx,
                complete_tx: Some(complete_tx),
            })) as *mut c_void;

            unsafe {
                fn_ptr(
                    ptrs.as_ptr(),
                    ptrs.len(),
                    ctx_ptr,
                    ud.ptr,
                    stream_yield_trampoline,
                    stream_complete_trampoline,
                    stream_data,
                );
                drop_ctx_ptr(ctx_ptr);
            }
            drop(temp_values);

            let stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx)
                .map(|v| v as Box<dyn Value>);

            Ok(Box::pin(stream) as ValueStream)
        },
    );

    Box::into_raw(Box::new(CgTask { inner: task }))
}

// ---------------------------------------------------------------------------
// Destructor
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_destroy(t: *mut CgTask) {
    if !t.is_null() {
        unsafe { drop(Box::from_raw(t)) };
    }
}
