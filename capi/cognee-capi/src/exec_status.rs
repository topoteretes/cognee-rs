use std::ffi::c_void;

use async_trait::async_trait;
use cognee_core::task::TaskError;
use cognee_core::{ExecStatusManager, NoopExecStatusManager};
use uuid::Uuid;

pub struct CgExecStatusManager {
    #[allow(dead_code)]
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
            let did = crate::util::cstring_lossy(data_id);
            let pn = crate::util::cstring_lossy(pipeline_name);
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
            let did = crate::util::cstring_lossy(data_id);
            let pn = crate::util::cstring_lossy(pipeline_name);
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
            let did = crate::util::cstring_lossy(data_id);
            let pn = crate::util::cstring_lossy(pipeline_name);
            let err = crate::util::cstring_lossy(error);
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
            let did = crate::util::cstring_lossy(data_id);
            let pn = crate::util::cstring_lossy(pipeline_name);
            let tn = crate::util::cstring_lossy(task_name);
            let (uid_ptr, _uid_bytes) = uuid_to_bytes_ptr(user_id);
            let ns_c = node_set.map(crate::util::cstring_lossy);
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

/// # Safety
/// `mgr` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_exec_status_destroy(mgr: *mut CgExecStatusManager) {
    if !mgr.is_null() {
        unsafe { drop(Box::from_raw(mgr)) };
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Verify that trait methods do not panic when passed strings containing
    /// interior NUL bytes (e.g. an error message from a pipeline engine that
    /// embeds binary data). Before the fix these would call
    /// `CString::new(s).unwrap()` and panic.
    #[tokio::test]
    async fn interior_nul_does_not_panic() {
        // Shared state: collects the strings the C callback receives.
        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        unsafe extern "C" fn on_mark_failed(
            state: *mut c_void,
            data_id: *const std::ffi::c_char,
            _pipeline_name: *const std::ffi::c_char,
            _dataset_id: *const u8,
            error: *const std::ffi::c_char,
        ) {
            let collector = unsafe { &*(state as *const Mutex<Vec<String>>) };
            let did = unsafe { std::ffi::CStr::from_ptr(data_id) }
                .to_string_lossy()
                .into_owned();
            let err = unsafe { std::ffi::CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            collector.lock().unwrap().push(did);
            collector.lock().unwrap().push(err);
        }

        let vtable = CgExecStatusManagerVtable {
            is_completed: None,
            mark_completed: None,
            mark_failed: Some(on_mark_failed),
            stamp_provenance: None,
            destroy: None,
        };

        let state_ptr = Arc::as_ptr(&received_clone) as *mut c_void;
        let mgr = VtableExecStatus {
            state: state_ptr,
            vtable,
        };

        // "a\0b" has an interior NUL — this must not panic.
        mgr.mark_failed("data\0id", "pipe", None, "error\0msg")
            .await
            .expect("mark_failed should succeed even with interior NUL bytes");

        let got = received.lock().unwrap();
        // The callback should receive the sanitized strings (NUL stripped).
        assert_eq!(got[0], "dataid", "data_id NUL should be stripped");
        assert_eq!(got[1], "errormsg", "error NUL should be stripped");
    }

    /// Verify that stamp_provenance with an interior-NUL node_set does not panic.
    #[tokio::test]
    async fn stamp_provenance_nul_node_set_does_not_panic() {
        unsafe extern "C" fn on_stamp_provenance(
            _state: *mut c_void,
            _data_id: *const std::ffi::c_char,
            _pipeline_name: *const std::ffi::c_char,
            _task_name: *const std::ffi::c_char,
            _user_id: *const u8,
            node_set: *const std::ffi::c_char,
        ) {
            // Verify the node_set pointer is non-null and readable.
            if !node_set.is_null() {
                let _ = unsafe { std::ffi::CStr::from_ptr(node_set) }.to_string_lossy();
            }
        }

        let vtable = CgExecStatusManagerVtable {
            is_completed: None,
            mark_completed: None,
            mark_failed: None,
            stamp_provenance: Some(on_stamp_provenance),
            destroy: None,
        };

        let mgr = VtableExecStatus {
            state: std::ptr::null_mut(),
            vtable,
        };

        // "node\0set" has an interior NUL — must not panic.
        mgr.stamp_provenance("data", "pipe", "task", None, Some("node\0set"))
            .await
            .expect("stamp_provenance should succeed even with interior NUL in node_set");
    }
}
