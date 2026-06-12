//! Phase 5 — admin / session / pipeline-run / user / notebook ops.
//!
//! Covers:
//!   - Pipeline-run resets (#14)
//!   - Default user (#15)
//!   - Notebooks (#16)
//!   - Session CRUD (#13)
//!
//! Delegates to the shared async logic in
//! `cognee_bindings_common::ops::{admin, sessions}`.

use std::sync::Arc;

use neon::prelude::*;

use cognee_bindings_common::ops::admin;
use cognee_bindings_common::ops::sessions;

use crate::errors::{SdkError, throw_sdk_error};
use crate::json::{js_to_value, parse_js, read_opts};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;

// ---------------------------------------------------------------------------
// Pipeline-run resets (#14)
// ---------------------------------------------------------------------------

/// `cogneeResetPipelineRunStatus(handle, datasetId, pipelineName) -> Promise<void>`
pub fn cognee_reset_pipeline_run_status(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);
    let pipeline_name = cx.argument::<JsString>(2)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = admin::run_reset_pipeline_run_status(&state, &dataset_id_str, &pipeline_name)
            .await
            .map(|_| ());
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(()) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeResetDatasetPipelineRunStatus(handle, datasetId) -> Promise<void>`
pub fn cognee_reset_dataset_pipeline_run_status(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = admin::run_reset_dataset_pipeline_run_status(&state, &dataset_id_str)
            .await
            .map(|_| ());
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(()) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// Default user (#15)
// ---------------------------------------------------------------------------

/// `cogneeGetOrCreateDefaultUser(handle) -> Promise<User>`
///
/// `cognee_models::User` derives `Serialize` → direct serde.
pub fn cognee_get_or_create_default_user(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = admin::run_get_or_create_default_user(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).unwrap_or_else(|_| "null".to_string());
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// Notebooks (#16)
// ---------------------------------------------------------------------------

/// `cogneeListNotebooks(handle) -> Promise<Notebook[]>`
///
/// Seeds tutorial notebooks on the very first call for a new user (idempotent).
/// `Notebook` derives `Serialize` → direct serde.
pub fn cognee_list_notebooks(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = admin::run_list_notebooks(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).unwrap_or_else(|_| "[]".to_string());
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeCreateNotebook(handle, name, cells?, deletable?) -> Promise<Notebook>`
pub fn cognee_create_notebook(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let name = cx.argument::<JsString>(1)?.value(&mut cx);

    // cells: optional JSON array (default empty array)
    let cells_json: serde_json::Value = match cx.argument_opt(2) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Array(vec![]),
    };

    // deletable: optional boolean (ignored — forced to true by Python parity)
    let deletable = match cx.argument_opt(3) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => arg
            .downcast::<JsBoolean, _>(&mut cx)
            .map(|b| b.value(&mut cx))
            .unwrap_or(true),
        _ => true,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = admin::run_create_notebook(&state, name, cells_json, deletable).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).unwrap_or_else(|_| "null".to_string());
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeUpdateNotebook(handle, id, patch) -> Promise<Notebook | null>`
///
/// `patch`: `{ name?: string, cells?: any }`
/// `NotebookUpdatePatch` does NOT derive `Serialize` — marshalled from JS by hand.
pub fn cognee_update_notebook(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let id_str = cx.argument::<JsString>(1)?.value(&mut cx);
    let patch_arg = cx.argument::<JsValue>(2)?;
    let patch_json = js_to_value(&mut cx, patch_arg)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = admin::run_update_notebook(&state, &id_str, patch_json).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).unwrap_or_else(|_| "null".to_string());
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeDeleteNotebook(handle, id) -> Promise<boolean>`
pub fn cognee_delete_notebook(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = admin::run_delete_notebook(&state, &id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(serde_json::Value::Bool(removed)) => Ok(cx.boolean(removed)),
            Ok(_) => Ok(cx.boolean(false)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

// ---------------------------------------------------------------------------
// Session ops (#13)
// ---------------------------------------------------------------------------

/// `cogneeGetSession(handle, sessionId, opts?) -> Promise<SessionQAEntry[]>`
///
/// opts: `{ lastN?: number }`
/// `SessionQAEntry` derives `Serialize` → direct serde.
pub fn cognee_get_session(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let session_id = cx.argument::<JsString>(1)?.value(&mut cx);
    let opts = read_opts(&mut cx, 2)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = sessions::run_get_session(&state, &session_id, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(val) => {
                let json_str = serde_json::to_string(&val).unwrap_or_else(|_| "[]".to_string());
                parse_js(&mut cx, &json_str)
            }
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeAddFeedback(handle, sessionId, qaId, feedbackText?, feedbackScore?, opts?) -> Promise<boolean>`
pub fn cognee_add_feedback(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let session_id = cx.argument::<JsString>(1)?.value(&mut cx);
    let qa_id = cx.argument::<JsString>(2)?.value(&mut cx);

    let feedback_text: Option<String> = match cx.argument_opt(3) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            Some(
                arg.downcast::<JsString, _>(&mut cx)
                    .map(|s| s.value(&mut cx))
                    .unwrap_or_default(),
            )
        }
        _ => None,
    };

    let feedback_score: Option<i32> = match cx.argument_opt(4) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => arg
            .downcast::<JsNumber, _>(&mut cx)
            .map(|n| n.value(&mut cx) as i32)
            .ok(),
        _ => None,
    };

    // Build an opts value from the positional feedback args so the shared op receives them.
    let mut opts_map = serde_json::Map::new();
    if let Some(ref text) = feedback_text {
        opts_map.insert(
            "feedbackText".to_string(),
            serde_json::Value::String(text.clone()),
        );
    }
    if let Some(score) = feedback_score {
        opts_map.insert(
            "feedbackScore".to_string(),
            serde_json::Value::Number(score.into()),
        );
    }
    let opts_val = serde_json::Value::Object(opts_map);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = sessions::run_add_feedback(&state, &session_id, &qa_id, &opts_val).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(serde_json::Value::Bool(ok)) => Ok(cx.boolean(ok)),
            Ok(_) => Ok(cx.boolean(false)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeDeleteFeedback(handle, sessionId, qaId, opts?) -> Promise<boolean>`
pub fn cognee_delete_feedback(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let session_id = cx.argument::<JsString>(1)?.value(&mut cx);
    let qa_id = cx.argument::<JsString>(2)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = sessions::run_delete_feedback(&state, &session_id, &qa_id).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(serde_json::Value::Bool(ok)) => Ok(cx.boolean(ok)),
            Ok(_) => Ok(cx.boolean(false)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeGetGraphContext(handle, sessionId, opts?) -> Promise<string | null>`
pub fn cognee_get_graph_context(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let session_id = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = sessions::run_get_graph_context(&state, &session_id).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(serde_json::Value::String(ctx)) => Ok(cx.string(ctx).as_value(&mut cx)),
            Ok(serde_json::Value::Null) => Ok(cx.null().as_value(&mut cx)),
            Ok(_) => Ok(cx.null().as_value(&mut cx)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

/// `cogneeSetGraphContext(handle, sessionId, context, opts?) -> Promise<void>`
pub fn cognee_set_graph_context(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let session_id = cx.argument::<JsString>(1)?.value(&mut cx);
    let context = cx.argument::<JsString>(2)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = sessions::run_set_graph_context(&state, &session_id, &context).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(_) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}
