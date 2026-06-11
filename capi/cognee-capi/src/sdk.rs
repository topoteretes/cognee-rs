//! SDK-tier C API: `CgSdk` handle + `CgSdkWaiter` sync bridge.
//!
//! ## Overview
//!
//! This module implements the first tier of the C SDK surface (Phase 1b):
//!
//! - [`CgSdk`] — opaque handle wrapping `Arc<HandleState>`. Cheap to share
//!   across threads (`cg_sdk_clone`). Async ops keep the state alive via
//!   their own Arc clones, so callbacks may fire after `cg_sdk_destroy`.
//!
//! - [`CgSdkWaiter`] — single-use sync bridge. Create one, pass
//!   `cg_sdk_waiter_callback` + the waiter pointer as `user_data` to any
//!   `cg_sdk_*` async op, then block on `cg_sdk_waiter_wait`.
//!
//! ## Tier rule (R2)
//!
//! All `cg_sdk_*` functions return only:
//!   - `CG_OK` (0)
//!   - `CG_ERR_NULL_POINTER` (1)
//!   - `CG_ERR_RUNTIME` (3)
//!   - `CG_ERR_UTF8` (10)
//!   - SDK codes 11–18 (via the callback's `code` parameter for async ops)
//!
//! Engine codes 2, 4–9 never cross the SDK tier.
//!
//! ## Deferred-callback rule (R1)
//!
//! All async ops (`cg_sdk_warm`, `cg_sdk_owner_id`) spawn a tokio task so the
//! callback is **never** invoked synchronously from the initiating call.
//! Validation errors are also delivered via the spawned task.

use std::ffi::{CStr, CString, c_char};
use std::sync::{Arc, Condvar, Mutex};

use cognee_bindings_common::HandleState;
use cognee_lib::config::ConfigManager;

use crate::error::{CgErrorCode, set_last_error};
use crate::runtime::{ensure_runtime, global_runtime};
use crate::util::null_check;

// ── CgSdkResultCallback ─────────────────────────────────────────────────────

/// Callback invoked exactly once when an async SDK operation completes.
///
/// Parameters:
/// - `code` — `CG_OK` on success, an SDK error code (11–18) or one of
///   `CG_ERR_NULL_POINTER`/`CG_ERR_RUNTIME`/`CG_ERR_UTF8` on failure.
/// - `result_json` — on success, a valid JSON document (may be `"null"` for
///   void ops, a quoted string, `true`/`false`, or an object/array); `NULL`
///   on error. **Valid only inside the callback** — copy if needed.
/// - `error_message` — human-readable message on error; `NULL` on success.
///   **Valid only inside the callback** — copy if needed.
/// - `user_data` — the pointer passed to the initiating `cg_sdk_*` call.
///
/// The callback fires on a tokio worker thread. If the calling context
/// requires thread affinity (e.g. a UI thread) the caller must marshal back
/// themselves.
pub type CgSdkResultCallback = unsafe extern "C" fn(
    code: CgErrorCode,
    result_json: *const c_char,
    error_message: *const c_char,
    user_data: *mut std::ffi::c_void,
);

// ── CgSdkWaiter ─────────────────────────────────────────────────────────────

/// Internal state for the single-use sync waiter.
struct WaiterInner {
    /// `None` = not yet fired; `Some((code, result_json_owned))`.
    result: Option<(CgErrorCode, Option<CString>)>,
    /// Set to `true` once `cg_sdk_waiter_wait` has consumed the result.
    consumed: bool,
}

/// A single-use synchronous bridge for async SDK ops.
///
/// Usage:
/// ```c
/// CgSdkWaiter* w = cg_sdk_waiter_new();
/// cg_sdk_warm(sdk, cg_sdk_waiter_callback, w);
/// char* result = NULL;
/// CgErrorCode code = cg_sdk_waiter_wait(w, &result);
/// // use result …
/// cg_string_destroy(result);
/// cg_sdk_waiter_destroy(w);
/// ```
///
/// **Single-use**: calling `cg_sdk_waiter_wait` twice returns
/// `CG_ERR_VALIDATION`. Do not reuse a waiter after `wait` returns.
pub struct CgSdkWaiter {
    inner: Mutex<WaiterInner>,
    condvar: Condvar,
}

