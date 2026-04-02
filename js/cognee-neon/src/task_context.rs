use std::sync::Arc;

use neon::prelude::*;

pub use cognee_core::TaskContext as CogneeTaskContext;
use cognee_core::{RayonThreadPool, TaskContextBuilder};
use cognee_database::MockDatabase;
use cognee_graph::MockGraphDB;
use cognee_vector::MockVectorDB;

use crate::cancellation::NeonCancellationHandle;
use crate::error::throw_core_error;

/// Opaque wrapper around `Arc<CogneeTaskContext>`.
pub struct NeonTaskContext {
    pub inner: Arc<CogneeTaskContext>,
}

impl Finalize for NeonTaskContext {}

/// Create a mock task context with in-memory backends (for testing).
///
/// Returns a JS object: `{ handle: CancellationHandle, context: TaskContext }`
pub fn task_context_mock(mut cx: FunctionContext) -> JsResult<JsObject> {
    let pool = Arc::new(
        RayonThreadPool::with_default_threads().or_else(|e| throw_core_error(&mut cx, e))?,
    );

    let (handle, ctx) = TaskContextBuilder::new()
        .thread_pool(pool)
        .database(Arc::new(MockDatabase::new()))
        .graph_db(Arc::new(MockGraphDB::new()))
        .vector_db(Arc::new(MockVectorDB::new()))
        .build()
        .or_else(|e| throw_core_error(&mut cx, e))?;

    let result = cx.empty_object();
    let js_handle = cx.boxed(NeonCancellationHandle { inner: handle });
    let js_ctx = cx.boxed(NeonTaskContext {
        inner: Arc::new(ctx),
    });
    result.set(&mut cx, "handle", js_handle)?;
    result.set(&mut cx, "context", js_ctx)?;
    Ok(result)
}

/// Clone a task context (Arc bump).
pub fn task_context_clone(mut cx: FunctionContext) -> JsResult<JsBox<NeonTaskContext>> {
    let ctx = cx.argument::<JsBox<NeonTaskContext>>(0)?;
    Ok(cx.boxed(NeonTaskContext {
        inner: Arc::clone(&ctx.inner),
    }))
}
