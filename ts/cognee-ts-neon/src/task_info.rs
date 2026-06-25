use neon::prelude::*;

use cognee_core::TaskInfo;

use crate::task::NeonTask;

/// Wrapper around `TaskInfo` stored in `JsBox`.
pub struct NeonTaskInfo {
    pub inner: TaskInfo,
}

impl Finalize for NeonTaskInfo {}

/// Try to read an optional string property from a JS object.
fn get_opt_string<'cx>(
    cx: &mut impl Context<'cx>,
    obj: &Handle<'cx, JsObject>,
    key: &str,
) -> Option<String> {
    let val = obj.get_value(cx, key).ok()?;
    val.downcast::<JsString, _>(cx).ok().map(|s| s.value(cx))
}

/// Try to read an optional number property from a JS object.
fn get_opt_number<'cx>(
    cx: &mut impl Context<'cx>,
    obj: &Handle<'cx, JsObject>,
    key: &str,
) -> Option<f64> {
    let val = obj.get_value(cx, key).ok()?;
    val.downcast::<JsNumber, _>(cx).ok().map(|n| n.value(cx))
}

/// Create a `TaskInfo` from a `NeonTask` and an optional options object.
///
/// Options: `{ name?: string, batchSize?: number, weight?: number, summaryTemplate?: string }`
pub fn task_info_new(mut cx: FunctionContext) -> JsResult<JsBox<NeonTaskInfo>> {
    let task_box = cx.argument::<JsBox<NeonTask>>(0)?;
    let task = clone_task(&task_box.inner);
    let mut info = TaskInfo::new(task);

    // Read optional options object (argument 1).
    if let Some(arg1) = cx.argument_opt(1)
        && let Ok(opts) = arg1.downcast::<JsObject, _>(&mut cx)
    {
        if let Some(name) = get_opt_string(&mut cx, &opts, "name") {
            info.name = Some(name);
        }
        if let Some(bs) = get_opt_number(&mut cx, &opts, "batchSize") {
            info.batch_size = Some(bs as usize);
        }
        if let Some(w) = get_opt_number(&mut cx, &opts, "weight") {
            info.weight = w as u32;
        }
        if let Some(tmpl) = get_opt_string(&mut cx, &opts, "summaryTemplate") {
            info.summary_template = Some(tmpl);
        }
    }

    Ok(cx.boxed(NeonTaskInfo { inner: info }))
}

/// Clone a Task by cloning the inner Arc for each variant.
pub(crate) fn clone_task(task: &cognee_core::Task) -> cognee_core::Task {
    use cognee_core::Task;
    match task {
        Task::Sync(f) => Task::Sync(f.clone()),
        Task::Async(f) => Task::Async(f.clone()),
        Task::SyncIter(f) => Task::SyncIter(f.clone()),
        Task::AsyncStream(f) => Task::AsyncStream(f.clone()),
        Task::SyncBatch(f) => Task::SyncBatch(f.clone()),
        Task::AsyncBatch(f) => Task::AsyncBatch(f.clone()),
        Task::SyncIterBatch(f) => Task::SyncIterBatch(f.clone()),
        Task::AsyncStreamBatch(f) => Task::AsyncStreamBatch(f.clone()),
    }
}
