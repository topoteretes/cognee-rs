use std::ffi::{CStr, CString, c_char};

use crate::error::{CgErrorCode, set_last_error};

/// Build a C string from a Rust string, replacing interior NUL bytes so the
/// conversion can never fail. Used on FFI callback paths where panicking would
/// abort the host process.
///
/// Interior NUL bytes are silently dropped — the same lossy behaviour that the
/// JS and Python bindings already accept. This is preferable to a crash.
pub(crate) fn cstring_lossy(s: &str) -> CString {
    match CString::new(s) {
        Ok(c) => c,
        Err(_) => {
            let sanitized: String = s.chars().filter(|&c| c != '\0').collect();
            CString::new(sanitized).expect("interior NULs stripped, so this cannot fail")
        }
    }
}

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
    cstring_lossy(s).into_raw()
}

/// Free a string previously returned by this library.
///
/// # Safety
/// `s` must have been allocated by this library, or be null.
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
#[allow(unused_macros)]
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

// ── cg_json_string_decode ────────────────────────────────────────────────────

/// Decode a JSON string literal to raw UTF-8.
///
/// Parses `json_string` (a null-terminated UTF-8 JSON string literal,
/// including the surrounding double-quote characters), unescapes all JSON
/// escape sequences, and writes the decoded bytes to `*out_utf8` as a new
/// heap-allocated null-terminated C string.
///
/// The caller owns the result and must free it with `cg_string_destroy`.
///
/// Returns:
/// - `CG_OK` on success; `*out_utf8` is set to the decoded string.
/// - `CG_ERR_NULL_POINTER` if `json_string` or `out_utf8` is NULL.
/// - `CG_ERR_UTF8` if `json_string` is not valid UTF-8.
/// - `CG_ERR_SDK_VALIDATION` (14) if `json_string` is not a valid JSON string
///   literal (i.e. not a JSON string type, or malformed escapes).
///
/// ## Rationale (D9, R8)
///
/// `cg_sdk_visualize` delivers the HTML document as a JSON-escaped quoted
/// string.  This helper removes the unescaping burden from C callers who need
/// the raw HTML bytes. For large outputs, `cg_sdk_visualize_to_file` avoids
/// the need entirely.
///
/// # Safety
/// `json_string` must be a valid null-terminated UTF-8 string. `out_utf8`
/// must be a valid non-null pointer to a `char*` that will receive the result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_json_string_decode(
    json_string: *const c_char,
    out_utf8: *mut *mut c_char,
) -> CgErrorCode {
    null_check!(json_string);
    null_check!(out_utf8);

    let input = match unsafe { CStr::from_ptr(json_string) }.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(format!("json_string is not valid UTF-8: {e}"));
            return CgErrorCode::Utf8Error;
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(input) {
        Ok(v) => v,
        Err(e) => {
            set_last_error(format!("json_string is not valid JSON: {e}"));
            return CgErrorCode::SdkValidation;
        }
    };

    let decoded = match parsed.as_str() {
        Some(s) => s,
        None => {
            set_last_error(
                "json_string is not a JSON string (e.g. it may be a number, bool, null, object, or array)",
            );
            return CgErrorCode::SdkValidation;
        }
    };

    let c_result = match CString::new(decoded) {
        Ok(s) => s,
        Err(_) => {
            set_last_error(
                "decoded string contains a null byte and cannot be represented as a C string",
            );
            return CgErrorCode::SdkValidation;
        }
    };

    unsafe { *out_utf8 = c_result.into_raw() };
    CgErrorCode::Ok
}
