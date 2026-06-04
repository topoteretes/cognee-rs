//! Phase 5 — memory ops: remember / remember_entry / memify / improve.
//!
//! Each export follows the canonical pattern:
//!   `Arc::clone(&handle.state)` → `runtime().spawn` → `state.services().await?`
//!   → call cognee-lib API → `settle_with`.
//!
//! ## Serde notes
//! - `RememberResult` derives `Serialize` → direct `serde_json::to_value`.
//!   `cognify_result` / `memify_result` fields carry `#[serde(skip)]` so the
//!   serialized JSON covers only the public fields.
//! - `MemifyResult`, `ImproveResult` do NOT derive `Serialize` → hand-built JSON
//!   via the helpers below.

use std::sync::Arc;

use neon::prelude::*;
use serde_json::json;
use uuid::Uuid;

use cognee_lib::api::{ImproveParams, remember, remember_entry};
use cognee_lib::cognify::{MemifyConfig, run_memify};
use cognee_lib::database::{PipelineRunRepository, SeaOrmPipelineRunRepository};
use cognee_lib::models::{FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};

use crate::errors::{SdkError, throw_sdk_error};
use crate::json::{js_to_value, marshal_inputs, parse_js, read_opts};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;


/// Parse an optional `tenant` UUID string out of an `opts` object.
fn opts_tenant(opts: &serde_json::Value) -> Result<Option<Uuid>, SdkError> {
    match opts.get("tenant").and_then(|v| v.as_str()) {
        Some(s) => Uuid::parse_str(s)
            .map(Some)
            .map_err(|e| SdkError::Validation(format!("invalid `tenant` UUID: {e}"))),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// MemifyResult JSON helper (not Serialize).
// ---------------------------------------------------------------------------

fn memify_result_json(r: &cognee_lib::cognify::MemifyResult) -> serde_json::Value {
    json!({
        "tripletCount": r.triplet_count,
        "indexedCount": r.index_result.indexed_count,
        "batchCount": r.index_result.batch_count,
        "alreadyCompleted": r.already_completed,
        "priorPipelineRunId": r.prior_pipeline_run_id.map(|id| id.to_string()),
    })
}

// ---------------------------------------------------------------------------
// cogneeRemember
// ---------------------------------------------------------------------------

/// `cogneeRemember(handle, dataInput, datasetName, opts?) -> Promise<RememberResult>`
///
/// opts: `{ sessionId?, selfImprovement?, tenant? }`
///
/// `RememberResult` derives `Serialize` — result is passed through
/// `serde_json::to_value` directly (cognify_result / memify_result fields are
/// `#[serde(skip)]` so they are invisible to JS).
pub fn cognee_remember(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let data_arg = cx.argument::<JsValue>(1)?;
    let inputs_json = js_to_value(&mut cx, data_arg)?;
    let dataset_name = cx.argument::<JsString>(2)?.value(&mut cx);
    let opts = read_opts(&mut cx, 3)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_remember(&state, inputs_json, &dataset_name, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_remember(
    state: &crate::sdk::HandleState,
    inputs_json: serde_json::Value,
    dataset_name: &str,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let inputs = marshal_inputs(&inputs_json)?;
    let tenant_id = opts_tenant(opts)?;
    let session_id_owned = opts
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let session_id: Option<&str> = session_id_owned.as_deref();
    let self_improvement = opts
        .get("selfImprovement")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let cognify_config = Arc::new(svc.cognify_config.clone());

    let result = remember(
        inputs,
        dataset_name,
        session_id,
        self_improvement,
        owner_id,
        tenant_id,
        svc.add_pipeline.clone(),
        svc.llm.clone(),
        svc.storage.clone(),
        svc.graph_db.clone(),
        svc.vector_db.clone(),
        svc.embedding_engine.clone(),
        Some(svc.database.clone()),
        Some(svc.session_store.clone()),
        Some(svc.session_manager.clone()),
        Some(svc.checkpoint_store.clone()),
        svc.ontology_resolver.clone(),
        cognify_config,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("remember failed: {e}")))?;

    serde_json::to_string(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize RememberResult: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeRememberEntry
// ---------------------------------------------------------------------------

/// `cogneeRememberEntry(handle, entry, datasetName, sessionId, opts?) -> Promise<RememberResult>`
///
/// `entry` is a discriminated union:
///   `{ type: "qa", question, answer, context?, feedbackText?, feedbackScore?, usedGraphElementIds? }`
///   `{ type: "trace", originFunction, status, memoryQuery?, memoryContext?, methodParams?,
///      methodReturnValue?, errorMessage?, generateFeedbackWithLlm? }`
///   `{ type: "feedback", qaId, feedbackText?, feedbackScore? }`
pub fn cognee_remember_entry(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let entry_arg = cx.argument::<JsValue>(1)?;
    let entry_json = js_to_value(&mut cx, entry_arg)?;
    let dataset_name = cx.argument::<JsString>(2)?.value(&mut cx);
    let session_id = cx.argument::<JsString>(3)?.value(&mut cx);
    let opts = read_opts(&mut cx, 4)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result =
            run_remember_entry(&state, entry_json, &dataset_name, &session_id, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_remember_entry(
    state: &crate::sdk::HandleState,
    entry_json: serde_json::Value,
    dataset_name: &str,
    session_id: &str,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let tenant_id = opts_tenant(opts)?;
    let entry = marshal_memory_entry(&entry_json)?;

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let result = remember_entry(
        entry,
        dataset_name,
        session_id,
        owner_id,
        tenant_id,
        Some(svc.database.clone()),
        Some(svc.session_store.clone()),
        Some(svc.session_manager.clone()),
        Some(svc.llm.clone()),
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("remember_entry failed: {e}")))?;

    serde_json::to_string(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize RememberResult: {e}")))
}

/// Marshal a `{ type, … }` JSON object into a `MemoryEntry`.
fn marshal_memory_entry(value: &serde_json::Value) -> Result<MemoryEntry, SdkError> {
    let obj = value
        .as_object()
        .ok_or_else(|| SdkError::Validation("memory entry must be an object".to_string()))?;
    let ty = obj.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
        SdkError::Validation("memory entry is missing a string `type`".to_string())
    })?;

    match ty {
        "qa" => {
            let question = obj
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let answer = obj
                .get("answer")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let context = obj
                .get("context")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let feedback_text = obj
                .get("feedbackText")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let feedback_score = obj
                .get("feedbackScore")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32);
            let used_graph_element_ids = obj.get("usedGraphElementIds").cloned();
            Ok(MemoryEntry::Qa(QAEntry {
                question,
                answer,
                context,
                feedback_text,
                feedback_score,
                used_graph_element_ids,
            }))
        }
        "trace" => {
            let origin_function = obj
                .get("originFunction")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    SdkError::Validation("trace entry requires `originFunction`".to_string())
                })?
                .to_string();
            let status = obj
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("success")
                .to_string();
            let method_params = obj.get("methodParams").cloned();
            let method_return_value = obj.get("methodReturnValue").cloned();
            let memory_query = obj
                .get("memoryQuery")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let memory_context = obj
                .get("memoryContext")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let error_message = obj
                .get("errorMessage")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let generate_feedback_with_llm = obj
                .get("generateFeedbackWithLlm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(MemoryEntry::Trace(TraceEntry {
                origin_function,
                status,
                method_params,
                method_return_value,
                memory_query,
                memory_context,
                error_message,
                generate_feedback_with_llm,
            }))
        }
        "feedback" => {
            let qa_id = obj
                .get("qaId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| SdkError::Validation("feedback entry requires `qaId`".to_string()))?
                .to_string();
            let feedback_text = obj
                .get("feedbackText")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let feedback_score = obj
                .get("feedbackScore")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32);
            Ok(MemoryEntry::Feedback(FeedbackEntry {
                qa_id,
                feedback_text,
                feedback_score,
            }))
        }
        other => Err(SdkError::Validation(format!(
            "unknown memory entry type `{other}`. Valid: qa, trace, feedback"
        ))),
    }
}

