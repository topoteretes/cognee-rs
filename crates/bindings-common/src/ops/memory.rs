//! Shared async memory operations: `remember`, `remember_entry`, `memify`, `improve`.
//!
//! These functions contain the pure-Rust async logic that is shared between
//! every language binding surface (C API, Neon JS, Python). Each function takes
//! a [`HandleState`] reference and `serde_json::Value` arguments, performs the
//! operation against the underlying cognee APIs, and returns a
//! `serde_json::Value` result (or an [`SdkError`]).
//!
//! The binding-specific wrappers (C string parsing, Neon JS promise settling,
//! Python `future_into_py`, etc.) live in the individual binding crates and
//! call through to these shared functions.
//!
//! ## Input wire shapes (opts / entry keys are camelCase)
//!
//! The argument objects below use camelCase keys. Result shapes differ by op:
//! `memify` / `improve` are hand-built as camelCase (see [`memify_result_json`]
//! and [`improve_result_json`]),
//! but `remember` / `remember_entry` pass the `RememberResult` struct straight
//! through `serde_json::to_value`, so their result keys are **snake_case** —
//! deliberately, to preserve Python-SDK wire parity (Python's
//! `RememberResult.to_dict()` emits snake_case; the HTTP v2 remember DTO makes
//! the same carve-out). See issue #46.
//!
//! ### `remember` opts
//! ```json
//! {"sessionId":"...", "selfImprovement":bool, "tenant":"<uuid>",
//!  "extractor":"auto"|"llm"|"gliner", "graphBackend":...}
//! ```
//!
//! `extractor` (Python's argument) and `graphBackend` select the graph
//! extractor as for `cognify`; `extractor` with `sessionId` is an error, as in
//! Python, since a session-scoped remember does not cognify.
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
//!
//! ### `improve` result
//! ```json
//! {"datasetId":"<uuid>"|null, "datasetName":"...", "sessionIds":["..."],
//!  "stages":[{"stage":"feedback_weights", "status":"completed"|"already_completed"|
//!             "skipped"|"errored", "reason":...|null, "error":...|null,
//!             "counts":{"sessions":1}, "durationMs":N}, ...],
//!  "background":false, "finished":true, "error":null,
//!  "status":"completed"|"errored"|"skipped"|"running"}
//! ```
//!
//! `stages` holds one entry per improve stage, in registry order. A fatal
//! `persist_session_qa` failure is an error, not a result.

use std::sync::Arc;

use serde_json::json;

use cognee::api::{ImproveParams, ImproveResult, remember, remember_entry};
use cognee::cognify::{MemifyConfig, run_memify};
use cognee::database::PipelineRunRepository;
use cognee::models::{FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};

use crate::wire::marshal_inputs;
use crate::{HandleState, SdkError};

// ---------------------------------------------------------------------------
// opts helpers.
// ---------------------------------------------------------------------------

/// Parse an optional `tenant` UUID string out of an `opts` object.
///
/// Re-exported from `ops::pipeline` rather than copied: three identical copies
/// of this parser existed, all silently treating a present non-string `tenant`
/// as absent, and a fix to one would not have reached the others.
use super::pipeline::opts_tenant;

// ---------------------------------------------------------------------------
// MemifyResult JSON helper (MemifyResult does not derive Serialize).
// ---------------------------------------------------------------------------

/// Serialise a [`cognee::cognify::MemifyResult`] to a camelCase JSON value.
pub fn memify_result_json(r: &cognee::cognify::MemifyResult) -> serde_json::Value {
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
    if session_id.is_some()
        && opts
            .get(crate::graph_backend::EXTRACTOR_OPT)
            .is_some_and(|v| !v.is_null())
    {
        return Err(SdkError::Validation(
            "`extractor` is not supported when `sessionId` is provided".to_string(),
        ));
    }
    let self_improvement = opts
        .get("selfImprovement")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    // Resolved before `remember` writes anything: an unregistered extractor
    // kind fails here instead of after ingestion. A session-scoped remember
    // does not cognify, so it resolves none (with no LLM key, `auto` would
    // otherwise fail it over a GLiNER extractor it never uses).
    let cognify_config = Arc::new(if session_id.is_some() {
        svc.cognify_config.clone()
    } else {
        super::pipeline::cognify_config_with_graph_backend(svc.as_ref(), opts).await?
    });

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

    crate::ops::lifecycle::checkpoint_graph_after(svc.as_ref(), "remember").await;

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
        Arc::clone(&svc.database).pipeline_runs();

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
    let cognify_config =
        super::pipeline::cognify_config_with_graph_backend(svc.as_ref(), opts).await?;

    let result = cognee::api::improve(ImproveParams {
        dataset_name,
        dataset_id: None,
        session_ids,
        node_name,
        owner_id,
        tenant_id,
        feedback_alpha,
        extraction_tasks: None,
        enrichment_tasks: None,
        data: None,
        build_global_context_index: false,
        build_truth_subspace: false,
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
        cognify_config: &cognify_config,
    })
    .await
    .map_err(improve_error)?;

    crate::ops::lifecycle::checkpoint_graph_after(svc.as_ref(), "improve").await;

    Ok(improve_result_json(&result))
}

