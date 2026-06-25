use cognee_core::pipeline::PipelineRunHandle;

pub struct CgPipelineRunHandle {
    pub(crate) inner: Option<PipelineRunHandle>,
}

/// # Safety
/// `h` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_handle_is_finished(h: *const CgPipelineRunHandle) -> bool {
    if h.is_null() {
        return true;
    }
    match unsafe { &(*h).inner } {
        Some(handle) => handle.is_finished(),
        None => true,
    }
}

/// # Safety
/// `h` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_handle_abort(h: *mut CgPipelineRunHandle) {
    if h.is_null() {
        return;
    }
    if let Some(handle) = unsafe { &(*h).inner } {
        handle.abort();
    }
}

/// # Safety
/// `h` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_handle_destroy(h: *mut CgPipelineRunHandle) {
    if !h.is_null() {
        unsafe { drop(Box::from_raw(h)) };
    }
}
