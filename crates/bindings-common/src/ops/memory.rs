//! Shared async memory operations: `remember`, `remember_entry`, `memify`, `improve`.
//!
//! These functions contain the pure-Rust async logic that is shared between
//! every language binding surface (C API, Neon JS, Python). Each function takes
//! a [`HandleState`] reference and `serde_json::Value` arguments, performs the
//! operation against the underlying cognee-lib APIs, and returns a
//! `serde_json::Value` result (or an [`SdkError`]).
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.
//!
//! ## Wire shapes (all keys camelCase)
//!
//! ### `remember` opts
//! ```json
//! {"sessionId":"...", "selfImprovement":bool, "tenant":"<uuid>"}
//! ```
//!
//! ### `remember_entry` entry (discriminated union on `"type"`)
//! ```json
//! {"type":"qa", "question":"...", "answer":"...", "context":"...",
//!  "feedbackText":"...", "feedbackScore":N, "usedGraphElementIds":{...}}
//! {"type":"trace", "originFunction":"...", "status":"...",
//!  "methodParams":..., "methodReturnValue":...,
//!  "memoryQuery":"...", "memoryContext":"...", "errorMessage":"...",
//!  "generateFeedbackWithLlm":bool}
//! {"type":"feedback", "qaId":"...", "feedbackText":"...", "feedbackScore":N}
//! ```
//!
//! ### `memify` opts
//! ```json
//! {"tripletBatchSize":N, "nodeTypeFilter":"...", "nodeNameFilter":["..."],
//!  "nodeNameFilterOperator":"AND"|"OR"}
//! ```
//!
//! ### `improve` opts (required: `datasetName`)
//! ```json
//! {"datasetName":"...", "sessionIds":["..."], "nodeName":["..."],
//!  "feedbackAlpha":0.1, "tenant":"<uuid>"}
//! ```

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use cognee_lib::api::{ImproveParams, remember, remember_entry};
use cognee_lib::cognify::{MemifyConfig, run_memify};
use cognee_lib::database::{PipelineRunRepository, SeaOrmPipelineRunRepository};
use cognee_lib::models::{FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};

use crate::wire::marshal_inputs;
use crate::{HandleState, SdkError};

// ---------------------------------------------------------------------------
// opts helpers.
// ---------------------------------------------------------------------------

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
// MemifyResult JSON helper (MemifyResult does not derive Serialize).
// ---------------------------------------------------------------------------

/// Serialise a [`cognee_lib::cognify::MemifyResult`] to a camelCase JSON value.
pub fn memify_result_json(r: &cognee_lib::cognify::MemifyResult) -> serde_json::Value {
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

/// Marshal a `{ "type": "qa"|"trace"|"feedback", … }` JSON object into a
/// [`MemoryEntry`]. Returns [`SdkError::Validation`] for unknown types or
/// missing required fields.
pub fn marshal_memory_entry(value: &serde_json::Value) -> Result<MemoryEntry, SdkError> {
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
// Public top-level memory operations.
// ---------------------------------------------------------------------------

/// Run the `remember` pipeline: ingest inputs, cognify, optionally improve.
///
/// `inputs_json` must be a single `{ type, … }` object or an array of them.
/// `opts` may be `serde_json::Value::Null` when no options were provided.
pub async fn run_remember(
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

/// Store a single typed memory entry (QA, trace, or feedback).
///
/// `entry_json` is a discriminated-union object; `opts` may be
/// `serde_json::Value::Null` when no options were provided.
pub async fn run_remember_entry(
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

/// Run the memify pipeline: create triplet embeddings for all graph edges.
///
/// Idempotent — safe to re-run.  `opts` may be `serde_json::Value::Null`
/// when no options were provided.
pub async fn run_memify_op(
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

/// Apply graph improvement based on session feedback.
///
/// `opts` must contain `"datasetName"` (required); all other fields are
/// optional. `opts` may not be `serde_json::Value::Null` — callers must
/// validate that `datasetName` is present before or rely on the
/// [`SdkError::Validation`] returned here.
pub async fn run_improve(
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
        build_global_context_index: false,
        run_in_background: false,
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
