use std::sync::Arc;

use async_trait::async_trait;
use neon::prelude::*;
use uuid::Uuid;

use cognee_core::{NoopWatcher, PipelineRunInfo, PipelineStatus, PipelineWatcher, TaskStatus};

/// Opaque watcher wrapper stored in `JsBox`.
pub struct NeonWatcher {
    pub inner: Arc<dyn PipelineWatcher>,
}

impl Finalize for NeonWatcher {}

/// Watcher that forwards events to a JS object via a Neon `Channel`.
struct JsWatcherImpl {
    js_obj: Arc<Root<JsObject>>,
    channel: Channel,
}

// Root<JsObject> is Send; we add Sync for PipelineWatcher.
unsafe impl Sync for JsWatcherImpl {}

#[async_trait]
impl PipelineWatcher for JsWatcherImpl {
    async fn on_pipeline(&self, _pipeline_id: Uuid, _status: PipelineStatus) {}

    async fn on_task(
        &self,
        _pipeline_id: Uuid,
        _task_index: usize,
        _task_name: Option<&str>,
        _total_tasks: usize,
        _status: TaskStatus,
    ) {
    }

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        let run_id = run.run_id.to_string();
        let name = run.pipeline_name.clone();
        let js_obj = Arc::clone(&self.js_obj);
        self.channel.send(move |mut cx| {
            let obj = js_obj.to_inner(&mut cx);
            if let Ok(cb) = obj.get::<JsFunction, _, _>(&mut cx, "onPipelineRunStarted") {
                let run_id = cx.string(&run_id);
                let name = cx.string(&name);
                let _ = cb.call_with(&cx).arg(run_id).arg(name).exec(&mut cx);
            }
            Ok(())
        });
    }

    async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, output_count: usize) {
        let run_id = run.run_id.to_string();
        let count = output_count;
        let js_obj = Arc::clone(&self.js_obj);
        self.channel.send(move |mut cx| {
            let obj = js_obj.to_inner(&mut cx);
            if let Ok(cb) = obj.get::<JsFunction, _, _>(&mut cx, "onPipelineRunCompleted") {
                let run_id = cx.string(&run_id);
                let count = cx.number(count as f64);
                let _ = cb.call_with(&cx).arg(run_id).arg(count).exec(&mut cx);
            }
            Ok(())
        });
    }

    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
        let run_id = run.run_id.to_string();
        let error = error.to_string();
        let js_obj = Arc::clone(&self.js_obj);
        self.channel.send(move |mut cx| {
            let obj = js_obj.to_inner(&mut cx);
            if let Ok(cb) = obj.get::<JsFunction, _, _>(&mut cx, "onPipelineRunErrored") {
                let run_id = cx.string(&run_id);
                let err = cx.string(&error);
                let _ = cb.call_with(&cx).arg(run_id).arg(err).exec(&mut cx);
            }
            Ok(())
        });
    }

    async fn on_task_started(&self, run: &PipelineRunInfo, task_name: &str, task_index: usize) {
        let run_id = run.run_id.to_string();
        let name = task_name.to_string();
        let index = task_index;
        let js_obj = Arc::clone(&self.js_obj);
        self.channel.send(move |mut cx| {
            let obj = js_obj.to_inner(&mut cx);
            if let Ok(cb) = obj.get::<JsFunction, _, _>(&mut cx, "onTaskStarted") {
                let run_id = cx.string(&run_id);
                let name = cx.string(&name);
                let idx = cx.number(index as f64);
                let _ = cb
                    .call_with(&cx)
                    .arg(run_id)
                    .arg(name)
                    .arg(idx)
                    .exec(&mut cx);
            }
            Ok(())
        });
    }

    async fn on_task_completed(&self, run: &PipelineRunInfo, task_name: &str, result_count: usize) {
        let run_id = run.run_id.to_string();
        let name = task_name.to_string();
        let count = result_count;
        let js_obj = Arc::clone(&self.js_obj);
        self.channel.send(move |mut cx| {
            let obj = js_obj.to_inner(&mut cx);
            if let Ok(cb) = obj.get::<JsFunction, _, _>(&mut cx, "onTaskCompleted") {
                let run_id = cx.string(&run_id);
                let name = cx.string(&name);
                let count = cx.number(count as f64);
                let _ = cb
                    .call_with(&cx)
                    .arg(run_id)
                    .arg(name)
                    .arg(count)
                    .exec(&mut cx);
            }
            Ok(())
        });
    }

    async fn on_task_errored(&self, run: &PipelineRunInfo, task_name: &str, error: &str) {
        let run_id = run.run_id.to_string();
        let name = task_name.to_string();
        let error = error.to_string();
        let js_obj = Arc::clone(&self.js_obj);
        self.channel.send(move |mut cx| {
            let obj = js_obj.to_inner(&mut cx);
            if let Ok(cb) = obj.get::<JsFunction, _, _>(&mut cx, "onTaskErrored") {
                let run_id = cx.string(&run_id);
                let name = cx.string(&name);
                let err = cx.string(&error);
                let _ = cb
                    .call_with(&cx)
                    .arg(run_id)
                    .arg(name)
                    .arg(err)
                    .exec(&mut cx);
            }
            Ok(())
        });
    }
}

/// Create a watcher from a JS object with optional callback methods.
pub fn watcher_new(mut cx: FunctionContext) -> JsResult<JsBox<NeonWatcher>> {
    let js_obj = cx.argument::<JsObject>(0)?.root(&mut cx);
    let channel = cx.channel();

    let watcher = JsWatcherImpl {
        js_obj: Arc::new(js_obj),
        channel,
    };

    Ok(cx.boxed(NeonWatcher {
        inner: Arc::new(watcher),
    }))
}

/// Create a no-op watcher.
pub fn watcher_noop(mut cx: FunctionContext) -> JsResult<JsBox<NeonWatcher>> {
    Ok(cx.boxed(NeonWatcher {
        inner: Arc::new(NoopWatcher),
    }))
}
