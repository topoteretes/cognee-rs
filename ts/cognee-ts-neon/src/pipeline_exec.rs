use std::sync::Arc;

use neon::prelude::*;

use cognee_core::{NoopWatcher, Pipeline, execute, execute_blocking, execute_in_background};

use crate::error::throw_execution_error;
use crate::pipeline::NeonPipeline;
use crate::run_handle::NeonRunHandle;
use crate::task_context::NeonTaskContext;
use crate::value::{js_to_value, value_to_js};
use crate::watcher::NeonWatcher;

/// Helper: extract the inputs JS array into `Vec<Arc<dyn cognee_core::Value>>`.
fn extract_inputs(
    cx: &mut FunctionContext,
    arg_index: usize,
) -> NeonResult<Vec<Arc<dyn cognee_core::Value>>> {
    let arr = cx.argument::<JsArray>(arg_index)?;
    let len = arr.len(cx);
    let mut inputs = Vec::with_capacity(len as usize);
    for i in 0..len {
        let item = arr.get::<JsValue, _, _>(cx, i)?;
        inputs.push(js_to_value(cx, item)?);
    }
    Ok(inputs)
}

/// Helper: snapshot the pipeline out of the Mutex.
fn snapshot_pipeline(pipeline: &NeonPipeline) -> Pipeline {
    let guard = pipeline.inner.lock().unwrap(); // lock poison is unrecoverable
    // We need to clone the Pipeline. Pipeline is not Clone, so we reconstruct.
    let src = &*guard;
    let mut p = Pipeline::new(src.description.clone());
    p.name = src.name.clone();
    p.retry_policy = src.retry_policy.clone();
    p.batch_size = src.batch_size;
    p.concurrency = src.concurrency;
    // Clone tasks (Arc-based, cheap).
    for info in &src.tasks {
        p.tasks.push(cognee_core::TaskInfo {
            task: crate::task_info::clone_task(&info.task),
            name: info.name.clone(),
            batch_size: info.batch_size,
            summary_template: info.summary_template.clone(),
            weight: info.weight,
            enriches: info.enriches,
            rate_limiter: info.rate_limiter.clone(),
        });
    }
    p
}

/// Execute pipeline synchronously (blocking). Returns a Promise.
///
/// `pipelineExecute(pipeline, inputs[], context) -> Promise<value[]>`
///
/// Uses `execute_blocking` which creates its own single-threaded tokio runtime,
/// so it does NOT require `init()` to have been called.
pub fn pipeline_execute(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let pipeline_box = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let inputs = extract_inputs(&mut cx, 1)?;
    let ctx_box = cx.argument::<JsBox<NeonTaskContext>>(2)?;

    let pipeline = snapshot_pipeline(&pipeline_box);
    let ctx = Arc::clone(&ctx_box.inner);
    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    // Run on a separate thread because execute_blocking blocks.
    std::thread::spawn(move || {
        let result = execute_blocking(&pipeline, inputs, ctx, &NoopWatcher);
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(run_result) => {
                let arr = JsArray::new(&mut cx, run_result.outputs.len());
                for (i, val) in run_result.outputs.iter().enumerate() {
                    let js_val = value_to_js(&mut cx, val.as_ref())?;
                    arr.set(&mut cx, i as u32, js_val)?;
                }
                Ok(arr)
            }
            Err(e) => throw_execution_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// Execute pipeline asynchronously. Returns a Promise.
///
/// `pipelineExecuteAsync(pipeline, inputs[], context) -> Promise<value[]>`
///
/// Requires `init()` to have been called (uses the global tokio runtime).
pub fn pipeline_execute_async(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let pipeline_box = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let inputs = extract_inputs(&mut cx, 1)?;
    let ctx_box = cx.argument::<JsBox<NeonTaskContext>>(2)?;

    let pipeline = snapshot_pipeline(&pipeline_box);
    let ctx = Arc::clone(&ctx_box.inner);
    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    let rt = crate::runtime::runtime();
    rt.spawn(async move {
        let result = execute(&pipeline, inputs, ctx, &NoopWatcher).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(outputs) => {
                let arr = JsArray::new(&mut cx, outputs.len());
                for (i, val) in outputs.iter().enumerate() {
                    let js_val = value_to_js(&mut cx, val.as_ref())?;
                    arr.set(&mut cx, i as u32, js_val)?;
                }
                Ok(arr)
            }
            Err(e) => throw_execution_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// Execute pipeline in the background. Returns a RunHandle immediately.
///
/// `pipelineExecuteBackground(pipeline, inputs[], context) -> RunHandle`
///
/// Requires `init()` to have been called.
pub fn pipeline_execute_background(mut cx: FunctionContext) -> JsResult<JsBox<NeonRunHandle>> {
    let pipeline_box = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let inputs = extract_inputs(&mut cx, 1)?;
    let ctx_box = cx.argument::<JsBox<NeonTaskContext>>(2)?;

    let pipeline = Arc::new(snapshot_pipeline(&pipeline_box));
    let ctx = Arc::clone(&ctx_box.inner);

    let rt = crate::runtime::runtime();
    // execute_in_background spawns a tokio task internally, but it needs
    // to be called from within a tokio context.
    let handle =
        rt.block_on(async { execute_in_background(pipeline, inputs, ctx, Arc::new(NoopWatcher)) });

    Ok(cx.boxed(NeonRunHandle {
        inner: std::sync::Mutex::new(Some(handle)),
    }))
}

/// Execute pipeline asynchronously with a watcher. Returns a Promise.
///
/// `pipelineExecuteWithWatcher(pipeline, inputs[], context, watcher) -> Promise<value[]>`
pub fn pipeline_execute_with_watcher(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let pipeline_box = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let inputs = extract_inputs(&mut cx, 1)?;
    let ctx_box = cx.argument::<JsBox<NeonTaskContext>>(2)?;
    let watcher_box = cx.argument::<JsBox<NeonWatcher>>(3)?;

    let pipeline = snapshot_pipeline(&pipeline_box);
    let ctx = Arc::clone(&ctx_box.inner);
    let watcher = Arc::clone(&watcher_box.inner);
    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    let rt = crate::runtime::runtime();
    rt.spawn(async move {
        let result = execute(&pipeline, inputs, ctx, watcher.as_ref()).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(outputs) => {
                let arr = JsArray::new(&mut cx, outputs.len());
                for (i, val) in outputs.iter().enumerate() {
                    let js_val = value_to_js(&mut cx, val.as_ref())?;
                    arr.set(&mut cx, i as u32, js_val)?;
                }
                Ok(arr)
            }
            Err(e) => throw_execution_error(&mut cx, e),
        });
    });

    Ok(promise)
}
