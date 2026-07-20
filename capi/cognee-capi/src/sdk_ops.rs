//! Phase 4 — core pipeline ops: `add`, `cognify`, `add_and_cognify`.
//!
//! Each export follows the Phase-2 canonical pattern: clone the
//! `Arc<HandleState>` into `spawn_sdk_op`, obtain a `CogneeServices` via
//! `state.services().await?`, call the `cognee` API with the bundled
//! `Arc<dyn …>` handles, marshal the result back to JSON, and deliver it
//! through the callback.
//!
//! ## Shared async logic
//!
//! The pure-Rust async op bodies (`add`, `cognify`, `add_and_cognify`) and all
//! their helpers now live in `cognee_bindings_common::ops::pipeline`. This file
//! contains only the C-specific wrappers: `parse_c_str_or_fire` and the
//! `#[no_mangle]` extern "C" exports that parse C strings, spawn async tasks,
//! and deliver results through the callback.

use std::ffi::{CStr, CString, c_char};
use std::sync::Arc;

use cognee_bindings_common::SdkError;
use cognee_bindings_common::ops::pipeline;

use crate::error::CgErrorCode;
use crate::runtime::global_runtime;
use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};

// ---------------------------------------------------------------------------
// UTF-8 helper: parse a raw C string, delivering errors via the deferred
// callback pattern (R1).  Returns `None` if parsing fails (caller should
// return immediately).  `ud_raw` is the `user_data as usize` stash.
// ---------------------------------------------------------------------------

/// Attempt to parse a (non-null) C string pointer into an owned `String`.
///
/// On success returns `Some(owned)`.  On UTF-8 error, fires the callback on a
/// spawned thread (R1) and returns `None` — the caller must return immediately.
///
/// `ud_raw` carries `user_data as usize` so the closure is `Send`.
pub(crate) fn parse_c_str_or_fire(
    ptr: *const c_char,
    field_name: &'static str,
    callback: CgSdkResultCallback,
    ud_raw: usize,
) -> Option<String> {
    // Guard against null pointers for required (non-optional) string params.
    if ptr.is_null() {
        let rt = global_runtime()?;
        let msg_text = format!("{field_name} must not be null");
        rt.handle().spawn(async move {
            let msg = CString::new(msg_text).unwrap_or_else(|_| {
                CString::new("argument must not be null").expect("literal has no null bytes")
            });
            // SAFETY: ud_raw was a valid *mut c_void at capture time.
            unsafe {
                callback(
                    CgErrorCode::NullPointer,
                    std::ptr::null(),
                    msg.as_ptr(),
                    ud_raw as *mut std::ffi::c_void,
                )
            };
        });
        return None;
    }
    match unsafe { CStr::from_ptr(ptr) }.to_str() {
        Ok(s) => Some(s.to_owned()),
        Err(_) => {
            // Deliver via a spawned OS thread to honour R1.
            let rt = global_runtime()?;
            let msg_text = format!("{field_name} is not valid UTF-8");
            rt.handle().spawn(async move {
                let msg = CString::new(msg_text).unwrap_or_else(|_| {
                    CString::new("argument is not valid UTF-8").expect("literal has no null bytes")
                });
                // SAFETY: ud_raw was a valid *mut c_void at capture time.
                unsafe {
                    callback(
                        CgErrorCode::Utf8Error,
                        std::ptr::null(),
                        msg.as_ptr(),
                        ud_raw as *mut std::ffi::c_void,
                    )
                };
            });
            None
        }
    }
}

// ---------------------------------------------------------------------------
// C-exported functions.
// ---------------------------------------------------------------------------

