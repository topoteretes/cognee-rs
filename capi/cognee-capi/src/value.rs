use std::ffi::{c_char, c_void};
use std::sync::Arc;

use cognee_core::Value;

use crate::error::{CgErrorCode, set_last_error};

/// Opaque handle wrapping `Arc<dyn Value>`.
pub struct CgValue {
    pub(crate) inner: Arc<dyn Value>,
}

// ---------------------------------------------------------------------------
// Opaque user-data wrapper
// ---------------------------------------------------------------------------

/// Wraps an arbitrary `void*` from C as a Rust `Value`.
///
/// # Safety contract
/// The C caller guarantees:
/// - `data` is valid until `destructor` is called (or forever if no destructor).
/// - `data` is safe to send across threads (`Send + Sync`).
struct OpaqueValue {
    data: *mut c_void,
    destructor: Option<unsafe extern "C" fn(*mut c_void)>,
}

// C caller guarantees thread-safety.
unsafe impl Send for OpaqueValue {}
unsafe impl Sync for OpaqueValue {}

impl Drop for OpaqueValue {
    fn drop(&mut self) {
        if let Some(dtor) = self.destructor {
            unsafe { dtor(self.data) };
        }
    }
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn cg_value_from_i64(v: i64) -> *mut CgValue {
    Box::into_raw(Box::new(CgValue { inner: Arc::new(v) }))
}

#[unsafe(no_mangle)]
pub extern "C" fn cg_value_from_f64(v: f64) -> *mut CgValue {
    Box::into_raw(Box::new(CgValue { inner: Arc::new(v) }))
}

#[unsafe(no_mangle)]
pub extern "C" fn cg_value_from_bool(v: bool) -> *mut CgValue {
    Box::into_raw(Box::new(CgValue { inner: Arc::new(v) }))
}

/// Create a value from a UTF-8 string. The string is copied.
///
/// # Safety
/// `s` must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_from_string(s: *const c_char) -> *mut CgValue {
    if s.is_null() {
        set_last_error("null string pointer");
        return std::ptr::null_mut();
    }
    let rs = match unsafe { crate::util::c_str_to_str(s) } {
        Ok(s) => s.to_owned(),
        Err(_) => return std::ptr::null_mut(),
    };
    Box::into_raw(Box::new(CgValue {
        inner: Arc::new(rs),
    }))
}

/// Create a value from a byte buffer. The data is copied.
///
/// # Safety
/// `data` must point to `len` valid bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_from_bytes(data: *const u8, len: usize) -> *mut CgValue {
    if data.is_null() && len > 0 {
        set_last_error("null data pointer with non-zero length");
        return std::ptr::null_mut();
    }
    let bytes = if len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    Box::into_raw(Box::new(CgValue {
        inner: Arc::new(bytes),
    }))
}

/// Create a value wrapping an opaque C pointer.
///
/// `destructor` is called when the value is dropped (may be NULL).
///
/// # Safety
/// - `data` must be valid for the lifetime of this value.
/// - `data` must be safe to use from any thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_from_opaque(
    data: *mut c_void,
    destructor: Option<unsafe extern "C" fn(*mut c_void)>,
) -> *mut CgValue {
    Box::into_raw(Box::new(CgValue {
        inner: Arc::new(OpaqueValue { data, destructor }),
    }))
}

// ---------------------------------------------------------------------------
// Helper: resolve through ArcValueHolder wrappers
// ---------------------------------------------------------------------------

use crate::iterator::ArcValueHolder;

/// Resolve through ArcValueHolder layers to get the actual `dyn Any`.
fn resolve_any(val: &dyn Value) -> &dyn std::any::Any {
    let any = val.as_any();
    // If the value is an ArcValueHolder, unwrap and try the inner value
    if let Some(holder) = any.downcast_ref::<ArcValueHolder>() {
        return resolve_any(holder.0.as_ref());
    }
    any
}

// ---------------------------------------------------------------------------
// Accessors
// ---------------------------------------------------------------------------

