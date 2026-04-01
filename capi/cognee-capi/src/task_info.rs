use std::ffi::c_char;

use cognee_core::TaskInfo;

use crate::task::CgTask;

/// Opaque handle wrapping `TaskInfo`.
pub struct CgTaskInfo {
    pub(crate) inner: TaskInfo,
}

/// Create a new `CgTaskInfo` from a `CgTask`. Takes ownership of the task.
///
/// # Safety
/// `task` must be a valid pointer created by a `cg_task_*` constructor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_info_new(task: *mut CgTask) -> *mut CgTaskInfo {
    if task.is_null() {
        return std::ptr::null_mut();
    }
    let task = unsafe { Box::from_raw(task) };
    Box::into_raw(Box::new(CgTaskInfo {
        inner: TaskInfo::new(task.inner),
    }))
}

/// Set a human-readable name on the task info.
///
/// # Safety
/// `info` must be valid. `name` must be a null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_info_set_name(info: *mut CgTaskInfo, name: *const c_char) {
    if info.is_null() || name.is_null() {
        return;
    }
    if let Ok(s) = unsafe { crate::util::c_str_to_str(name) } {
        unsafe { (*info).inner.name = Some(s.to_owned()) };
    }
}

/// Set batch size override on the task info.
///
/// # Safety
/// `info` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_info_set_batch_size(info: *mut CgTaskInfo, size: usize) {
    if info.is_null() || size == 0 {
        return;
    }
    unsafe { (*info).inner.batch_size = Some(size) };
}

/// Set the progress weight.
///
/// # Safety
/// `info` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_info_set_weight(info: *mut CgTaskInfo, weight: u32) {
    if info.is_null() {
        return;
    }
    unsafe { (*info).inner.weight = weight };
}

/// Set a summary template (e.g. "Processed {n} items").
///
/// # Safety
/// `info` must be valid. `tmpl` must be a null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_info_set_summary(info: *mut CgTaskInfo, tmpl: *const c_char) {
    if info.is_null() || tmpl.is_null() {
        return;
    }
    if let Ok(s) = unsafe { crate::util::c_str_to_str(tmpl) } {
        unsafe { (*info).inner.summary_template = Some(s.to_owned()) };
    }
}

/// Destroy a task info handle.
///
/// # Safety
/// `info` must have been created by `cg_task_info_new`, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_task_info_destroy(info: *mut CgTaskInfo) {
    if !info.is_null() {
        unsafe { drop(Box::from_raw(info)) };
    }
}
