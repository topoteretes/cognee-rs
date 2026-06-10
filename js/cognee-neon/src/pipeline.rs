use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use neon::prelude::*;

use cognee_core::{Pipeline, RetryDelay, RetryPolicy};

use crate::task_info::NeonTaskInfo;

/// Wrapper around `Pipeline` stored in `JsBox`.
///
/// Uses `Arc<Mutex<Pipeline>>` so the pipeline can be shared with execution
/// functions that need `Arc<Pipeline>` for background execution.
pub struct NeonPipeline {
    pub inner: Arc<Mutex<Pipeline>>,
}

impl Finalize for NeonPipeline {}

pub fn pipeline_new(mut cx: FunctionContext) -> JsResult<JsBox<NeonPipeline>> {
    let desc = cx
        .argument_opt(0)
        .and_then(|v| v.downcast::<JsString, _>(&mut cx).ok())
        .map(|s| s.value(&mut cx))
        .unwrap_or_default();

    Ok(cx.boxed(NeonPipeline {
        inner: Arc::new(Mutex::new(Pipeline::new(desc))),
    }))
}

pub fn pipeline_set_name(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let pipeline = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let name = cx.argument::<JsString>(1)?.value(&mut cx);
    pipeline.inner.lock().unwrap().name = Some(name);
    Ok(cx.undefined())
}

pub fn pipeline_add_task(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let pipeline = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let task_info_box = cx.argument::<JsBox<NeonTaskInfo>>(1)?;

    // Clone the TaskInfo (Task variants are Arc-based, so cloning is cheap).
    let info = clone_task_info(&task_info_box.inner);
    pipeline.inner.lock().unwrap().tasks.push(info);
    Ok(cx.undefined())
}

pub fn pipeline_set_batch_size(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let pipeline = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let size = cx.argument::<JsNumber>(1)?.value(&mut cx) as usize;
    if size == 0 {
        return cx.throw_range_error("batch_size must be > 0");
    }
    pipeline.inner.lock().unwrap().batch_size = size;
    Ok(cx.undefined())
}

pub fn pipeline_set_concurrency(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let pipeline = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let n = cx.argument::<JsNumber>(1)?.value(&mut cx) as usize;
    if n == 0 {
        return cx.throw_range_error("concurrency must be > 0");
    }
    pipeline.inner.lock().unwrap().concurrency = n;
    Ok(cx.undefined())
}

/// Set retry policy from a JS object.
///
/// `{ type: 'none' }` or
/// `{ type: 'limited', maxAttempts: number, delay: { type: 'constant', ms: number } | { type: 'exponential', baseMs: number, factor?: number } }`
pub fn pipeline_set_retry(mut cx: FunctionContext) -> JsResult<JsUndefined> {
    let pipeline = cx.argument::<JsBox<NeonPipeline>>(0)?;
    let opts = cx.argument::<JsObject>(1)?;

    let retry_type = opts.get::<JsString, _, _>(&mut cx, "type")?.value(&mut cx);

    let policy = match retry_type.as_str() {
        "none" => RetryPolicy::NoRetry,
        "limited" => {
            let max_attempts = opts
                .get::<JsNumber, _, _>(&mut cx, "maxAttempts")?
                .value(&mut cx) as u32;
            let max_attempts = NonZeroU32::new(max_attempts)
                .ok_or(())
                .or_else(|_| cx.throw_range_error("maxAttempts must be > 0"))?;

            let delay_obj = opts.get::<JsObject, _, _>(&mut cx, "delay")?;
            let delay_type = delay_obj
                .get::<JsString, _, _>(&mut cx, "type")?
                .value(&mut cx);

            let delay = match delay_type.as_str() {
                "constant" => {
                    let ms = delay_obj
                        .get::<JsNumber, _, _>(&mut cx, "ms")?
                        .value(&mut cx) as u64;
                    RetryDelay::Constant(Duration::from_millis(ms))
                }
                "exponential" => {
                    let base_ms = delay_obj
                        .get::<JsNumber, _, _>(&mut cx, "baseMs")?
                        .value(&mut cx) as u64;
                    let factor = delay_obj
                        .get_opt::<JsNumber, _, _>(&mut cx, "factor")?
                        .map(|n| n.value(&mut cx) as u32)
                        .unwrap_or(2);
                    RetryDelay::Exponential {
                        base: Duration::from_millis(base_ms),
                        factor,
                    }
                }
                other => {
                    return cx.throw_type_error(format!("unknown delay type: {other}"));
                }
            };

            RetryPolicy::Limited {
                max_attempts,
                delay,
            }
        }
        other => {
            return cx.throw_type_error(format!("unknown retry type: {other}"));
        }
    };

    pipeline.inner.lock().unwrap().retry_policy = policy;
    Ok(cx.undefined())
}

/// Clone a TaskInfo (Task's inner Arcs are cheap to clone).
fn clone_task_info(info: &cognee_core::TaskInfo) -> cognee_core::TaskInfo {
    cognee_core::TaskInfo {
        task: crate::task_info::clone_task(&info.task),
        name: info.name.clone(),
        batch_size: info.batch_size,
        summary_template: info.summary_template.clone(),
        weight: info.weight,
        enriches: info.enriches,
    }
}
