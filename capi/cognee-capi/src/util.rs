use std::ffi::{CStr, CString, c_char};

use crate::error::{CgErrorCode, set_last_error};

/// Convert a C string pointer to a Rust `&str`.
///
/// # Safety
/// Caller must ensure `ptr` is a valid, null-terminated UTF-8 string.
pub unsafe fn c_str_to_str<'a>(ptr: *const c_char) -> Result<&'a str, CgErrorCode> {
    if ptr.is_null() {
        set_last_error("null string pointer");
        return Err(CgErrorCode::NullPointer);
    }
    let cs = unsafe { CStr::from_ptr(ptr) };
    cs.to_str().map_err(|e| {
        set_last_error(format!("invalid UTF-8: {e}"));
        CgErrorCode::Utf8Error
    })
}

/// Allocate a C string from a Rust `&str`. Caller must free via `cg_string_destroy`.
pub fn str_to_c_owned(s: &str) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => {
            // String contained a null byte — replace with empty
            CString::new("").unwrap().into_raw()
        }
    }
}

/// Free a string previously returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_string_destroy(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}

/// Macro for null-checking a pointer argument. Returns `CgErrorCode::NullPointer`
/// and sets last error if null.
macro_rules! null_check {
    ($ptr:expr) => {
        if $ptr.is_null() {
            $crate::error::set_last_error(concat!("null pointer: ", stringify!($ptr)));
            return $crate::error::CgErrorCode::NullPointer;
        }
    };
    ($ptr:expr, $ret:expr) => {
        if $ptr.is_null() {
            $crate::error::set_last_error(concat!("null pointer: ", stringify!($ptr)));
            return $ret;
        }
    };
}

/// Macro wrapping a Result-returning expression. On Err, sets last error and
/// returns the given error code.
macro_rules! ffi_try {
    ($expr:expr, $code:expr) => {
        match $expr {
            Ok(val) => val,
            Err(e) => {
                $crate::error::set_last_error(e.to_string());
                return $code;
            }
        }
    };
}

pub(crate) use null_check;