/// Add data to the named dataset.
///
/// `inputs_json` is a `CogneeDataInput` object **or** array (see wire shapes
/// in the header).  `dataset_name` is the target dataset name (will be
/// auto-created if absent).  `opts_json` may be `NULL` or a JSON object with
/// an optional `"tenant"` key (UUID string).
///
/// Async (D4, R1): the callback fires on a tokio worker thread, never
/// synchronously from this call.
///
/// On success `result_json` is a `CogneeAddResult` JSON object:
/// `{"datasetName":"…","added":[…],"addedCount":N,"deduplicated":[…],"deduplicatedCount":M}`
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `inputs_json` and
/// `dataset_name` must be valid null-terminated UTF-8 strings.
/// `opts_json` may be NULL.  `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_add(
    sdk: *const CgSdk,
    inputs_json: *const c_char,
    dataset_name: *const c_char,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    // Stash user_data as usize so error-path closures are Send (same pattern
    // as cg_sdk_warm / cg_sdk_owner_id in sdk.rs).
    let ud_raw = user_data as usize;

    // Parse string arguments before spawning (pointers are only valid during
    // this call).
    let inputs_str = match parse_c_str_or_fire(inputs_json, "inputs_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        // Parse inputs JSON.
        let inputs_val: serde_json::Value = serde_json::from_str(&inputs_str)
            .map_err(|e| SdkError::Validation(format!("inputs_json parse error: {e}")))?;
        // Parse opts JSON (default to null if absent).
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        pipeline::add(&state, inputs_val, &dataset_str, &opts_val).await
    });
}

/// Run the cognify pipeline on an existing dataset.
///
/// `dataset_name` is the name of a dataset that must already exist (created by
/// a prior `cg_sdk_add` call).  `opts_json` may be `NULL` or a JSON object with
/// optional keys: `tenant` (UUID string), `chunkSize` (integer), `chunkOverlap`
/// (integer), `summarization` (boolean), `temporalCognify` (boolean),
/// `triplet` (boolean).
///
/// Async (D4, R1): the callback fires on a tokio worker thread.
///
/// On success `result_json` is a `CogneeCognifyResult` JSON object:
/// `{"chunks":N,"entities":N,"edges":N,"summaries":N,"embeddings":N,"alreadyCompleted":false,"priorPipelineRunId":null}`
///
/// # Safety
/// `sdk` and `dataset_name` must be valid non-null pointers to null-terminated
/// UTF-8 strings.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_cognify(
    sdk: *const CgSdk,
    dataset_name: *const c_char,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        pipeline::cognify(&state, &dataset_str, &opts_val).await
    });
}

/// Add data and immediately cognify — a single combined op.
///
/// Equivalent to `cg_sdk_add` followed by `cg_sdk_cognify`, but with the
/// optimisation that cognify operates only on the **newly-added** items (items
/// that were already present are skipped).  If all inputs were duplicates,
/// cognify is skipped entirely and a zeroed `CogneeCognifyResult` is returned.
///
/// On success `result_json` is:
/// `{"add":CogneeAddResult,"cognify":CogneeCognifyResult}`
///
/// # Safety
/// Same as `cg_sdk_add`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_add_and_cognify(
    sdk: *const CgSdk,
    inputs_json: *const c_char,
    dataset_name: *const c_char,
    opts_json: *const c_char,
    callback: CgSdkResultCallback,
    user_data: *mut std::ffi::c_void,
) {
    if sdk.is_null() {
        crate::error::set_last_error("null pointer: sdk");
        return;
    }
    let state = Arc::clone(unsafe { &(*sdk).state });
    let ud_raw = user_data as usize;

    let inputs_str = match parse_c_str_or_fire(inputs_json, "inputs_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let opts_str: Option<String> = if opts_json.is_null() {
        None
    } else {
        match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
            Some(s) => Some(s),
            None => return,
        }
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let inputs_val: serde_json::Value = serde_json::from_str(&inputs_str)
            .map_err(|e| SdkError::Validation(format!("inputs_json parse error: {e}")))?;
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        pipeline::add_and_cognify(&state, inputs_val, &dataset_str, &opts_val).await
    });
}
