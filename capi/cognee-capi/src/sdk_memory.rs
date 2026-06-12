//! Phase 6 — memory ops: `cg_sdk_remember`, `cg_sdk_remember_entry`,
//! `cg_sdk_memify`, `cg_sdk_improve`.
//!
//! All four follow the Phase-4 canonical pattern:
//!   `Arc::clone(&(*sdk).state)` → `spawn_sdk_op` → `state.services().await?`
//!   → call cognee-lib API → serialize result → callback.
//!
//! ## Serde notes
//! - `RememberResult` derives `Serialize` → direct `serde_json::to_value`.
//!   `cognify_result`/`memify_result` fields carry `#[serde(skip)]` so they
//!   are invisible after serialisation.
//! - `MemifyResult` and `ImproveResult` do NOT derive `Serialize` → hand-built
//!   JSON via the local `memify_result_json` helper.

use std::ffi::c_char;
use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use cognee_bindings_common::wire::marshal_inputs;
use cognee_bindings_common::{HandleState, SdkError};
use cognee_lib::api::{ImproveParams, remember, remember_entry};
use cognee_lib::cognify::{MemifyConfig, run_memify};
use cognee_lib::database::{PipelineRunRepository, SeaOrmPipelineRunRepository};
use cognee_lib::models::{FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};

use crate::sdk::{CgSdk, CgSdkResultCallback, SendUserData, spawn_sdk_op};
use crate::sdk_ops::parse_c_str_or_fire;

// ---------------------------------------------------------------------------
// opts helpers (local copy, NOT in bindings-common — same decision as neon).
// ---------------------------------------------------------------------------

fn opts_tenant(opts: &serde_json::Value) -> Result<Option<Uuid>, SdkError> {
    match opts.get("tenant").and_then(|v| v.as_str()) {
        Some(s) => Uuid::parse_str(s)
            .map(Some)
            .map_err(|e| SdkError::Validation(format!("invalid `tenant` UUID: {e}"))),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// MemifyResult JSON helper (not Serialize — hand-built).
// Local to this module (same as neon's sdk_memory.rs).
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
// MemoryEntry marshalling.
// ---------------------------------------------------------------------------

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
// Core async logic.
// ---------------------------------------------------------------------------

async fn run_remember(
    state: &HandleState,
    inputs_json: serde_json::Value,
    dataset_name: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
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

    serde_json::to_value(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize RememberResult: {e}")))
}

async fn run_remember_entry(
    state: &HandleState,
    entry_json: serde_json::Value,
    dataset_name: &str,
    session_id: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
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

    serde_json::to_value(&result)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize RememberResult: {e}")))
}

async fn run_memify_op(
    state: &HandleState,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
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

    Ok(memify_result_json(&result))
}

async fn run_improve(
    state: &HandleState,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
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

    Ok(json!({
        "stagesRun": result.stages_run,
        "memifyResult": memify_json,
        "feedbackEntriesProcessed": result.feedback_entries_processed,
        "feedbackEntriesApplied": result.feedback_entries_applied,
        "sessionsPersisted": result.sessions_persisted,
        "edgesSynced": result.edges_synced,
    }))
}

// ---------------------------------------------------------------------------
// C-exported functions.
// ---------------------------------------------------------------------------

/// Add data to memory: ingest inputs, cognify, persist session interaction.
///
/// `inputs_json` is a `CogneeDataInput` object **or** array.
/// `dataset_name` is the target dataset name.
/// `opts_json` may be `NULL` or a JSON object with optional fields:
///   `"sessionId"` (string), `"selfImprovement"` (boolean), `"tenant"` (UUID string).
///
/// Async (D4, R1): the callback fires on a tokio worker thread, never
/// synchronously from this call.
///
/// On success `result_json` is a `RememberResult` JSON object.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `inputs_json` and `dataset_name`
/// must be valid null-terminated UTF-8 strings.  `opts_json` may be NULL.
/// `user_data` is forwarded to `callback` as-is.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_remember(
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
        run_remember(&state, inputs_val, &dataset_str, &opts_val).await
    });
}

/// Store a single memory entry (QA, trace, or feedback) into the session.
///
/// `entry_json` is a discriminated union on `"type"`:
///   `{"type":"qa", "question":"…", "answer":"…", …}`
///   `{"type":"trace", "originFunction":"…", "status":"…", …}`
///   `{"type":"feedback", "qaId":"…", …}`
/// `dataset_name` is the target dataset name (null-terminated UTF-8).
/// `session_id` is the session identifier (null-terminated UTF-8).
/// `opts_json` may be `NULL` or a JSON object with optional `"tenant"` (UUID string).
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk`, `entry_json`, `dataset_name`, `session_id` must be valid non-null
/// null-terminated UTF-8 strings.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_remember_entry(
    sdk: *const CgSdk,
    entry_json: *const c_char,
    dataset_name: *const c_char,
    session_id: *const c_char,
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

    let entry_str = match parse_c_str_or_fire(entry_json, "entry_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let dataset_str = match parse_c_str_or_fire(dataset_name, "dataset_name", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };
    let session_str = match parse_c_str_or_fire(session_id, "session_id", callback, ud_raw) {
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
        let entry_val: serde_json::Value = serde_json::from_str(&entry_str)
            .map_err(|e| SdkError::Validation(format!("entry_json parse error: {e}")))?;
        let opts_val: serde_json::Value = match opts_str {
            Some(ref s) => serde_json::from_str(s)
                .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?,
            None => serde_json::Value::Null,
        };
        run_remember_entry(&state, entry_val, &dataset_str, &session_str, &opts_val).await
    });
}

/// Run the memify pipeline: create triplet embeddings for all graph edges and
/// index them in the vector DB.  Idempotent (safe to re-run).
///
/// `opts_json` may be `NULL` or a JSON object with optional fields:
///   `"tripletBatchSize"` (integer), `"nodeTypeFilter"` (string),
///   `"nodeNameFilter"` (string array), `"nodeNameFilterOperator"` (string).
///
/// On success `result_json` is a JSON object:
///   `{"tripletCount":N,"indexedCount":N,"batchCount":N,"alreadyCompleted":bool,
///     "priorPipelineRunId":null|"<uuid>"}`
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `opts_json` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_memify(
    sdk: *const CgSdk,
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
        run_memify_op(&state, &opts_val).await
    });
}

/// Apply graph improvement based on session feedback.
///
/// `opts_json` is a JSON object with required field `"datasetName"` and optional:
///   `"sessionIds"` (string array), `"nodeName"` (string array),
///   `"feedbackAlpha"` (float, default 0.1), `"tenant"` (UUID string).
///
/// On success `result_json` is a JSON object:
///   `{"stagesRun":[…],"memifyResult":…,"feedbackEntriesProcessed":N,
///     "feedbackEntriesApplied":N,"sessionsPersisted":N,"edgesSynced":N}`
///
/// Async (D4, R1): callback fires on a tokio worker thread.
///
/// # Safety
/// `sdk` must be a valid `CgSdk*` or NULL.  `opts_json` must be a valid
/// non-null null-terminated UTF-8 JSON string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_sdk_improve(
    sdk: *const CgSdk,
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

    let opts_str = match parse_c_str_or_fire(opts_json, "opts_json", callback, ud_raw) {
        Some(s) => s,
        None => return,
    };

    let ud = SendUserData(user_data);
    spawn_sdk_op(callback, ud, async move {
        let opts_val: serde_json::Value = serde_json::from_str(&opts_str)
            .map_err(|e| SdkError::Validation(format!("opts_json parse error: {e}")))?;
        run_improve(&state, &opts_val).await
    });
}
