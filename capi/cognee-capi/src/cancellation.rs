use cognee_core::{CancellationHandle, CancellationToken, cancellation_pair};

use crate::error::CgErrorCode;
use crate::util::null_check;

pub struct CgCancellationHandle {
    pub(crate) inner: CancellationHandle,
}

pub struct CgCancellationToken {
    pub(crate) inner: CancellationToken,
}

/// Create a linked (handle, token) pair.
///
/// # Safety
/// `handle_out` and `token_out` must be valid, non-null pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_cancellation_pair(
    handle_out: *mut *mut CgCancellationHandle,
    token_out: *mut *mut CgCancellationToken,
) -> CgErrorCode {
    null_check!(handle_out);
    null_check!(token_out);
    let (h, t) = cancellation_pair();
    unsafe {
        *handle_out = Box::into_raw(Box::new(CgCancellationHandle { inner: h }));
        *token_out = Box::into_raw(Box::new(CgCancellationToken { inner: t }));
    }
    CgErrorCode::Ok
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_cancellation_handle_cancel(h: *mut CgCancellationHandle) {
    if !h.is_null() {
        unsafe { (*h).inner.cancel() };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_cancellation_handle_is_cancelled(
    h: *const CgCancellationHandle,
) -> bool {
    if h.is_null() {
        return false;
    }
    unsafe { (*h).inner.is_cancelled() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_cancellation_token_is_cancelled(t: *const CgCancellationToken) -> bool {
    if t.is_null() {
        return false;
    }
    unsafe { (*t).inner.is_cancelled() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_cancellation_handle_clone(
    h: *const CgCancellationHandle,
) -> *mut CgCancellationHandle {
    if h.is_null() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(CgCancellationHandle {
        inner: unsafe { (*h).inner.clone() },
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_cancellation_token_clone(
    t: *const CgCancellationToken,
) -> *mut CgCancellationToken {
    if t.is_null() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(CgCancellationToken {
        inner: unsafe { (*t).inner.clone() },
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_cancellation_handle_destroy(h: *mut CgCancellationHandle) {
    if !h.is_null() {
        unsafe { drop(Box::from_raw(h)) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_cancellation_token_destroy(t: *mut CgCancellationToken) {
    if !t.is_null() {
        unsafe { drop(Box::from_raw(t)) };
    }
}