/// Opaque SDK handle.
///
/// Wraps `Arc<HandleState>` so it is cheap to share across threads via
/// `cg_sdk_clone`. In-flight async operations keep their own clone of the
/// `Arc`, so they remain valid after `cg_sdk_destroy`.
///
/// ## Thread safety
///
/// `CgSdk` is `Send + Sync` (both `Arc` and `HandleState` are). Concurrent
/// calls to `cg_sdk_warm`, `cg_sdk_owner_id`, etc. are safe.
///
/// ## Ordering footgun (R7)
///
/// Because the global tokio runtime is a process-wide `OnceLock`,
/// `cg_init_with_threads(n)` called **after** the first `cg_sdk_new`
/// silently no-ops — `cg_sdk_new` calls `cg_init` idempotently and the
/// OnceLock is already occupied. Consumers wanting a custom thread count
/// must call `cg_init_with_threads` **before** the first `cg_sdk_new`.
pub struct CgSdk {
    pub state: Arc<HandleState>,
}

// SAFETY: HandleState is Send+Sync; CgSdk is a thin Arc wrapper.
unsafe impl Send for CgSdk {}
unsafe impl Sync for CgSdk {}

// ── cg_api_version ──────────────────────────────────────────────────────────

/// Returns the packed API version as `(major << 16) | minor`.
///
/// Current version: major=1, minor=1 (Phase 1b adds SDK symbols).
/// MINOR increments each phase that ships new symbols.
#[unsafe(no_mangle)]
pub extern "C" fn cg_api_version() -> u32 {
    (1u32 << 16) | 1u32
}

// ── cg_sdk_new ──────────────────────────────────────────────────────────────

/// Create a new `CgSdk` handle.
///
/// `settings_json` may be `NULL` (use environment defaults) or a JSON object
/// whose keys override the env-loaded `Settings`.  The 3-way overlay
/// (`defaults < env < json`) is applied here.
///
/// Idempotently initialises the global tokio runtime if it has not been
/// initialised yet.  See the **ordering footgun** note on [`CgSdk`] (R7).
///
/// Returns a heap-allocated `CgSdk*` on success. The caller must eventually
/// call `cg_sdk_destroy` (or `cg_sdk_clone` + `cg_sdk_destroy` for shared
/// ownership).  Returns `NULL` on failure; call `cg_last_error_message()` for
/// details.
///
/// Sync, no I/O — network/disk access happens on `cg_sdk_warm`.
///
/// # Safety
/// `settings_json`, if non-null, must be a valid null-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_new(settings_json: *const c_char) -> *mut CgSdk {
    // Idempotent runtime init (R7).
    let code = ensure_runtime();
    if code != CgErrorCode::Ok {
        set_last_error("failed to initialise global runtime");
        return std::ptr::null_mut();
    }

    // Build Settings via the 3-way overlay: defaults < env < json.
    let base_settings = ConfigManager::from_env().read().clone();

    let settings = if settings_json.is_null() {
        // NULL → env defaults only.
        base_settings
    } else {
        // Non-NULL → parse JSON patch and apply over the env-loaded settings.
        let json_str = match unsafe { CStr::from_ptr(settings_json) }.to_str() {
            Ok(s) => s,
            Err(e) => {
                set_last_error(format!("settings_json is not valid UTF-8: {e}"));
                return std::ptr::null_mut();
            }
        };

        match apply_settings_json_patch(base_settings, json_str) {
            Ok(s) => s,
            Err(msg) => {
                set_last_error(msg);
                return std::ptr::null_mut();
            }
        }
    };

    let state = Arc::new(HandleState::from_settings(settings));
    Box::into_raw(Box::new(CgSdk { state }))
}

