//! Phase 7 — cloud ops: `cg_sdk_serve` / `cg_sdk_disconnect`.
//!
//! Both functions are gated behind `#[cfg(feature = "cloud")]`.
//! When the feature is absent, the exported functions fire the callback with
//! `CG_ERR_FEATURE_NOT_BUILT` (16) via a spawned task (R1 deferred).
//!
//! ## Process-wide singletons
//!
//! `serve()` / `disconnect()` operate on the process-wide `CloudClient`
//! singleton, **not** on a `CgSdk*` handle — they do NOT accept a `sdk`
//! first argument.  The opts derive config from the global env / `ServeConfig`
//! builder, not from a handle's `HandleState`.  This matches the neon
//! reference implementation in `js/cognee-neon/src/sdk_cloud.rs`.
//!
//! ## Function shapes
//!
//! - `cg_sdk_serve(opts_json, cb, user_data)` — deserialises `opts_json` into
//!   `ServeConfig` fields and calls `cognee_lib::serve(config)`. On success
//!   `result_json` is `{"connected":true,"serviceUrl":"…"}`.
//!   `opts.url` → direct mode; absent → cloud (device-code) mode.
//!   Optional keys: `url`, `apiKey`, `cloudUrl`, `auth0Domain`,
//!   `auth0ClientId`, `auth0Audience`.
//!
//! - `cg_sdk_disconnect(opts_json, cb, user_data)` — calls
//!   `cognee_lib::disconnect(wipe_credentials)`.  `opts.wipeCredentials`
//!   (boolean, default false) controls whether the on-disk credential cache
//!   is erased.  On success `result_json` is `"null"` (D9).
//!
//! ## opts_json shapes
//!
//!   serve:      `{"url?":"…","apiKey?":"…","cloudUrl?":"…","auth0Domain?":"…",
//!                 "auth0ClientId?":"…","auth0Audience?":"…"}`
//!   disconnect: `{"wipeCredentials?":false}`

use std::ffi::c_char;

use cognee_bindings_common::SdkError;
use cognee_bindings_common::ops::cloud;

use crate::sdk::{CgSdkResultCallback, SendUserData, spawn_sdk_op};

// Only used in the feature-enabled path.
#[cfg(feature = "cloud")]
use std::ffi::CStr;

// ---------------------------------------------------------------------------
// C-exported functions (always present regardless of features — D6).
// ---------------------------------------------------------------------------

/// Connect the SDK to a Cognee Cloud instance.
///
/// Operates on the process-wide `CloudClient` singleton — does NOT take a
/// `CgSdk*` handle.  `opts_json` controls the connection mode:
///
///   - `{"url":"http://…"}` — **direct mode** (headless, for local servers
///     or CI).  `apiKey` is passed through to the server if present.
///   - `{}` or NULL — **cloud mode** (Auth0 device-code flow; requires a TTY).
///
/// Optional keys: `apiKey`, `cloudUrl`, `auth0Domain`, `auth0ClientId`,
/// `auth0Audience`.
///
/// On success `result_json` is:
///   `{"connected":true,"serviceUrl":"https://…"}`
///
/// Async (D4, R1): the callback fires on a tokio worker thread, never
/// synchronously from this call.
///
/// When the `cloud` feature was not compiled in, the callback fires with
/// `CG_ERR_FEATURE_NOT_BUILT` (16).
///
/// # Safety
/// `opts_json`, if non-NULL, must be a valid null-terminated UTF-8 string.
/// `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_serve(
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        #[cfg(feature = "cloud")]
        match unsafe { CStr::from_ptr(opts_json) }.to_str() {
            Ok(s) => Some(s.to_owned()),
            Err(_) => {
                let ud = SendUserData(user_data);
                spawn_sdk_op(callback, ud, async move {
                    Err(SdkError::Validation(
                        "opts_json is not valid UTF-8".to_string(),
                    ))
                });
                return;
            }
        }
        #[cfg(not(feature = "cloud"))]
        None
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        cloud::run_serve(opts_val).await
    });
}

/// Disconnect from Cognee Cloud and revert to local-execution mode.
///
/// Operates on the process-wide `CloudClient` singleton — does NOT take a
/// `CgSdk*` handle.
///
/// `opts_json` — NULL or a JSON object with an optional
/// `"wipeCredentials"` boolean (default `false`).  When `true`, the on-disk
/// credential cache is deleted so the next `cg_sdk_serve` must re-authenticate.
///
/// On success `result_json` is `"null"` (D9 — void op).
///
/// Async (D4, R1): the callback fires on a tokio worker thread.
///
/// When the `cloud` feature was not compiled in, the callback fires with
/// `CG_ERR_FEATURE_NOT_BUILT` (16).
///
/// # Safety
/// `opts_json`, if non-NULL, must be a valid null-terminated UTF-8 string.
/// `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_disconnect(
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        #[cfg(feature = "cloud")]
        match unsafe { CStr::from_ptr(opts_json) }.to_str() {
            Ok(s) => Some(s.to_owned()),
            Err(_) => {
                let ud = SendUserData(user_data);
                spawn_sdk_op(callback, ud, async move {
                    Err(SdkError::Validation(
                        "opts_json is not valid UTF-8".to_string(),
                    ))
                });
                return;
            }
        }
        #[cfg(not(feature = "cloud"))]
        None
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        cloud::run_disconnect(opts_val).await
    });
}