// ---------------------------------------------------------------------------
// cogneeMemify
// ---------------------------------------------------------------------------

/// `cogneeMemify(handle, opts?) -> Promise<MemifyResult>`
///
/// opts: `{ tripletBatchSize?, nodeTypeFilter?, nodeNameFilter?, nodeNameFilterOperator? }`
///
/// NOTE: `extraction_tasks`, `enrichment_tasks`, `custom_data` fields in
/// `MemifyConfig` are `#[serde(skip)]` closures and cannot be passed from JS.
pub fn cognee_memify(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let opts = read_opts(&mut cx, 1)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_memify_op(&state, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_memify_op(
    state: &crate::sdk::HandleState,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let mut config = MemifyConfig::default();
    if let Some(n) = opts.get("tripletBatchSize").and_then(|v| v.as_u64()) {
        config = config.with_triplet_batch_size(n as usize);
    }
    if let Some(s) = opts.get("nodeTypeFilter").and_then(|v| v.as_str()) {
        config = config.with_node_type_filter(s.to_string());
    }
    if let Some(arr) = opts.get("nodeNameFilter").and_then(|v| v.as_array()) {
        let names: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect();
        config = config.with_node_name_filter(names);
    }
    if let Some(op) = opts.get("nodeNameFilterOperator").and_then(|v| v.as_str()) {
        config = config.with_node_name_filter_operator(op.to_string());
    }

    let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&svc.database)));

    let result = run_memify(
        svc.graph_db.clone(),
        svc.vector_db.clone(),
        svc.embedding_engine.clone(),
        svc.cpu_pool(),
        svc.database.clone(),
        pipeline_run_repo,
        None,
        Some(owner_id),
        None,
        &config,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("memify failed: {e}")))?;

    serde_json::to_string(&memify_result_json(&result))
        .map_err(|e| SdkError::Runtime(format!("failed to serialize MemifyResult: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeImprove
// ---------------------------------------------------------------------------

/// `cogneeImprove(handle, opts) -> Promise<ImproveResult>`
///
/// opts: `{ datasetName, sessionIds?, nodeName?, feedbackAlpha?, tenant? }`
///
/// `ImproveResult` does NOT derive `Serialize` → hand-built JSON.
pub fn cognee_improve(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);
    let opts = read_opts(&mut cx, 1)?;

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_improve(&state, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_improve(
    state: &crate::sdk::HandleState,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let dataset_name = opts
        .get("datasetName")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SdkError::Validation("`datasetName` is required for improve".to_string()))?
        .to_string();

    let session_ids: Option<Vec<String>> = opts.get("sessionIds").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
    });

    let node_name: Option<Vec<String>> = opts.get("nodeName").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
    });

    let feedback_alpha = opts
        .get("feedbackAlpha")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.1);

    let tenant_id = opts_tenant(opts)?;

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    // ImproveParams<'a> borrows &'a CognifyConfig and Option<&'a AddPipeline> —
    // keep svc in scope so the borrows outlive the improve() call.
    let result = cognee_lib::api::improve(ImproveParams {
        dataset_name,
        session_ids,
        node_name,
        owner_id,
        tenant_id,
        feedback_alpha,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        llm: svc.llm.clone(),
        storage: svc.storage.clone(),
        graph_db: svc.graph_db.clone(),
        vector_db: svc.vector_db.clone(),
        embedding_engine: svc.embedding_engine.clone(),
        ontology_resolver: svc.ontology_resolver.clone(),
        db: Some(svc.database.clone()),
        session_store: Some(svc.session_store.clone()),
        session_manager: Some(svc.session_manager.clone()),
        add_pipeline: Some(svc.add_pipeline.as_ref()),
        checkpoint_store: Some(svc.checkpoint_store.clone()),
        cognify_config: &svc.cognify_config,
    })
    .await
    .map_err(|e| SdkError::Runtime(format!("improve failed: {e}")))?;

    let memify_json = result
        .memify_result
        .as_ref()
        .map(memify_result_json)
        .unwrap_or(serde_json::Value::Null);

    let improve_json = json!({
        "stagesRun": result.stages_run,
        "memifyResult": memify_json,
        "feedbackEntriesProcessed": result.feedback_entries_processed,
        "feedbackEntriesApplied": result.feedback_entries_applied,
        "sessionsPersisted": result.sessions_persisted,
        "edgesSynced": result.edges_synced,
    });

    serde_json::to_string(&improve_json)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize ImproveResult: {e}")))
}

