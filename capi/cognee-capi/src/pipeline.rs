use std::ffi::{c_char, c_void};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use cognee_core::pipeline::{Pipeline, RetryDelay, RetryPolicy};
use cognee_core::{DataIdFn, Value};

use crate::error::set_last_error;
use crate::task_info::CgTaskInfo;
use crate::value::CgValue;

pub struct CgPipeline {
    /// The pipeline is stored behind an `Arc` so that background and async
    /// execution paths can cheaply clone a reference to the fully-built task
    /// list rather than reconstructing it.
    ///
    /// Mutation (adding tasks, setting fields) uses `Arc::get_mut` — this is
    /// always `Some` during construction because no second `Arc` clone exists
    /// until the first execute call is made.
    pub(crate) inner: Arc<Pipeline>,
}

/// Retry delay kind tag.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum CgRetryDelayKind {
    Constant = 0,
    Exponential = 1,
}

/// C-compatible retry delay specification.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CgRetryDelaySpec {
    pub kind: CgRetryDelayKind,
    pub base_ms: u64,
    pub factor: u32,
}

/// Create a new pipeline with the given description.
///
/// # Safety
/// `description` must be a null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_new(description: *const c_char) -> *mut CgPipeline {
    let desc = if description.is_null() {
        String::new()
    } else {
        match unsafe { crate::util::c_str_to_str(description) } {
            Ok(s) => s.to_owned(),
            Err(_) => return std::ptr::null_mut(),
        }
    };
    Box::into_raw(Box::new(CgPipeline {
        inner: Arc::new(Pipeline::new(desc)),
    }))
}

/// Set a human-readable pipeline name.
///
/// # Safety
/// `p` and `name` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_set_name(p: *mut CgPipeline, name: *const c_char) {
    if p.is_null() || name.is_null() {
        return;
    }
    if let Ok(s) = unsafe { crate::util::c_str_to_str(name) } {
        Arc::get_mut(unsafe { &mut (*p).inner })
            .expect("pipeline Arc has no second owner during construction")
            .name = Some(s.to_owned());
    }
}

/// Add a task to the pipeline. Takes ownership of `info`.
///
/// # Safety
/// `p` and `info` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_add_task(p: *mut CgPipeline, info: *mut CgTaskInfo) {
    if p.is_null() || info.is_null() {
        return;
    }
    let info = unsafe { Box::from_raw(info) };
    Arc::get_mut(unsafe { &mut (*p).inner })
        .expect("pipeline Arc has no second owner during construction")
        .tasks
        .push(info.inner);
}

/// Set the default batch size.
///
/// # Safety
/// `p` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_set_batch_size(p: *mut CgPipeline, size: usize) {
    if p.is_null() || size == 0 {
        return;
    }
    Arc::get_mut(unsafe { &mut (*p).inner })
        .expect("pipeline Arc has no second owner during construction")
        .batch_size = size;
}

/// Set the item-level concurrency.
///
/// # Safety
/// `p` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_set_concurrency(p: *mut CgPipeline, n: usize) {
    if p.is_null() || n == 0 {
        return;
    }
    Arc::get_mut(unsafe { &mut (*p).inner })
        .expect("pipeline Arc has no second owner during construction")
        .concurrency = n;
}

/// Set retry policy to no-retry.
///
/// # Safety
/// `p` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_set_retry_none(p: *mut CgPipeline) {
    if p.is_null() {
        return;
    }
    Arc::get_mut(unsafe { &mut (*p).inner })
        .expect("pipeline Arc has no second owner during construction")
        .retry_policy = RetryPolicy::NoRetry;
}

/// Set retry policy to limited retries.
///
/// # Safety
/// `p` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_set_retry_limited(
    p: *mut CgPipeline,
    max_attempts: u32,
    delay: CgRetryDelaySpec,
) {
    if p.is_null() {
        return;
    }
    let Some(max) = NonZeroU32::new(max_attempts) else {
        set_last_error("max_attempts must be > 0");
        return;
    };
    let rd = match delay.kind {
        CgRetryDelayKind::Constant => RetryDelay::Constant(Duration::from_millis(delay.base_ms)),
        CgRetryDelayKind::Exponential => RetryDelay::Exponential {
            base: Duration::from_millis(delay.base_ms),
            factor: delay.factor,
        },
    };
    Arc::get_mut(unsafe { &mut (*p).inner })
        .expect("pipeline Arc has no second owner during construction")
        .retry_policy = RetryPolicy::Limited {
        max_attempts: max,
        delay: rd,
    };
}

/// C function pointer type for extracting data IDs.
///
/// Returns `true` if a data ID was written to `buf`. `*written` is set to
/// the number of bytes written (excluding null terminator).
pub type CgDataIdFnPtr = unsafe extern "C" fn(
    v: *const CgValue,
    buf: *mut c_char,
    buf_len: usize,
    written: *mut usize,
    user_data: *mut c_void,
) -> bool;

struct DataIdUserData {
    ptr: *mut c_void,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}

unsafe impl Send for DataIdUserData {}
unsafe impl Sync for DataIdUserData {}

impl Drop for DataIdUserData {
    fn drop(&mut self) {
        if let Some(dtor) = self.destroy {
            unsafe { dtor(self.ptr) };
        }
    }
}

/// Set a data ID extraction function for incremental deduplication.
///
/// # Safety
/// `p` must be valid. `fn_ptr` must be a valid function pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_set_data_id_fn(
    p: *mut CgPipeline,
    fn_ptr: CgDataIdFnPtr,
    user_data: *mut c_void,
    destroy_ud: Option<unsafe extern "C" fn(*mut c_void)>,
) {
    if p.is_null() {
        return;
    }
    let ud = Arc::new(DataIdUserData {
        ptr: user_data,
        destroy: destroy_ud,
    });

    let data_id_fn: DataIdFn = Arc::new(move |value: Arc<dyn Value>| {
        let cg = Box::new(CgValue {
            inner: Arc::clone(&value),
        });
        let cg_ptr = Box::into_raw(cg) as *const CgValue;

        let mut buf = vec![0u8; 256];
        let mut written: usize = 0;

        let ok = unsafe {
            fn_ptr(
                cg_ptr,
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                &mut written,
                ud.ptr,
            )
        };

        unsafe {
            drop(Box::from_raw(cg_ptr as *mut CgValue));
        }

        if ok && written > 0 && written <= buf.len() {
            Some(String::from_utf8_lossy(&buf[..written]).into_owned())
        } else {
            None
        }
    });

    Arc::get_mut(unsafe { &mut (*p).inner })
        .expect("pipeline Arc has no second owner during construction")
        .data_id_fn = Some(data_id_fn);
}

/// # Safety
/// `p` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_pipeline_destroy(p: *mut CgPipeline) {
    if !p.is_null() {
        unsafe { drop(Box::from_raw(p)) };
    }
}