/// Apply a JSON object patch on top of `base` settings.
///
/// Only a limited set of top-level string/numeric keys are handled.  Unknown
/// keys are silently ignored (callers should use `cg_sdk_config_set` for
/// granular overrides once Phase 3 lands).
fn apply_settings_json_patch(
    mut base: cognee_lib::config::Settings,
    json: &str,
) -> Result<cognee_lib::config::Settings, String> {
    let patch: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("settings_json parse error: {e}"))?;

    let obj = patch
        .as_object()
        .ok_or_else(|| "settings_json must be a JSON object".to_string())?;

    // Apply known keys.  Phase 3 will replace this with ConfigManager::set.
    macro_rules! apply_str {
        ($field:ident, $key:expr) => {
            if let Some(serde_json::Value::String(v)) = obj.get($key) {
                base.$field = v.clone();
            }
        };
    }
    macro_rules! apply_u64 {
        ($field:ident, $key:expr) => {
            if let Some(v) = obj.get($key).and_then(|v| v.as_u64()) {
                base.$field = v as _;
            }
        };
    }

    apply_str!(llm_api_key, "llmApiKey");
    apply_str!(llm_model, "llmModel");
    apply_str!(llm_endpoint, "llmEndpoint");
    apply_str!(embedding_model_name, "embeddingModelName");
    apply_str!(embedding_endpoint, "embeddingEndpoint");
    apply_str!(embedding_api_key, "embeddingApiKey");
    apply_str!(embedding_provider, "embeddingProvider");
    apply_str!(graph_file_path, "graphFilePath");
    apply_str!(graph_database_url, "graphDatabaseUrl");
    apply_str!(data_root_directory, "dataRootDirectory");
    apply_str!(relational_db_url, "relationalDbUrl");
    apply_u64!(chunk_size, "chunkSize");
    apply_u64!(chunk_overlap, "chunkOverlap");

    Ok(base)
}

// ── cg_sdk_warm ─────────────────────────────────────────────────────────────

/// Warm the SDK handle: build and cache `CogneeServices` (DB connect, user
/// bootstrap, engine init).
///
/// Async (D4): the callback fires on a tokio worker thread, **never**
/// synchronously from this call (R1).
///
/// On success `result_json` is `"null"` (D9). On failure `result_json` is
/// `NULL` and `error_message` carries the human-readable message.
///
/// In-flight ops keep their own Arc clone of the handle state, so callbacks
/// may fire after `cg_sdk_destroy`.
///
/// # Safety
/// `sdk` must be a valid pointer to a `CgSdk` allocated by `cg_sdk_new`
/// (or null, in which case this is a no-op). `user_data` is forwarded to
/// `callback` as-is; its validity is the caller's responsibility.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_warm(
    sdk: *const CgSdk,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let user_data = SendUserData(user_data);

    let rt = match global_runtime() {
        Some(rt) => rt,
        None => {
            // Runtime not initialised — deliver error via a spawned OS thread
            // to honour the deferred-callback rule (R1): the callback must
            // never fire synchronously from the initiating call.
            // Stash the user_data pointer as usize to make the closure Send
            // (raw pointers are !Send but usize is Send; we re-interpret on
            // the other side).  The C caller guarantees the pointer is valid
            // when the callback fires.
            let ud_raw = user_data.0 as usize;
            std::thread::spawn(move || {
                let err_msg =
                    CString::new("runtime not initialised; call cg_init first")
                        .unwrap_or_else(|_| {
                            CString::new("runtime not initialised")
                                .expect("literal has no null bytes")
                        });
                // SAFETY: ud_raw was a valid *mut c_void at time of capture.
                unsafe {
                    callback(
                        CgErrorCode::RuntimeError,
                        std::ptr::null(),
                        err_msg.as_ptr(),
                        ud_raw as *mut std::ffi::c_void,
                    )
                };
            });
            return;
        }
    };

    rt.handle().spawn(async move {
        let ud = user_data; // SendUserData wrapper (Send)
        match state.services().await {
            Ok(_) => {
                let null_json = b"null\0";
                unsafe {
                    callback(
                        CgErrorCode::Ok,
                        null_json.as_ptr() as *const c_char,
                        std::ptr::null(),
                        ud.0,
                    )
                };
            }
            Err(e) => {
                let code = CgErrorCode::from(&e);
                let msg = CString::new(e.to_string()).unwrap_or_else(|_| {
                    CString::new("(error message contained null byte)")
                        .expect("literal has no null bytes")
                });
                unsafe { callback(code, std::ptr::null(), msg.as_ptr(), ud.0) };
            }
        }
    });
}

// ── cg_sdk_owner_id ─────────────────────────────────────────────────────────

