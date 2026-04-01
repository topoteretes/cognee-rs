use std::ffi::c_void;

use async_trait::async_trait;
use cognee_core::task::TaskError;
use cognee_core::{ExecStatusManager, NoopExecStatusManager};
use uuid::Uuid;

pub struct CgExecStatusManager {
    pub(crate) inner: Box<dyn ExecStatusManager>,
}

/// Create a no-op exec status manager (processes everything, no dedup).
#[unsafe(no_mangle)]
pub extern "C" fn cg_exec_status_noop() -> *mut CgExecStatusManager {
    Box::into_raw(Box::new(CgExecStatusManager {
        inner: Box::new(NoopExecStatusManager),
    }))
}

/// C-side vtable for a custom exec status manager.
///
/// All callbacks are synchronous. UUID pointers are 16-byte arrays (null = None).
#[repr(C)]
pub struct CgExecStatusManagerVtable {
    pub is_completed: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            data_id: *const std::ffi::c_char,
            pipeline_name: *const std::ffi::c_char,
            dataset_id: *const u8, // 16 bytes or null
        ) -> bool,
    >,
    pub mark_completed: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            data_id: *const std::ffi::c_char,
            pipeline_name: *const std::ffi::c_char,
            dataset_id: *const u8,
        ),
    >,
    pub mark_failed: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            data_id: *const std::ffi::c_char,
            pipeline_name: *const std::ffi::c_char,
            dataset_id: *const u8,
            error: *const std::ffi::c_char,
        ),
    >,
    pub stamp_provenance: Option<
        unsafe extern "C" fn(
            state: *mut c_void,
            data_id: *const std::ffi::c_char,
            pipeline_name: *const std::ffi::c_char,
            task_name: *const std::ffi::c_char,
            user_id: *const u8,
            node_set: *const std::ffi::c_char,
        ),
    >,
    pub destroy: Option<unsafe extern "C" fn(state: *mut c_void)>,
}

struct VtableExecStatus {
    state: *mut c_void,
    vtable: CgExecStatusManagerVtable,
}

unsafe impl Send for VtableExecStatus {}
unsafe impl Sync for VtableExecStatus {}

impl Drop for VtableExecStatus {
    fn drop(&mut self) {
        if let Some(dtor) = self.vtable.destroy {
            unsafe { dtor(self.state) };
        }
    }
}

fn uuid_to_bytes_ptr(uuid: Option<Uuid>) -> (*const u8, Option<[u8; 16]>) {
    match uuid {
        Some(u) => {
            let bytes = *u.as_bytes();
            (bytes.as_ptr(), Some(bytes))
        }
        None => (std::ptr::null(), None),
    }
}

#[async_trait]
impl ExecStatusManager for VtableExecStatus {
    async fn is_completed(
        &self,
        data_id: &str,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
    ) -> Result<bool, TaskError> {
        if let Some(f) = self.vtable.is_completed {
            let did = std::ffi::CString::new(data_id).unwrap();
            let pn = std::ffi::CString::new(pipeline_name).unwrap();
            let (ds_ptr, _ds_bytes) = uuid_to_bytes_ptr(dataset_id);
            Ok(unsafe { f(self.state, did.as_ptr(), pn.as_ptr(), ds_ptr) })
        } else {
            Ok(false)
        }
    }

    async fn mark_completed(
        &self,
        data_id: &str,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
    ) -> Result<(), TaskError> {
        if let Some(f) = self.vtable.mark_completed {
            let did = std::ffi::CString::new(data_id).unwrap();
            let pn = std::ffi::CString::new(pipeline_name).unwrap();
            let (ds_ptr, _ds_bytes) = uuid_to_bytes_ptr(dataset_id);
            unsafe { f(self.state, did.as_ptr(), pn.as_ptr(), ds_ptr) };
        }
        Ok(())
    }

    async fn mark_failed(
        &self,
        data_id: &str,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
        error: &str,
    ) -> Result<(), TaskError> {
        if let Some(f) = self.vtable.mark_failed {
            let did = std::ffi::CString::new(data_id).unwrap();
            let pn = std::ffi::CString::new(pipeline_name).unwrap();
            let err = std::ffi::CString::new(error).unwrap();
            let (ds_ptr, _ds_bytes) = uuid_to_bytes_ptr(dataset_id);
            unsafe { f(self.state, did.as_ptr(), pn.as_ptr(), ds_ptr, err.as_ptr()) };
        }
        Ok(())
    }

    async fn stamp_provenance(
        &self,
        data_id: &str,
        pipeline_name: &str,
        task_name: &str,
        user_id: Option<Uuid>,
        node_set: Option<&str>,
    ) -> Result<(), TaskError> {
        if let Some(f) = self.vtable.stamp_provenance {
            let did = std::ffi::CString::new(data_id).unwrap();
            let pn = std::ffi::CString::new(pipeline_name).unwrap();
            let tn = std::ffi::CString::new(task_name).unwrap();
            let (uid_ptr, _uid_bytes) = uuid_to_bytes_ptr(user_id);
            let ns_c = node_set.map(|s| std::ffi::CString::new(s).unwrap());
            let ns_ptr = ns_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
            unsafe {
                f(
                    self.state,
                    did.as_ptr(),
                    pn.as_ptr(),
                    tn.as_ptr(),
                    uid_ptr,
                    ns_ptr,
                )
            };
        }
        Ok(())
    }
}

/// Create a custom exec status manager from a C vtable.
///
/// # Safety
/// `state` must be valid until `vtable.destroy` is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_exec_status_new(
    state: *mut c_void,
    vtable: CgExecStatusManagerVtable,
) -> *mut CgExecStatusManager {
    Box::into_raw(Box::new(CgExecStatusManager {
        inner: Box::new(VtableExecStatus { state, vtable }),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_exec_status_destroy(mgr: *mut CgExecStatusManager) {
    if !mgr.is_null() {
        unsafe { drop(Box::from_raw(mgr)) };
    }
}
