use std::ffi::{c_char, c_void};

use async_trait::async_trait;
use cognee_core::pipeline::{
    NoopWatcher, PipelineRunInfo, PipelineStatus, PipelineWatcher, TaskStatus,
};
use uuid::Uuid;

pub struct CgPipelineWatcher {
    pub(crate) inner: Box<dyn PipelineWatcher>,
}

/// C-side watcher vtable. All callbacks are synchronous and must not block.
/// NULL function pointers are treated as no-ops.
#[repr(C)]
pub struct CgPipelineWatcherVtable {
    /// Called on pipeline-level status changes.
    /// `status_tag`: 0=Started, 1=Succeeded, 2=Failed, 3=Cancelled, 4=ItemSkipped
    /// `detail`: error message or data_id (null if N/A)
    pub on_pipeline: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            pipeline_id: *const c_char,
            status_tag: i32,
            count_or_index: usize,
            detail: *const c_char,
        ),
    >,

    /// Called on per-task status changes.
    /// `status_tag`: 0=Started, 1=Retrying, 2=Succeeded, 3=Failed
    pub on_task: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            pipeline_id: *const c_char,
            task_index: usize,
            task_name: *const c_char,
            total_tasks: usize,
            status_tag: i32,
            attempts: u32,
            detail: *const c_char,
        ),
    >,

    pub on_run_started: Option<
        unsafe extern "C" fn(state: *mut c_void, run_id: *const c_char, name: *const c_char),
    >,
    pub on_run_completed: Option<
        unsafe extern "C" fn(state: *mut c_void, run_id: *const c_char, output_count: usize),
    >,
    pub on_run_errored: Option<
        unsafe extern "C" fn(state: *mut c_void, run_id: *const c_char, error: *const c_char),
    >,
    pub on_task_started: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            run_id: *const c_char,
            task_name: *const c_char,
            index: usize,
        ),
    >,
    pub on_task_completed: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            run_id: *const c_char,
            task_name: *const c_char,
            count: usize,
        ),
    >,
    pub on_task_errored: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            run_id: *const c_char,
            task_name: *const c_char,
            error: *const c_char,
        ),
    >,
    pub destroy: Option<unsafe extern "C" fn(state: *mut c_void)>,
}

struct VtableWatcher {
    state: *mut c_void,
    vtable: CgPipelineWatcherVtable,
}

unsafe impl Send for VtableWatcher {}
unsafe impl Sync for VtableWatcher {}

impl Drop for VtableWatcher {
    fn drop(&mut self) {
        if let Some(dtor) = self.vtable.destroy {
            unsafe { dtor(self.state) };
        }
    }
}

/// Helper to create a temporary CString for FFI. Interior NUL bytes are
/// silently dropped via `cstring_lossy` — same lossy behaviour as the JS and
/// Python bindings. Never panics.
fn to_c(s: &str) -> std::ffi::CString {
    crate::util::cstring_lossy(s)
}

fn uuid_to_c(u: Uuid) -> std::ffi::CString {
    to_c(&u.to_string())
}

#[async_trait]
impl PipelineWatcher for VtableWatcher {
    async fn on_pipeline(&self, pipeline_id: Uuid, status: PipelineStatus) {
        if let Some(f) = self.vtable.on_pipeline {
            let pid = uuid_to_c(pipeline_id);
            let (tag, count_or_index, detail_str) = match &status {
                PipelineStatus::Started { task_count } => (0i32, *task_count, None),
                PipelineStatus::Succeeded { output_count } => (1, *output_count, None),
                PipelineStatus::Failed { task_index, error } => {
                    (2, *task_index, Some(error.as_str()))
                }
                PipelineStatus::Cancelled => (3, 0, None),
                PipelineStatus::ItemSkipped { data_id } => (4, 0, Some(data_id.as_str())),
            };
            let detail_c = detail_str.map(to_c);
            let detail_ptr = detail_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            unsafe { f(self.state, pid.as_ptr(), tag, count_or_index, detail_ptr) };
        }
    }

