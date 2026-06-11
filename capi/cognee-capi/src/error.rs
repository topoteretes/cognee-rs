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

/// Convenience helper: store `err`'s message in the thread-local last-error
/// slot *and* return the corresponding `CgErrorCode`.
///
/// Use this in every `cg_sdk_*` function that handles a `SdkError` so the
/// two-step pattern (set message + return code) does not repeat at every call
/// site.
///
/// **Thread-local caveat**: for async ops the error is delivered through the
/// callback's `error_message` parameter, not through this thread-local.  Call
/// this helper on the *calling* thread only for synchronous paths (e.g. inside
/// `cg_sdk_waiter_wait` after unblocking, or inside `cg_sdk_new`).
#[allow(dead_code)] // used in unit test below and future sync SDK paths (phases 3–7)
pub(crate) fn set_last_error_from(err: &SdkError) -> CgErrorCode {
    set_last_error(err.to_string());
    CgErrorCode::from(err)
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

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_bindings_common::SdkError;

    /// Engine codes that must never cross into the SDK tier (R2).
    const ENGINE_ONLY_CODES: &[u32] = &[2, 4, 5, 6, 7, 8, 9];

    fn is_engine_only_code(code: CgErrorCode) -> bool {
        ENGINE_ONLY_CODES.contains(&(code as u32))
    }

    /// Verify every `SdkError` variant maps to the expected SDK-tier code and
    /// that none maps to an engine-only code (R2 tiering rule enforcement).
    #[test]
    fn from_sdk_error_maps_to_sdk_tier_codes() {
        // Each pair: (SdkError variant, expected CgErrorCode)
        let cases: &[(SdkError, CgErrorCode)] = &[
            (
                SdkError::Component(cognee_lib::ComponentError::GraphDb("test".to_string())),
                CgErrorCode::Component,
            ),
            (
                SdkError::ServiceBuild("test".to_string()),
                CgErrorCode::ServiceBuild,
            ),
            (
                SdkError::UserBootstrap("test".to_string()),
                CgErrorCode::UserBootstrap,
            ),
            (
                SdkError::Runtime("test".to_string()),
                CgErrorCode::RuntimeError,
            ),
            (
                SdkError::Validation("test".to_string()),
                CgErrorCode::SdkValidation,
            ),
            (
                SdkError::Unsupported("test".to_string()),
                CgErrorCode::Unsupported,
            ),
            (
                SdkError::FeatureNotBuilt("test".to_string()),
                CgErrorCode::FeatureNotBuilt,
            ),
        ];

        for (err, expected_code) in cases {
            let code = CgErrorCode::from(err);
            assert_eq!(
                code,
                *expected_code,
                "SdkError::{} should map to {:?}",
                err.code(),
                expected_code
            );
            // R2: no SdkError must ever produce an engine-only code.
            assert!(
                !is_engine_only_code(code),
                "SdkError::{} produced engine-only code {:?} — R2 violation",
                err.code(),
                code
            );
            // R2: all SDK-tier codes must be >= 11.
            assert!(
                (code as u32) >= 11 || code == CgErrorCode::RuntimeError,
                "SdkError::{} produced code {:?} which is not a valid SDK-tier code",
                err.code(),
                code
            );
        }
    }

    /// Verify `set_last_error_from` stores the message and returns the code.
    #[test]
    fn set_last_error_from_stores_and_returns() {
        let err = SdkError::Validation("bad input".to_string());
        let code = set_last_error_from(&err);
        assert_eq!(code, CgErrorCode::SdkValidation);
        // The thread-local should now contain the error message.
        LAST_ERROR.with(|cell| {
            let borrow = cell.borrow();
            let msg = borrow.as_ref().expect("last error should be set");
            let msg_str = msg.to_str().expect("last error should be valid UTF-8");
            assert!(
                msg_str.contains("bad input"),
                "last error message should contain 'bad input', got: {msg_str}"
            );
        });
    }
}