/// Return the owner id as a quoted JSON string (e.g. `"\"<uuid>\""`, D9).
///
/// Warms the handle lazily if services have not yet been built.
///
/// Async (D4): callback fires on a tokio worker thread, never synchronously
/// (R1).
///
/// # Safety
/// `sdk` must be a valid pointer to a `CgSdk` allocated by `cg_sdk_new`
/// (or null, in which case this is a no-op). `user_data` is forwarded to
/// `callback` as-is; its validity is the caller's responsibility.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_owner_id(
    sdk: *const CgSdk,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let user_data = SendUserData(user_data);

    let rt = match global_runtime() {
        Some(rt) => rt,
        None => {
            // Runtime not initialised — deliver error via a spawned OS thread
            // to honour the deferred-callback rule (R1).
            // Stash user_data as usize (same pattern as cg_sdk_warm above).
            let ud_raw = user_data.0 as usize;
            std::thread::spawn(move || {
                let err_msg =
                    CString::new("runtime not initialised; call cg_init first")
                        .unwrap_or_else(|_| {
                            CString::new("runtime not initialised")
                                .expect("literal has no null bytes")
                        });
                // SAFETY: ud_raw was a valid *mut c_void at time of capture.
                unsafe {
                    callback(
                        CgErrorCode::RuntimeError,
                        std::ptr::null(),
                        err_msg.as_ptr(),
                        ud_raw as *mut std::ffi::c_void,
                    )
                };
            });
            return;
        }
    };

    rt.handle().spawn(async move {
        let ud = user_data; // SendUserData wrapper (Send)
        match state.owner_id().await {
            Ok(uuid) => {
                // Strict JSON: quoted string per D9.
                let json = format!("\"{}\"", uuid);
                let json_c = match CString::new(json) {
                    Ok(s) => s,
                    Err(_) => {
                        let msg = CString::new("owner_id serialization failed (null byte)")
                            .expect("literal has no null bytes");
                        unsafe {
                            callback(
                                CgErrorCode::RuntimeError,
                                std::ptr::null(),
                                msg.as_ptr(),
                                ud.0,
                            )
                        };
                        return;
                    }
                };
                unsafe { callback(CgErrorCode::Ok, json_c.as_ptr(), std::ptr::null(), ud.0) };
            }
            Err(e) => {
                let code = CgErrorCode::from(&e);
                let msg = CString::new(e.to_string()).unwrap_or_else(|_| {
                    CString::new("(error message contained null byte)")
                        .expect("literal has no null bytes")
                });
                unsafe { callback(code, std::ptr::null(), msg.as_ptr(), ud.0) };
            }
        }
    });
}

// ── cg_sdk_clone / cg_sdk_destroy ────────────────────────────────────────────

/// Arc-clone the handle. Cheap (single atomic increment).
///
/// The caller is responsible for eventually calling `cg_sdk_destroy` on the
/// returned pointer.  Returns `NULL` if `sdk` is null.
///
/// # Safety
/// `sdk` must be a valid pointer to a `CgSdk` or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_clone(sdk: *const CgSdk) -> *mut CgSdk {
    null_check!(sdk, std::ptr::null_mut());
    let state = Arc::clone(unsafe { &(*sdk).state });
    Box::into_raw(Box::new(CgSdk { state }))
}

/// Destroy a `CgSdk` handle.
///
/// Drops the `Arc<HandleState>`. In-flight async ops keep their own clones of
/// the state, so callbacks may fire **after** this call — do not access `sdk`
/// from any callback registered before destruction.
///
/// No-op if `sdk` is null.
///
/// # Safety
/// `sdk` must be a pointer previously returned by `cg_sdk_new` or
/// `cg_sdk_clone`, or null. Must not be called while the pointer is still
/// in use on another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_destroy(sdk: *mut CgSdk) {
    if !sdk.is_null() {
        drop(unsafe { Box::from_raw(sdk) });
    }
}

// ── CgSdkWaiter ─────────────────────────────────────────────────────────────

/// Create a new single-use `CgSdkWaiter`.
///
/// Pass `cg_sdk_waiter_callback` as the callback and the returned pointer as
/// `user_data` to any `cg_sdk_*` async op, then call `cg_sdk_waiter_wait` to
/// block until the callback fires.
///
/// **Single-use**: each waiter must only be used with exactly one async op.
/// Reuse (calling `wait` twice, or passing the waiter to two ops) returns
/// `CG_ERR_VALIDATION` on the second `wait` call.
#[unsafe(no_mangle)]
pub extern "C" fn cg_sdk_waiter_new() -> *mut CgSdkWaiter {
    let w = CgSdkWaiter {
        inner: Mutex::new(WaiterInner {
            result: None,
            consumed: false,
        }),
        condvar: Condvar::new(),
    };
    Box::into_raw(Box::new(w))
}