/// The binding error of a failed `improve()`. A fatal-stage abort keeps its
/// partial result ([`SdkError::ImproveAborted`]), as Python's raised error
/// carries `improve_result`; anything else is a runtime error.
fn improve_error(error: cognee::api::ApiError) -> SdkError {
    match error {
        cognee::api::ApiError::ImproveAborted { result, .. } => SdkError::ImproveAborted {
            message: result
                .error
                .clone()
                .unwrap_or_else(|| "a fatal improve stage errored".to_string()),
            improve_result: Box::new(improve_result_json(&result)),
        },
        other => SdkError::Runtime(format!("improve failed: {other}")),
    }
}

/// Serialise an [`ImproveResult`] to the binding wire shape: Python's
/// `ImproveResult` with camelCase keys, one entry per stage in registry order
/// and the computed `status`.
pub fn improve_result_json(result: &ImproveResult) -> serde_json::Value {
    let stages: Vec<serde_json::Value> = result
        .stages
        .iter()
        .map(|stage| {
            json!({
                "stage": stage.stage,
                "status": stage.status.as_str(),
                "reason": stage.reason,
                "error": stage.error,
                "counts": stage.counts,
                "durationMs": stage.duration_ms,
            })
        })
        .collect();

    json!({
        "datasetId": result.dataset_id.map(|id| id.to_string()),
        "datasetName": result.dataset_name,
        "sessionIds": result.session_ids,
        "stages": stages,
        "background": result.background,
        "finished": result.finished,
        "error": result.error,
        "status": result.status().as_str(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use cognee::api::StageResult;

    // Python attaches `improve_result` to the error a fatal-stage abort
    // raises; the binding error keeps it in the wire shape.
    #[test]
    fn an_aborted_improve_keeps_its_partial_result() {
        let mut result = ImproveResult::new("docs", vec!["s1".to_string()]);
        result
            .stages
            .push(StageResult::errored("persist_session_qa", "boom"));
        result.error = Some("boom".to_string());
        let error = improve_error(cognee::api::ApiError::ImproveAborted {
            result: Box::new(result.clone()),
            budget_exhausted: false,
        });
        assert_eq!(error.code(), "IMPROVE_ABORTED");
        assert_eq!(error.to_string(), "improve failed: boom");
        assert_eq!(error.improve_result(), Some(&improve_result_json(&result)));
        assert!(
            error
                .detailed_message()
                .starts_with("improve failed: boom\nimprove_result: {")
        );

        let other = improve_error(cognee::api::ApiError::Improve("lookup".to_string()));
        assert_eq!(other.code(), "RUNTIME_ERROR");
        assert!(other.improve_result().is_none());
    }

    #[test]
    fn improve_result_json_is_camel_case_with_the_computed_status() {
        let mut result = ImproveResult::new("docs", vec!["s1".to_string()]);
        result
            .stages
            .push(StageResult::skipped("review_conflicts", "opt_in_disabled"));
        let wire = improve_result_json(&result);
        assert_eq!(
            wire,
            json!({
                "datasetId": null,
                "datasetName": "docs",
                "sessionIds": ["s1"],
                "stages": [{"stage": "review_conflicts", "status": "skipped",
                            "reason": "opt_in_disabled", "error": null, "counts": {},
                            "durationMs": 0}],
                "background": false,
                "finished": true,
                "error": null,
                "status": "skipped",
            })
        );
    }
}
