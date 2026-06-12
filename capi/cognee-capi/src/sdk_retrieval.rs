//! Phase 5 — retrieval ops: `search`, `recall`.
//!
//! The core async logic lives in
//! [`cognee_bindings_common::ops::retrieval`]; this module only contains the
//! C-exported entry points that parse C strings and dispatch to the shared
//! operations via [`spawn_sdk_op`].
//!
//! ## Input marshalling
//!
//! `SearchType` is parsed from its SCREAMING_SNAKE_CASE serde wire name via
//! `serde_json::from_value(Value::String(s))` — the same path the HTTP server
//! uses, guaranteed to stay in sync with the `#[serde(rename_all =
//! "SCREAMING_SNAKE_CASE")]` attribute on `SearchType`.
//!
//! `opts_json` fields are camelCase (D3), mirroring the TS wire shapes.
//!
//! ## Result marshalling
//!
//! `SearchResponse` IS `Serialize` — pass through `serde_json::to_string`
//! directly (no extra helpers needed).
//!
//! `RecallResult` does NOT derive `Serialize` (derives only `Debug, Clone`) —
//! hand-build JSON with camelCase keys: `items`, `searchTypeUsed`, `autoRouted`,
//! `searchResponse`.

use std::ffi::c_char;
use std::sync::Arc;

use cognee_bindings_common::ops::retrieval;
use cognee_bindings_common::{HandleState, SdkError};

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

// ---------------------------------------------------------------------------
// C-exported functions.
// ---------------------------------------------------------------------------

/// Search the knowledge graph.
///
/// `query` is the search query string (required, non-null).
/// `opts_json` may be `NULL` or a JSON object with optional fields:
///   "searchType"             — SCREAMING_SNAKE_CASE string (default GRAPH_COMPLETION)
///   "datasets"               — string array of dataset names
///   "datasetIds"             — UUID string array
///   "topK"                   — integer
///   "systemPrompt"           — string
///   "sessionId"              — string
///   "nodeType"               — string
///   "nodeName"               — string array
///   "onlyContext"            — boolean
///   "useCombinedContext"     — boolean
///   "verbose"                — boolean
///   "saveInteraction"        — boolean (default true)
///   "autoFeedbackDetection"  — boolean
///
/// `userId` from opts is ignored; `user_id` in `SearchRequest` is always set
/// from the handle's `owner_id` so dataset-name resolution works correctly.
///
/// Async (D4, R1): the callback fires on a tokio worker thread, never
/// synchronously from this call.
///
/// On success `result_json` is a `SearchResponse` JSON value (an array of
/// search result objects).
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `query` must be a valid
/// null-terminated UTF-8 string.  `opts_json` may be NULL.
/// `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_search(
    sdk: *const CgSdk,
    query: *const c_char,
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

    let query_str = match parse_c_str_or_fire(query, "query", callback, ud_raw) {
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
        retrieval::search(&state, &query_str, &opts_val).await
    });
}

/// Recall from memory using the unified recall pipeline.
///
/// `query` is the recall query string (required, non-null).
/// `opts_json` may be `NULL` or a JSON object with optional fields:
///   "searchType"  — SCREAMING_SNAKE_CASE string for forced search type
///   "datasets"    — string array of dataset names to restrict recall
///   "topK"        — integer (default 10)
///   "autoRoute"   — boolean (default false)
///   "sessionId"   — string
///   "scope"       — string or string array:
///                   "auto" | "graph" | "session" | "trace" | "graph_context"
///                   (absent/null → "auto" default applied by recall())
///
/// Async (D4, R1): the callback fires on a tokio worker thread.
///
/// On success `result_json` is a JSON object with keys:
///   "items"          — array of recall items
///   "searchTypeUsed" — null or SCREAMING_SNAKE_CASE string
///   "autoRouted"     — boolean
///   "searchResponse" — null or SearchResponse value
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `query` must be a valid
/// null-terminated UTF-8 string.  `opts_json` may be NULL.
/// `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_recall(
    sdk: *const CgSdk,
    query: *const c_char,
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

    let query_str = match parse_c_str_or_fire(query, "query", callback, ud_raw) {
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
        retrieval::recall(&state, &query_str, &opts_val).await
    });
}