/// Callback suitable for passing to any `cg_sdk_*` async op when using the
/// waiter pattern.  Pass the `CgSdkWaiter*` as `user_data`.
///
/// # Safety
/// `user_data` must be a valid `*mut CgSdkWaiter` allocated by
/// `cg_sdk_waiter_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_waiter_callback(
    code: CgErrorCode,
    result_json: *const c_char,
    _error_message: *const c_char,
    user_data: *mut std::ffi::c_void,
) {
    if user_data.is_null() {
        return;
    }
    let waiter = unsafe { &*(user_data as *const CgSdkWaiter) };

    // Copy the result_json string into owned storage so the caller can use
    // it after the callback returns (the pointer from the op is only valid
    // inside the callback).
    let owned_json: Option<CString> = if result_json.is_null() {
        None
    } else {
        let s = unsafe { CStr::from_ptr(result_json) };
        // Best-effort: if conversion fails, treat as no result.
        s.to_str().ok().and_then(|s| CString::new(s).ok())
    };

    let mut guard = waiter.inner.lock().unwrap_or_else(|p| {
        // lock poison is unrecoverable
        p.into_inner()
    });
    guard.result = Some((code, owned_json));
    drop(guard);
    waiter.condvar.notify_one();
}

/// Block until the associated async op's callback fires, then return the
/// result.
///
/// `out_result_json` is set to a heap-allocated JSON string on success (`CG_OK`);
/// the caller must free it with `cg_string_destroy`.  On error it is set to
/// `NULL`.
///
/// Returns `CG_ERR_VALIDATION` if called on an already-consumed waiter
/// (single-use contract, R6).
///
/// Returns `CG_ERR_RUNTIME` if called from a tokio runtime thread (would
/// deadlock the worker).
///
/// # Safety
/// `waiter` must be a valid `*mut CgSdkWaiter` allocated by
/// `cg_sdk_waiter_new` and not yet consumed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_waiter_wait(
    waiter: *mut CgSdkWaiter,
    out_result_json: *mut *mut c_char,
) -> CgErrorCode {
    null_check!(waiter);
    // Detect tokio context — blocking here would deadlock a worker thread.
    if tokio::runtime::Handle::try_current().is_ok() {
        set_last_error(
            "cg_sdk_waiter_wait called from a tokio runtime thread; \
             this would deadlock the worker. Do not call wait from inside a callback.",
        );
        return CgErrorCode::RuntimeError;
    }

    let w = unsafe { &*waiter };

    let mut guard = w.inner.lock().unwrap_or_else(|p| {
        // lock poison is unrecoverable
        p.into_inner()
    });

    // Single-use check.
    if guard.consumed {
        set_last_error("cg_sdk_waiter_wait: waiter already consumed (single-use)");
        return CgErrorCode::SdkValidation;
    }

    // Block until the callback fires.
    guard = w
        .condvar
        .wait_while(guard, |inner| inner.result.is_none())
        .unwrap_or_else(|p| {
            // lock poison is unrecoverable
            p.into_inner()
        });

    guard.consumed = true;
    let (code, owned_json) = guard
        .result
        .take()
        .expect("condvar wait_while ensures result is Some before we proceed");

    drop(guard);

    // Transfer the owned JSON string to the caller.
    if !out_result_json.is_null() {
        unsafe {
            *out_result_json = match owned_json {
                Some(s) => s.into_raw(),
                None => std::ptr::null_mut(),
            };
        }
    }

    code
}

/// Destroy a `CgSdkWaiter`.
///
/// No-op if `waiter` is null.  Must not be called while `cg_sdk_waiter_wait`
/// is blocking on the same waiter from another thread.
///
/// # Safety
/// `waiter` must be a pointer previously returned by `cg_sdk_waiter_new`,
/// or null. Must not be called concurrently with `cg_sdk_waiter_wait`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_waiter_destroy(waiter: *mut CgSdkWaiter) {
    if !waiter.is_null() {
        drop(unsafe { Box::from_raw(waiter) });
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Newtype that makes a `*mut c_void` user-data pointer `Send`.
///
/// C callers are responsible for ensuring the pointer remains valid and is
/// not accessed from multiple threads simultaneously without synchronisation.
/// The SDK contract (R1) guarantees the callback fires exactly once, so a
/// raw pointer passed as `user_data` is safe to move into the spawned task.
struct SendUserData(*mut std::ffi::c_void);
// SAFETY: C caller guarantees pointer is not concurrently mutated.
unsafe impl Send for SendUserData {}

