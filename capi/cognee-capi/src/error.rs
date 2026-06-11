use std::cell::RefCell;
use std::ffi::{CString, c_char};

use cognee_bindings_common::SdkError;

/// Error codes returned by all `cg_*` functions.
///
/// ## Tier rule (R2)
///
/// The enum is split into two tiers:
/// - **Engine tier** (values 0–10): returned by `cg_*` engine functions
///   (`cg_pipeline_*`, `cg_task_*`, `cg_value_*`, etc.).
/// - **SDK tier** (values 11–18): returned by `cg_sdk_*` functions **only**.
///
/// `cg_sdk_*` functions must never return engine codes 2, 4–9. They are
/// allowed to return `CG_OK` (0), `CG_ERR_NULL_POINTER` (1),
/// `CG_ERR_RUNTIME` (3), and `CG_ERR_UTF8` (10) for generic plumbing
/// failures, plus the SDK-tier codes 11–18 for SDK-specific errors.
///
/// Engine-tier functions must never return SDK codes 11–18.
///
/// Values are append-only per decision D5.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgErrorCode {
    // ── Engine tier (0–10) ──────────────────────────────────────────────────
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

    // ── SDK tier (11–18, append-only per D5) ────────────────────────────────
    /// An engine (storage / database / graph / vector / embedding / llm)
    /// failed to initialise through the `ComponentManager`.
    Component = 11,
    /// A derived service (thread pool, session store, ontology resolver, …)
    /// failed to construct.
    ServiceBuild = 12,
    /// The relational user bootstrap (`get_or_create_default_user`) failed.
    UserBootstrap = 13,
    /// Invalid input from the binding boundary (bad shape / missing field /
    /// parse failure). SDK-tier variant; distinct from engine-tier
    /// `InvalidArgument` (2) per R2.
    SdkValidation = 14,
    /// A requested input variant or feature is recognised but not yet wired
    /// end-to-end (e.g. `s3` / recursive `dataItem` inputs).
    Unsupported = 15,
    /// The binding function requires a Cargo feature that was not compiled
    /// into this build.
    FeatureNotBuilt = 16,
    /// `cg_sdk_config_set` was called with an unknown configuration key.
    UnknownConfigKey = 17,
    /// `cg_sdk_config_set` was called with a value whose type does not match
    /// the expected type for the given key.
    ConfigTypeMismatch = 18,
}

/// Map a `SdkError` to a `CgErrorCode` (SDK tier only — values 11–18).
///
/// `cg_sdk_*` functions use this impl so that engine codes 2, 4–9 never
/// cross into the SDK tier (R2).
impl From<&SdkError> for CgErrorCode {
    fn from(e: &SdkError) -> Self {
        match e {
            SdkError::Component(_) => CgErrorCode::Component,
            SdkError::ServiceBuild(_) => CgErrorCode::ServiceBuild,
            SdkError::UserBootstrap(_) => CgErrorCode::UserBootstrap,
            SdkError::Runtime(_) => CgErrorCode::RuntimeError,
            SdkError::Validation(_) => CgErrorCode::SdkValidation,
            SdkError::Unsupported(_) => CgErrorCode::Unsupported,
            SdkError::FeatureNotBuilt(_) => CgErrorCode::FeatureNotBuilt,
        }
    }
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
