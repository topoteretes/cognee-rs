use cognee_core::pipeline::PipelineRunHandle;

pub struct CgPipelineRunHandle {
    pub(crate) inner: Option<PipelineRunHandle>,
}

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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_handle_abort(h: *mut CgPipelineRunHandle) {
    if h.is_null() {
        return;
    }
    if let Some(handle) = unsafe { &(*h).inner } {
        handle.abort();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_run_handle_destroy(h: *mut CgPipelineRunHandle) {
    if !h.is_null() {
        unsafe { drop(Box::from_raw(h)) };
    }
}
