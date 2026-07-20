//! Phase 7 — visualization ops: `cg_sdk_visualize` / `cg_sdk_visualize_to_file`.
//!
//! Both functions are gated behind `#[cfg(feature = "visualization")]`.
//! When the feature is absent, the exported functions fire the callback with
//! `CG_ERR_FEATURE_NOT_BUILT` (16) via a spawned task (R1 deferred), so
//! callers get a typed runtime error instead of a link failure (D6).
//!
//! ## Function shapes
//!
//! - `cg_sdk_visualize(sdk, opts_json, cb, user_data)` — calls
//!   `cognee::visualization::render(&*graph_db)` and delivers the
//!   self-contained HTML document as a **quoted JSON string** (D9).  The HTML
//!   may be several hundred kilobytes; copy it out of the callback or use
//!   `cg_json_string_decode` (see `util.rs`) to unescape into raw UTF-8.
//!   Prefer `cg_sdk_visualize_to_file` for large graphs.
//!
//! - `cg_sdk_visualize_to_file(sdk, file_path, opts_json, cb, user_data)` —
//!   calls `cognee::visualize(&*graph_db, destination_path)` and delivers
//!   the written file path as a **quoted JSON string** (D9).
//!   `opts_json.destinationPath` is optional; absent → default
//!   `~/graph_visualization.html`.
//!
//! ## opts_json shape
//!
//!   `{"destinationPath?": "<path>"}` — only `destinationPath` is parsed;
//!   unknown keys are ignored.

use std::ffi::c_char;

use cognee_bindings_common::SdkError;

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};

// These are only used in the feature-enabled paths.
#[cfg(feature = "visualization")]
use cognee_bindings_common::ops::visualization;
#[cfg(feature = "visualization")]
use crate::error::set_last_error;
#[cfg(feature = "visualization")]
use serde_json::json;
#[cfg(feature = "visualization")]
use std::ffi::CStr;
#[cfg(feature = "visualization")]
use std::sync::Arc;

// ---------------------------------------------------------------------------
// C-exported functions (always present regardless of features — D6).
// ---------------------------------------------------------------------------

/// Render the knowledge-graph as a self-contained HTML document.
///
/// The HTML is delivered via the callback as a **quoted JSON string** (D9),
/// e.g. `"\"<!DOCTYPE html>…\""`.  Use `cg_json_string_decode` to unescape
/// it to raw UTF-8.  For large graphs, prefer `cg_sdk_visualize_to_file` to
/// avoid holding the full HTML in memory.
///
/// `opts_json` — NULL or a JSON object; currently only `"destinationPath"`
/// (string) is parsed; all other keys are ignored.
///
/// Async (D4, R1): the callback fires on a tokio worker thread, never
/// synchronously from this call.
///
/// When the `visualization` feature was not compiled in, the callback fires
/// with `CG_ERR_FEATURE_NOT_BUILT` (16) and an informational message.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL. `opts_json`, if non-NULL, must be
/// a valid null-terminated UTF-8 string. `user_data` is forwarded to
/// `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_visualize(
    sdk: *const CgSdk,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    #[cfg(feature = "visualization")]
    {
        if sdk.is_null() {
            set_last_error("null pointer: sdk");
            return;
        }
        let state = Arc::clone(unsafe { &(*sdk).state });

        let opts_str: Option<String> = if opts_json.is_null() {
            None
        } else {
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
        };

        let ud = SendUserData(user_data);
        spawn_sdk_op(callback, ud, async move {
            let opts_val: serde_json::Value = match opts_str {
                Some(ref s) => serde_json::from_str(s)
                    .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
                None => serde_json::Value::Null,
            };
            let html = visualization::visualize(&state, Some(&opts_val)).await?;
            // D9: return as a quoted JSON string (the HTML is large; cg_json_string_decode
            // can unescape it client-side).
            Ok(json!(html))
        });
    }

    #[cfg(not(feature = "visualization"))]
    {
        let _ = (sdk, opts_json); // suppress unused warnings
        let ud = SendUserData(user_data);
        spawn_sdk_op(callback, ud, async move {
            Err(SdkError::FeatureNotBuilt(
                "visualization feature not built".to_string(),
            ))
        });
    }
}

/// Render the knowledge-graph to a file and return the written path.
///
/// The written file path is delivered via the callback as a **quoted JSON
/// string** (D9), e.g. `"\"/home/user/graph_visualization.html\""`.
///
/// `opts_json` — NULL or a JSON object with an optional `"destinationPath"`
/// field (string).  When absent, the default path
/// (`~/graph_visualization.html`) is used.
///
/// Async (D4, R1): the callback fires on a tokio worker thread.
///
/// When the `visualization` feature was not compiled in, the callback fires
/// with `CG_ERR_FEATURE_NOT_BUILT` (16).
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL. `opts_json`, if non-NULL, must be
/// a valid null-terminated UTF-8 string. `user_data` is forwarded to
/// `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_visualize_to_file(
    sdk: *const CgSdk,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    #[cfg(feature = "visualization")]
    {
        if sdk.is_null() {
            set_last_error("null pointer: sdk");
            return;
        }
        let state = Arc::clone(unsafe { &(*sdk).state });

        let opts_str: Option<String> = if opts_json.is_null() {
            None
        } else {
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
        };

        let ud = SendUserData(user_data);
        spawn_sdk_op(callback, ud, async move {
            let opts_val: serde_json::Value = match opts_str {
                Some(ref s) => serde_json::from_str(s)
                    .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
                None => serde_json::Value::Null,
            };
            let path_str = visualization::visualize_to_file(&state, Some(&opts_val)).await?;
            // D9: return as a quoted JSON string.
            Ok(json!(path_str))
        });
    }

    #[cfg(not(feature = "visualization"))]
    {
        let _ = (sdk, opts_json); // suppress unused warnings
        let ud = SendUserData(user_data);
        spawn_sdk_op(callback, ud, async move {
            Err(SdkError::FeatureNotBuilt(
                "visualization feature not built".to_string(),
            ))
        });
    }
}
