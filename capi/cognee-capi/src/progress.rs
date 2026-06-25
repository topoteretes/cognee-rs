use cognee_core::ProgressToken;

use crate::error::{CgErrorCode, core_error_to_code, set_last_error};
use crate::util::null_check;

pub struct CgProgressToken {
    pub(crate) inner: ProgressToken,
}

#[unsafe(no_mangle)]
pub extern "C" fn cg_progress_token_new() -> *mut CgProgressToken {
    Box::into_raw(Box::new(CgProgressToken {
        inner: ProgressToken::new(),
    }))
}

/// # Safety
/// `t` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_set(t: *mut CgProgressToken, fraction: f64) {
    if !t.is_null() {
        unsafe { (*t).inner.set(fraction) };
    }
}

/// # Safety
/// `t` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_fraction(t: *const CgProgressToken) -> f64 {
    if t.is_null() {
        return 0.0;
    }
    unsafe { (*t).inner.fraction() }
}

/// # Safety
/// `t` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_width(t: *const CgProgressToken) -> f64 {
    if t.is_null() {
        return 0.0;
    }
    unsafe { (*t).inner.width() }
}

/// # Safety
/// `t` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_is_complete(t: *const CgProgressToken) -> bool {
    if t.is_null() {
        return false;
    }
    unsafe { (*t).inner.is_complete() }
}

/// # Safety
/// `t` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_root_fraction(t: *const CgProgressToken) -> f64 {
    if t.is_null() {
        return 0.0;
    }
    unsafe { (*t).inner.root_fraction() }
}

/// Split a progress token into sub-tokens by relative weights.
///
/// # Safety
/// `t` must be valid. `weights` must point to `count` elements.
/// `out` receives an array of `*mut CgProgressToken`; `out_count` is set.
/// The caller must free each sub-token and the array via
/// `cg_progress_token_array_destroy`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_split(
    t: *mut CgProgressToken,
    weights: *const u32,
    count: usize,
    out: *mut *mut *mut CgProgressToken,
    out_count: *mut usize,
) -> CgErrorCode {
    null_check!(t);
    null_check!(weights);
    null_check!(out);
    null_check!(out_count);

    let weights_slice = unsafe { std::slice::from_raw_parts(weights, count) };
    let token = unsafe { &(*t).inner };

    match token.split(weights_slice) {
        Ok(subs) => {
            let mut ptrs: Vec<*mut CgProgressToken> = subs
                .into_iter()
                .map(|s| Box::into_raw(Box::new(CgProgressToken { inner: s })))
                .collect();
            unsafe {
                *out_count = ptrs.len();
                *out = ptrs.as_mut_ptr();
            }
            std::mem::forget(ptrs); // caller owns the array
            CgErrorCode::Ok
        }
        Err(e) => {
            set_last_error(e.to_string());
            core_error_to_code(&e)
        }
    }
}

/// # Safety
/// `t` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_subtoken(
    t: *mut CgProgressToken,
    frac_width: f64,
) -> *mut CgProgressToken {
    if t.is_null() {
        return std::ptr::null_mut();
    }
    let sub = unsafe { (*t).inner.subtoken(frac_width) };
    Box::into_raw(Box::new(CgProgressToken { inner: sub }))
}

/// # Safety
/// `t` must be a valid pointer or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_clone(
    t: *const CgProgressToken,
) -> *mut CgProgressToken {
    if t.is_null() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(CgProgressToken {
        inner: unsafe { (*t).inner.clone() },
    }))
}

/// # Safety
/// `t` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_destroy(t: *mut CgProgressToken) {
    if !t.is_null() {
        unsafe { drop(Box::from_raw(t)) };
    }
}

/// Destroy an array of progress tokens returned by `cg_progress_token_split`.
///
/// # Safety
/// `arr` must have been allocated by `cg_progress_token_split`, or be null.
/// `count` must match the count returned by that function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_progress_token_array_destroy(
    arr: *mut *mut CgProgressToken,
    count: usize,
) {
    if arr.is_null() {
        return;
    }
    let ptrs = unsafe { Vec::from_raw_parts(arr, count, count) };
    for p in ptrs {
        if !p.is_null() {
            unsafe { drop(Box::from_raw(p)) };
        }
    }
}