/// Try to read the value as an `i64`.
///
/// # Safety
/// `v` must be a valid `CgValue` pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_as_i64(v: *const CgValue, out: *mut i64) -> CgErrorCode {
    null_check!(v);
    null_check!(out);
    let val = unsafe { &*v };
    match resolve_any(val.inner.as_ref()).downcast_ref::<i64>() {
        Some(n) => {
            unsafe { *out = *n };
            CgErrorCode::Ok
        }
        None => {
            set_last_error("value is not i64");
            CgErrorCode::TypeMismatch
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_as_f64(v: *const CgValue, out: *mut f64) -> CgErrorCode {
    null_check!(v);
    null_check!(out);
    let val = unsafe { &*v };
    match resolve_any(val.inner.as_ref()).downcast_ref::<f64>() {
        Some(n) => {
            unsafe { *out = *n };
            CgErrorCode::Ok
        }
        None => {
            set_last_error("value is not f64");
            CgErrorCode::TypeMismatch
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_as_bool(v: *const CgValue, out: *mut bool) -> CgErrorCode {
    null_check!(v);
    null_check!(out);
    let val = unsafe { &*v };
    match resolve_any(val.inner.as_ref()).downcast_ref::<bool>() {
        Some(b) => {
            unsafe { *out = *b };
            CgErrorCode::Ok
        }
        None => {
            set_last_error("value is not bool");
            CgErrorCode::TypeMismatch
        }
    }
}

/// Read the value as a string. Sets `*out` to an interior pointer and `*len`
/// to the byte length. The pointer is valid as long as the `CgValue` lives.
///
/// # Safety
/// `v`, `out`, `len` must be valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_as_string(
    v: *const CgValue,
    out: *mut *const c_char,
    len: *mut usize,
) -> CgErrorCode {
    null_check!(v);
    null_check!(out);
    null_check!(len);
    let val = unsafe { &*v };
    match resolve_any(val.inner.as_ref()).downcast_ref::<String>() {
        Some(s) => {
            unsafe {
                *out = s.as_ptr() as *const c_char;
                *len = s.len();
            }
            CgErrorCode::Ok
        }
        None => {
            set_last_error("value is not String");
            CgErrorCode::TypeMismatch
        }
    }
}

/// Read the value as a byte buffer.
///
/// # Safety
/// `v`, `out`, `len` must be valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_as_bytes(
    v: *const CgValue,
    out: *mut *const u8,
    len: *mut usize,
) -> CgErrorCode {
    null_check!(v);
    null_check!(out);
    null_check!(len);
    let val = unsafe { &*v };
    match resolve_any(val.inner.as_ref()).downcast_ref::<Vec<u8>>() {
        Some(b) => {
            unsafe {
                *out = b.as_ptr();
                *len = b.len();
            }
            CgErrorCode::Ok
        }
        None => {
            set_last_error("value is not Vec<u8>");
            CgErrorCode::TypeMismatch
        }
    }
}

/// Read the value as an opaque pointer.
///
/// # Safety
/// `v` and `out` must be valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_as_opaque(
    v: *const CgValue,
    out: *mut *mut c_void,
) -> CgErrorCode {
    null_check!(v);
    null_check!(out);
    let val = unsafe { &*v };
    match resolve_any(val.inner.as_ref()).downcast_ref::<OpaqueValue>() {
        Some(ov) => {
            unsafe { *out = ov.data };
            CgErrorCode::Ok
        }
        None => {
            set_last_error("value is not opaque");
            CgErrorCode::TypeMismatch
        }
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Clone (increment refcount) a value handle.
///
/// # Safety
/// `v` must be a valid `CgValue` pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_clone(v: *const CgValue) -> *mut CgValue {
    if v.is_null() {
        return std::ptr::null_mut();
    }
    let val = unsafe { &*v };
    Box::into_raw(Box::new(CgValue {
        inner: Arc::clone(&val.inner),
    }))
}

/// Destroy a value handle.
///
/// # Safety
/// `v` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_destroy(v: *mut CgValue) {
    if !v.is_null() {
        unsafe {
            drop(Box::from_raw(v));
        }
    }
}
