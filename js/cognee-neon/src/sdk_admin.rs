//! Phase 5 — admin / session / pipeline-run / user / notebook ops.
//!
//! Covers:
//!   - Pipeline-run resets (#14)
//!   - Default user (#15)
//!   - Notebooks (#16)
//!   - Session CRUD (#13)

use std::sync::Arc;

use neon::prelude::*;
use uuid::Uuid;

use cognee_lib::api::get_or_create_default_user;
use cognee_lib::api::notebooks::{
    create_notebook, delete_notebook, list_notebooks, update_notebook,
};
use cognee_lib::api::{reset_dataset_pipeline_run_status, reset_pipeline_run_status};
use cognee_lib::database::{NotebookDb, NotebookUpdatePatch, UserDb};
use cognee_lib::session::get_session;

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
        let result = run_reset_pipeline_run_status(&state, &dataset_id_str, &pipeline_name).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(()) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_reset_pipeline_run_status(
    state: &crate::sdk::HandleState,
    dataset_id_str: &str,
    pipeline_name: &str,
) -> Result<(), SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    reset_pipeline_run_status(
        Arc::clone(&svc.pipeline_run_repo),
        owner_id,
        dataset_id,
        pipeline_name,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("reset_pipeline_run_status failed: {e}")))
}

/// `cogneeResetDatasetPipelineRunStatus(handle, datasetId) -> Promise<void>`
pub fn cognee_reset_dataset_pipeline_run_status(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let dataset_id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_reset_dataset_pipeline_run_status(&state, &dataset_id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(()) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_reset_dataset_pipeline_run_status(
    state: &crate::sdk::HandleState,
    dataset_id_str: &str,
) -> Result<(), SdkError> {
    let dataset_id = Uuid::parse_str(dataset_id_str)
        .map_err(|e| SdkError::Validation(format!("invalid dataset id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    reset_dataset_pipeline_run_status(Arc::clone(&svc.pipeline_run_repo), owner_id, dataset_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("reset_dataset_pipeline_run_status failed: {e}")))
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
        let result = run_get_or_create_default_user(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_get_or_create_default_user(
    state: &crate::sdk::HandleState,
) -> Result<String, SdkError> {
    let email = state.cm.settings().default_user_email.clone();
    let svc = state.services().await?;

    let user =
        get_or_create_default_user(Arc::clone(&svc.database).as_ref() as &dyn UserDb, &email)
            .await
            .map_err(|e| {
                SdkError::UserBootstrap(format!("get_or_create_default_user failed: {e}"))
            })?;

    serde_json::to_string(&user)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize User: {e}")))
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
        let result = run_list_notebooks(&state).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_list_notebooks(state: &crate::sdk::HandleState) -> Result<String, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    let notebooks = list_notebooks(&nb_db, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("list_notebooks failed: {e}")))?;

    serde_json::to_string(&notebooks)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize notebooks: {e}")))
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
        let result = run_create_notebook(&state, name, cells_json, deletable).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_create_notebook(
    state: &crate::sdk::HandleState,
    name: String,
    cells: serde_json::Value,
    deletable: bool,
) -> Result<String, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    let notebook = create_notebook(&nb_db, owner_id, name, cells, deletable)
        .await
        .map_err(|e| SdkError::Runtime(format!("create_notebook failed: {e}")))?;

    serde_json::to_string(&notebook)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize Notebook: {e}")))
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
        let result = run_update_notebook(&state, &id_str, patch_json).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_update_notebook(
    state: &crate::sdk::HandleState,
    id_str: &str,
    patch_json: serde_json::Value,
) -> Result<String, SdkError> {
    let id = Uuid::parse_str(id_str)
        .map_err(|e| SdkError::Validation(format!("invalid notebook id UUID: {e}")))?;

    let patch = NotebookUpdatePatch {
        name: patch_json
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        cells: patch_json.get("cells").cloned(),
    };

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    let result = update_notebook(&nb_db, id, owner_id, patch)
        .await
        .map_err(|e| SdkError::Runtime(format!("update_notebook failed: {e}")))?;

    match result {
        Some(nb) => serde_json::to_string(&nb)
            .map_err(|e| SdkError::Runtime(format!("failed to serialize Notebook: {e}"))),
        None => Ok("null".to_string()),
    }
}

/// `cogneeDeleteNotebook(handle, id) -> Promise<boolean>`
pub fn cognee_delete_notebook(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let id_str = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_delete_notebook(&state, &id_str).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(removed) => Ok(cx.boolean(removed)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_delete_notebook(
    state: &crate::sdk::HandleState,
    id_str: &str,
) -> Result<bool, SdkError> {
    let id = Uuid::parse_str(id_str)
        .map_err(|e| SdkError::Validation(format!("invalid notebook id UUID: {e}")))?;
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let nb_db: Arc<dyn NotebookDb> = Arc::clone(&svc.database) as Arc<dyn NotebookDb>;

    delete_notebook(&nb_db, id, owner_id)
        .await
        .map_err(|e| SdkError::Runtime(format!("delete_notebook failed: {e}")))
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
        let result = run_get_session(&state, &session_id, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_get_session(
    state: &crate::sdk::HandleState,
    session_id: &str,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
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

    serde_json::to_string(&entries)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize SessionQAEntry[]: {e}")))
}

/// `cogneeAddFeedback(handle, sessionId, qaId, feedbackText?, feedbackScore?, opts?) -> Promise<boolean>`
pub fn cognee_add_feedback(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let session_id = cx.argument::<JsString>(1)?.value(&mut cx);
    let qa_id = cx.argument::<JsString>(2)?.value(&mut cx);

    let feedback_text = match cx.argument_opt(3) {
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

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result =
            run_add_feedback(&state, &session_id, &qa_id, feedback_text, feedback_score).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(ok) => Ok(cx.boolean(ok)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_add_feedback(
    state: &crate::sdk::HandleState,
    session_id: &str,
    qa_id: &str,
    feedback_text: Option<String>,
    feedback_score: Option<i32>,
) -> Result<bool, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    cognee_lib::session::add_feedback(
        svc.session_manager.as_ref(),
        session_id,
        qa_id,
        Some(&owner_str),
        feedback_text.as_deref(),
        feedback_score,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("add_feedback failed: {e}")))
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
        let result = run_delete_feedback(&state, &session_id, &qa_id).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(ok) => Ok(cx.boolean(ok)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_delete_feedback(
    state: &crate::sdk::HandleState,
    session_id: &str,
    qa_id: &str,
) -> Result<bool, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    cognee_lib::session::delete_feedback(
        svc.session_manager.as_ref(),
        session_id,
        qa_id,
        Some(&owner_str),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("delete_feedback failed: {e}")))
}

/// `cogneeGetGraphContext(handle, sessionId, opts?) -> Promise<string | null>`
pub fn cognee_get_graph_context(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let session_id = cx.argument::<JsString>(1)?.value(&mut cx);

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_get_graph_context(&state, &session_id).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(Some(ctx)) => Ok(cx.string(ctx).as_value(&mut cx)),
            Ok(None) => Ok(cx.null().as_value(&mut cx)),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_get_graph_context(
    state: &crate::sdk::HandleState,
    session_id: &str,
) -> Result<Option<String>, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    cognee_lib::session::get_graph_context(
        svc.session_manager.as_ref(),
        session_id,
        Some(&owner_str),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("get_graph_context failed: {e}")))
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
        let result = run_set_graph_context(&state, &session_id, &context).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(()) => Ok(cx.undefined()),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_set_graph_context(
    state: &crate::sdk::HandleState,
    session_id: &str,
    context: &str,
) -> Result<(), SdkError> {
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
    .map_err(|e| SdkError::Runtime(format!("set_graph_context failed: {e}")))
}