    async fn on_task(
        &self,
        pipeline_id: Uuid,
        task_index: usize,
        task_name: Option<&str>,
        total_tasks: usize,
        status: TaskStatus,
    ) {
        if let Some(f) = self.vtable.on_task {
            let pid = uuid_to_c(pipeline_id);
            let name_c = task_name.map(to_c);
            let name_ptr = name_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            let (tag, attempts, detail_str) = match &status {
                TaskStatus::Started => (0i32, 0u32, None),
                TaskStatus::Retrying { attempt, error } => (1, *attempt, Some(error.as_str())),
                TaskStatus::Succeeded => (2, 0, None),
                TaskStatus::Failed { attempts, error } => (3, *attempts, Some(error.as_str())),
            };
            let detail_c = detail_str.map(to_c);
            let detail_ptr = detail_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            unsafe {
                f(
                    self.state,
                    pid.as_ptr(),
                    task_index,
                    name_ptr,
                    total_tasks,
                    tag,
                    attempts,
                    detail_ptr,
                )
            };
        }
    }

    async fn on_pipeline_run_started(&self, run: &PipelineRunInfo) {
        if let Some(f) = self.vtable.on_run_started {
            let rid = uuid_to_c(run.run_id);
            let name = to_c(&run.pipeline_name);
            unsafe { f(self.state, rid.as_ptr(), name.as_ptr()) };
        }
    }

    async fn on_pipeline_run_completed(&self, run: &PipelineRunInfo, output_count: usize) {
        if let Some(f) = self.vtable.on_run_completed {
            let rid = uuid_to_c(run.run_id);
            unsafe { f(self.state, rid.as_ptr(), output_count) };
        }
    }

    async fn on_pipeline_run_errored(&self, run: &PipelineRunInfo, error: &str) {
        if let Some(f) = self.vtable.on_run_errored {
            let rid = uuid_to_c(run.run_id);
            let err = to_c(error);
            unsafe { f(self.state, rid.as_ptr(), err.as_ptr()) };
        }
    }

    async fn on_task_started(&self, run: &PipelineRunInfo, task_name: &str, task_index: usize) {
        if let Some(f) = self.vtable.on_task_started {
            let rid = uuid_to_c(run.run_id);
            let tn = to_c(task_name);
            unsafe { f(self.state, rid.as_ptr(), tn.as_ptr(), task_index) };
        }
    }

    async fn on_task_completed(&self, run: &PipelineRunInfo, task_name: &str, result_count: usize) {
        if let Some(f) = self.vtable.on_task_completed {
            let rid = uuid_to_c(run.run_id);
            let tn = to_c(task_name);
            unsafe { f(self.state, rid.as_ptr(), tn.as_ptr(), result_count) };
        }
    }

    async fn on_task_errored(&self, run: &PipelineRunInfo, task_name: &str, error: &str) {
        if let Some(f) = self.vtable.on_task_errored {
            let rid = uuid_to_c(run.run_id);
            let tn = to_c(task_name);
            let err = to_c(error);
            unsafe { f(self.state, rid.as_ptr(), tn.as_ptr(), err.as_ptr()) };
        }
    }
}

/// Create a watcher from a C vtable.
///
/// # Safety
/// `state` must be valid until `vtable.destroy` is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_watcher_new(
    state: *mut c_void,
    vtable: CgPipelineWatcherVtable,
) -> *mut CgPipelineWatcher {
    Box::into_raw(Box::new(CgPipelineWatcher {
        inner: Box::new(VtableWatcher { state, vtable }),
    }))
}

/// Create a no-op watcher.
#[unsafe(no_mangle)]
pub extern "C" fn cg_pipeline_watcher_noop() -> *mut CgPipelineWatcher {
    Box::into_raw(Box::new(CgPipelineWatcher {
        inner: Box::new(NoopWatcher),
    }))
}

/// # Safety
/// `w` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_watcher_destroy(w: *mut CgPipelineWatcher) {
    if !w.is_null() {
        unsafe { drop(Box::from_raw(w)) };
    }
}
