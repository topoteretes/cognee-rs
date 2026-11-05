//! Shared async memory operations: `remember`, `remember_entry`, `improve`.
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
//! `improve` is hand-built as camelCase (see [`improve_result_json`]),
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
//! ### `improve` opts (required: `datasetName`)
//! ```json
//! {"datasetName":"...", "datasetId":"<uuid>", "sessionIds":["..."],
//!  "feedbackAlpha":0.1, "tenant":"<uuid>", "buildGlobalContextIndex":bool,
//!  "buildTruthSubspace":bool, "runInBackground":bool}
//! ```
//!
//! `buildGlobalContextIndex` / `buildTruthSubspace` opt in to those stages
//! (default `false`). With `runInBackground: true` the op returns at once with
//! a `running` result (`background: true`, `finished: false`, no stages) and
//! the run continues as a process-wide background task; wait for it with
//! `waitForBackgroundTasks`. There is no per-run handle.
//!
//! `datasetId` (optional) targets a dataset the caller already resolved and
//! authorized, possibly one shared with it, as Python's
//! `improve(dataset=<UUID>)`; without it `datasetName` is looked up among the
//! caller's own datasets.
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
    // Python's `remember(self_improvement=True)` improves unless told not to.
    let self_improvement = opts
        .get("selfImprovement")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

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

    let (improve_config, personalization, auto_feedback) = {
        let settings = state.cm.settings();
        (
            settings.auto_improve_config(),
            settings.preference_config(),
            settings.auto_feedback_enabled(),
        )
    };
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
        svc.ontology_resolver.clone(),
        cognify_config,
        improve_config,
        personalization,
        auto_feedback,
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

    // Optional id of a dataset the caller already resolved and authorized
    // (possibly owned by another user); Python `improve(dataset=<UUID>)`. A
    // present but non-string id is rejected rather than ignored: ignoring it
    // would silently fall back to the by-name lookup among the caller's own
    // datasets.
    let dataset_id = improve_dataset_id(opts)?;

    // Explicit wins; otherwise `improve()` uses `improve.feedback_alpha`.
    let feedback_alpha = opts.get("feedbackAlpha").and_then(|v| v.as_f64());
    let (improve_config, personalization, auto_feedback) = {
        let settings = state.cm.settings();
        let config = settings
            .improve_config()
            .map_err(|e| SdkError::Validation(e.to_string()))?;
        (
            config,
            settings.preference_config(),
            settings.auto_feedback_enabled(),
        )
    };

    let tenant_id = opts_tenant(opts)?;
    let flags = improve_flags(opts)?;

    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let cognify_config =
        super::pipeline::cognify_config_with_graph_backend(svc.as_ref(), opts).await?;

    if flags.run_in_background {
        // As Python's `improve(run_in_background=True)`: hand back a
        // `running` result now and let the run finish on its own. Callers wait
        // with `waitForBackgroundTasks`. The spawned task owns everything
        // `ImproveParams` borrows.
        let running = ImproveResult::running_in_background(
            Some(dataset_id.unwrap_or_else(|| {
                cognee::add::generate_dataset_id(&dataset_name, owner_id, tenant_id)
            })),
            dataset_name.clone(),
            session_ids.clone().unwrap_or_default(),
        );
        cognee::spawn_background_task(async move {
            let params = ImproveParams {
                dataset_name: dataset_name.clone(),
                dataset_id,
                session_ids,
                owner_id,
                tenant_id,
                feedback_alpha,
                config: improve_config,
                personalization,
                auto_feedback,
                build_global_context_index: flags.build_global_context_index,
                build_truth_subspace: flags.build_truth_subspace,
                run_in_background: true,
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
                dataset_locks: None,
                cognify_config: &cognify_config,
            };
            match cognee::api::improve(params).await {
                Ok(result) => tracing::info!(
                    dataset = %dataset_name,
                    status = result.status().as_str(),
                    stages = %result.stage_summary(),
                    "background improve finished"
                ),
                // A fatal-stage abort (`ApiError::ImproveAborted`) included.
                Err(e) => tracing::warn!(
                    dataset = %dataset_name,
                    "background improve failed: {e}"
                ),
            }
            crate::ops::lifecycle::checkpoint_graph_after(svc.as_ref(), "improve").await;
        });
        return Ok(improve_result_json(&running));
    }

    let result = cognee::api::improve(ImproveParams {
        dataset_name,
        dataset_id,
        session_ids,
        owner_id,
        tenant_id,
        feedback_alpha,
        config: improve_config,
        personalization,
        auto_feedback,
        build_global_context_index: flags.build_global_context_index,
        build_truth_subspace: flags.build_truth_subspace,
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
        // The add pipeline's registry (none here), else the process-wide one.
        dataset_locks: None,
        cognify_config: &cognify_config,
    })
    .await
    .map_err(improve_error)?;

    crate::ops::lifecycle::checkpoint_graph_after(svc.as_ref(), "improve").await;

    Ok(improve_result_json(&result))
}

