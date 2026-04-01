use std::cell::RefCell;
use std::ffi::{CString, c_char};

/// Error codes returned by all `cg_*` functions.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgErrorCode {
    Ok = 0,
    NullPointer = 1,
    InvalidArgument = 2,
    RuntimeError = 3,
    TaskFailed = 4,
    Cancelled = 5,
    NoTasks = 6,
    InvalidConfig = 7,
    MissingField = 8,
    TypeMismatch = 9,
    Utf8Error = 10,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

pub fn set_last_error(msg: impl Into<String>) {
    let s = msg.into();
    let cs =
        CString::new(s).unwrap_or_else(|_| CString::new("(error contained null byte)").unwrap());
    LAST_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(cs);
    });
}

pub fn clear_last_error() {
    LAST_ERROR.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// Returns a pointer to the last error message, or null if none.
///
/// The pointer is valid until the next call to any `cg_*` function on the same
/// thread, or until `cg_last_error_clear()` is called.
#[unsafe(no_mangle)]
pub extern "C" fn cg_last_error_message() -> *const c_char {
    LAST_ERROR.with(|cell| {
        let borrow = cell.borrow();
        match borrow.as_ref() {
            Some(cs) => cs.as_ptr(),
            None => std::ptr::null(),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cg_last_error_clear() {
    clear_last_error();
}

/// Map a cognee-core `ExecutionError` to an error code.
pub fn execution_error_to_code(e: &cognee_core::ExecutionError) -> CgErrorCode {
    match e {
        cognee_core::ExecutionError::TaskFailed { .. } => CgErrorCode::TaskFailed,
        cognee_core::ExecutionError::Cancelled => CgErrorCode::Cancelled,
        cognee_core::ExecutionError::NoTasks => CgErrorCode::NoTasks,
        cognee_core::ExecutionError::InvalidConfig { .. } => CgErrorCode::InvalidConfig,
    }
}

/// Map a cognee-core `CoreError` to an error code.
pub fn core_error_to_code(e: &cognee_core::CoreError) -> CgErrorCode {
    match e {
        cognee_core::CoreError::Runtime(_) => CgErrorCode::RuntimeError,
        cognee_core::CoreError::ThreadPoolBuild(_) => CgErrorCode::RuntimeError,
        cognee_core::CoreError::TaskAborted { .. } => CgErrorCode::TaskFailed,
        cognee_core::CoreError::MissingContextField { .. } => CgErrorCode::MissingField,
        cognee_core::CoreError::InvalidProgressSplit { .. } => CgErrorCode::InvalidArgument,
    }
}
