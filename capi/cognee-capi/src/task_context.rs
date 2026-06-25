use std::sync::Arc;

use cognee_core::{RayonThreadPool, TaskContext, TaskContextBuilder};

use crate::cancellation::CgCancellationHandle;
use crate::error::{CgErrorCode, core_error_to_code, set_last_error};
use crate::util::null_check;

/// Opaque handle wrapping `Arc<TaskContext>`.
pub struct CgTaskContext {
    pub(crate) inner: Arc<TaskContext>,
}

/// Create a mock task context with in-memory databases. Useful for examples
/// and testing.
///
/// # Safety
/// `handle_out` and `ctx_out` must be valid, non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_context_mock(
    handle_out: *mut *mut CgCancellationHandle,
    ctx_out: *mut *mut CgTaskContext,
) -> CgErrorCode {
    null_check!(handle_out);
    null_check!(ctx_out);

    let pool = match RayonThreadPool::with_default_threads() {
        Ok(p) => p,
        Err(e) => {
            set_last_error(e.to_string());
            return core_error_to_code(&e);
        }
    };

    let db = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .and_then(|rt| {
            rt.block_on(async {
                let db = cognee_database::connect("sqlite::memory:")
                    .await
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                cognee_database::initialize(&db)
                    .await
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                Ok::<_, std::io::Error>(db)
            })
        }) {
        Ok(db) => db,
        Err(e) => {
            set_last_error(e.to_string());
            return CgErrorCode::RuntimeError;
        }
    };

    let result = TaskContextBuilder::new()
        .thread_pool(Arc::new(pool))
        .database(Arc::new(db))
        .graph_db(Arc::new(cognee_graph::MockGraphDB::new()))
        .vector_db(Arc::new(cognee_vector::MockVectorDB::new()))
        .build();

    match result {
        Ok((handle, ctx)) => {
            unsafe {
                *handle_out = Box::into_raw(Box::new(CgCancellationHandle { inner: handle }));
                *ctx_out = Box::into_raw(Box::new(CgTaskContext {
                    inner: Arc::new(ctx),
                }));
            }
            CgErrorCode::Ok
        }
        Err(e) => {
            set_last_error(e.to_string());
            core_error_to_code(&e)
        }
    }
}

/// Clone (increment refcount) a task context.
///
/// # Safety
/// `ctx` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_context_clone(ctx: *const CgTaskContext) -> *mut CgTaskContext {
    if ctx.is_null() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(CgTaskContext {
        inner: Arc::clone(unsafe { &(*ctx).inner }),
    }))
}

/// Destroy a task context handle.
///
/// # Safety
/// `ctx` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_context_destroy(ctx: *mut CgTaskContext) {
    if !ctx.is_null() {
        unsafe { drop(Box::from_raw(ctx)) };
    }
}