/// The boolean `improve` opts that are not part of the dataset selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ImproveFlags {
    run_in_background: bool,
    build_global_context_index: bool,
    build_truth_subspace: bool,
}

/// Parse `runInBackground`, `buildGlobalContextIndex` and `buildTruthSubspace`
/// (all default `false`); a present non-boolean is rejected.
fn improve_flags(opts: &serde_json::Value) -> Result<ImproveFlags, SdkError> {
    let flag = |key: &str| super::pipeline::opt_bool(opts, key).map(Option::unwrap_or_default);
    Ok(ImproveFlags {
        run_in_background: flag("runInBackground")?,
        build_global_context_index: flag("buildGlobalContextIndex")?,
        build_truth_subspace: flag("buildTruthSubspace")?,
    })
}

/// The `datasetId` improve opt: `None` when absent or null, else a UUID string.
fn improve_dataset_id(opts: &serde_json::Value) -> Result<Option<uuid::Uuid>, SdkError> {
    match opts.get("datasetId") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(raw)) => uuid::Uuid::parse_str(raw)
            .map(Some)
            .map_err(|e| SdkError::Validation(format!("`datasetId` is not a valid UUID: {e}"))),
        Some(other) => Err(SdkError::Validation(format!(
            "`datasetId` must be a UUID string, got {other}"
        ))),
    }
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

    #[test]
    fn improve_flags_default_off_and_reach_each_field() {
        assert_eq!(improve_flags(&json!({})).unwrap(), ImproveFlags::default());
        assert_eq!(
            improve_flags(&json!({"runInBackground": null})).unwrap(),
            ImproveFlags::default()
        );
        let flags = improve_flags(&json!({
            "runInBackground": true,
            "buildGlobalContextIndex": true,
            "buildTruthSubspace": true,
        }))
        .unwrap();
        assert!(flags.run_in_background);
        assert!(flags.build_global_context_index);
        assert!(flags.build_truth_subspace);
        let only_truth = improve_flags(&json!({"buildTruthSubspace": true})).unwrap();
        assert!(only_truth.build_truth_subspace);
        assert!(!only_truth.build_global_context_index);
    }

    #[test]
    fn improve_flags_reject_non_booleans() {
        for key in [
            "runInBackground",
            "buildGlobalContextIndex",
            "buildTruthSubspace",
        ] {
            for bad in [json!("true"), json!(1)] {
                let err = improve_flags(&json!({ key: bad })).unwrap_err();
                assert!(matches!(err, SdkError::Validation(_)), "{err:?}");
                assert!(err.to_string().contains(key), "{err}");
            }
        }
    }

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

    // Python `improve(dataset=<UUID>)`: the id is taken as given; a value that
    // is not a UUID string is an error, never a silent fall back to the name.
    #[test]
    fn improve_dataset_id_opt() {
        let id = uuid::Uuid::new_v4();
        assert_eq!(improve_dataset_id(&json!({})).unwrap(), None);
        assert_eq!(
            improve_dataset_id(&json!({"datasetId": null})).unwrap(),
            None
        );
        assert_eq!(
            improve_dataset_id(&json!({"datasetId": id.to_string()})).unwrap(),
            Some(id)
        );
        for bad in [json!({"datasetId": "nope"}), json!({"datasetId": 7})] {
            assert!(matches!(
                improve_dataset_id(&bad),
                Err(SdkError::Validation(_))
            ));
        }
    }
}
