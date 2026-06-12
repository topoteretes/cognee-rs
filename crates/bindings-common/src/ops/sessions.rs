//! Shared async session operations: `get_session`, `add_feedback`,
//! `delete_feedback`, `get_graph_context`, `set_graph_context`.
//!
//! These functions contain the pure-Rust async logic shared between every
//! language binding surface (C API, Neon JS, Python). Each function takes a
//! [`HandleState`] reference plus typed arguments, performs the operation
//! against the underlying cognee-lib session APIs, and returns a
//! `serde_json::Value` result (or an [`SdkError`]).
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.
//!
//! ## Wire shapes (keys camelCase matching capi/neon wire contract)
//!
//! ### `get_session` opts
//! ```json
//! {"lastN": N}
//! ```
//!
//! ### `add_feedback` opts
//! ```json
//! {"feedbackText": "...", "feedbackScore": N}
//! ```

use cognee_lib::session::get_session;

use crate::{HandleState, SdkError};

/// Retrieve QA history entries for a session.
///
/// `opts` may be `serde_json::Value::Null` or an object with optional `"lastN"` (integer).
/// Returns a JSON array of `SessionQAEntry` objects.
pub async fn run_get_session(
    state: &HandleState,
    session_id: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let last_n = opts
        .get("lastN")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let entries = get_session(
        svc.session_store.as_ref(),
        session_id,
        Some(&owner_str),
        last_n,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("get_session failed: {e}")))?;

    serde_json::to_value(&entries)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize SessionQAEntry[]: {e}")))
}

/// Attach feedback to a QA entry.
///
/// `feedback_text` and `feedback_score` are extracted from `opts` using the
/// camelCase keys `"feedbackText"` and `"feedbackScore"`.
/// Returns `serde_json::Value::Bool(true/false)`.
pub async fn run_add_feedback(
    state: &HandleState,
    session_id: &str,
    qa_id: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let feedback_text = opts
        .get("feedbackText")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let feedback_score = opts
        .get("feedbackScore")
        .and_then(|v| v.as_i64())
        .map(|n| n as i32);

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let ok = cognee_lib::session::add_feedback(
        svc.session_manager.as_ref(),
        session_id,
        qa_id,
        Some(&owner_str),
        feedback_text.as_deref(),
        feedback_score,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("add_feedback failed: {e}")))?;

    Ok(serde_json::Value::Bool(ok))
}

/// Remove feedback from a QA entry.
///
/// Returns `serde_json::Value::Bool(true/false)`.
pub async fn run_delete_feedback(
    state: &HandleState,
    session_id: &str,
    qa_id: &str,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let ok = cognee_lib::session::delete_feedback(
        svc.session_manager.as_ref(),
        session_id,
        qa_id,
        Some(&owner_str),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("delete_feedback failed: {e}")))?;

    Ok(serde_json::Value::Bool(ok))
}

/// Retrieve the graph context snapshot for a session.
///
/// Returns `serde_json::Value::String(ctx)` when a context is set,
/// or `serde_json::Value::Null` when absent.
pub async fn run_get_graph_context(
    state: &HandleState,
    session_id: &str,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let ctx = cognee_lib::session::get_graph_context(
        svc.session_manager.as_ref(),
        session_id,
        Some(&owner_str),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("get_graph_context failed: {e}")))?;

    match ctx {
        Some(s) => Ok(serde_json::Value::String(s)),
        None => Ok(serde_json::Value::Null),
    }
}

/// Store a graph context snapshot for a session.
///
/// Returns `serde_json::Value::Null` (void op).
pub async fn run_set_graph_context(
    state: &HandleState,
    session_id: &str,
    context: &str,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    cognee_lib::session::set_graph_context(
        svc.session_manager.as_ref(),
        session_id,
        Some(&owner_str),
        context,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("set_graph_context failed: {e}")))?;

    Ok(serde_json::Value::Null)
}
