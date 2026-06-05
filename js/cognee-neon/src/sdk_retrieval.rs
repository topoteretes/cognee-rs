//! Phase 4 — retrieval ops: `search`, `recall`.
//!
//! Each export follows the canonical pattern:
//!   `Arc::clone(&handle.state)` → `runtime().spawn` → `state.services().await?`
//!   → call cognee-lib API → `settle_with`.
//!
//! ## Input marshalling
//!
//! `cogneeSearch` opts fields are camelCase JS keys that map to snake_case
//! `SearchRequest` fields; we hand-populate the struct rather than trying to
//! use `serde_json::from_value` on the whole opts object (which would require
//! matching serde names on both sides).
//!
//! `SearchType` is parsed from its SCREAMING_SNAKE_CASE serde wire name via
//! `serde_json::from_value(Value::String(s))` — the same path the HTTP server
//! uses, guaranteed to match the serde attribute.
//!
//! ## Result marshalling
//!
//! `SearchResponse` IS `Serialize` — pass through `serde_json::to_string` +
//! `parse_js`.
//!
//! `RecallResult` is NOT `Serialize` (derives only `Debug, Clone`) — hand-build
//! JSON from its fields: `items` (IS Serialize), `search_type_used` (IS
//! Serialize), `auto_routed` (bool), `search_response` (IS Serialize).

use std::sync::Arc;

use neon::prelude::*;
use serde_json::json;
use uuid::Uuid;

use cognee_lib::api::{ScopeInput, normalize_scope, recall};
use cognee_lib::search::{SearchRequest, SearchType};

use crate::errors::{SdkError, throw_sdk_error};
use crate::json::{js_to_value, parse_js};
use crate::runtime::runtime;
use crate::sdk::CogneeHandle;

// ---------------------------------------------------------------------------
// SearchType parsing.
// ---------------------------------------------------------------------------

/// Parse a `SearchType` from a SCREAMING_SNAKE_CASE wire string.
///
/// Uses `serde_json::from_value` so the exact path matches what the HTTP
/// server uses and is guaranteed to stay in sync with the `#[serde(rename_all
/// = "SCREAMING_SNAKE_CASE")]` attribute on `SearchType`.
fn parse_search_type(s: &str) -> Result<SearchType, SdkError> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(|_| {
        SdkError::Validation(format!(
            "unknown SearchType '{s}'. Valid values: SUMMARIES, CHUNKS, RAG_COMPLETION, \
             TRIPLET_COMPLETION, GRAPH_COMPLETION, GRAPH_SUMMARY_COMPLETION, CYPHER, \
             NATURAL_LANGUAGE, GRAPH_COMPLETION_COT, GRAPH_COMPLETION_CONTEXT_EXTENSION, \
             FEELING_LUCKY, FEEDBACK, TEMPORAL, CODING_RULES, CHUNKS_LEXICAL"
        ))
    })
}

// ---------------------------------------------------------------------------
// SearchRequest builder from opts.
// ---------------------------------------------------------------------------

/// Build a `SearchRequest` from the JS opts object (camelCase → snake_case).
///
/// `user_id` is always set from `owner_id` — required when `datasets` is
/// supplied (the orchestrator's dataset-resolution path errors with
/// `InvalidInput` when `user_id` is `None` and `datasets` is set).
fn build_search_request(
    query: &str,
    opts: &serde_json::Value,
    owner_id: Uuid,
) -> Result<SearchRequest, SdkError> {
    // searchType (default GRAPH_COMPLETION)
    let search_type = match opts.get("searchType").and_then(|v| v.as_str()) {
        Some(s) => parse_search_type(s)?,
        None => SearchType::default(),
    };

    // datasets: string array
    let datasets: Option<Vec<String>> = opts.get("datasets").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
    });

    // datasetIds: UUID string array
    let dataset_ids: Option<Vec<Uuid>> =
        opts.get("datasetIds")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().and_then(|s| Uuid::parse_str(s).ok()))
                    .collect()
            });

    // scalar opts
    let top_k = opts
        .get("topK")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let system_prompt = opts
        .get("systemPrompt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let session_id = opts
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let node_type = opts
        .get("nodeType")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let node_name: Option<Vec<String>> = opts.get("nodeName").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
    });
    let only_context = opts.get("onlyContext").and_then(|v| v.as_bool());
    let use_combined_context = opts.get("useCombinedContext").and_then(|v| v.as_bool());
    let verbose = opts.get("verbose").and_then(|v| v.as_bool());
    // save_interaction defaults to true (matching Python SDK behavior) when not set.
    let save_interaction = Some(
        opts.get("saveInteraction")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    );
    let auto_feedback_detection = opts.get("autoFeedbackDetection").and_then(|v| v.as_bool());

    Ok(SearchRequest {
        query_text: query.to_string(),
        search_type,
        top_k,
        datasets,
        dataset_ids,
        system_prompt,
        system_prompt_path: None,
        only_context,
        use_combined_context,
        session_id,
        node_type,
        node_name,
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction,
        // Always populate user_id from owner_id so dataset-name resolution works.
        user_id: Some(owner_id),
        verbose,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
    })
}

// ---------------------------------------------------------------------------
// ScopeInput builder from opts.
// ---------------------------------------------------------------------------

/// Build a `ScopeInput` from the `opts.scope` field (a string or string array).
///
/// Returns `None` when the field is absent (caller gets `[Auto]` from
/// `normalize_scope(None)`).
fn build_scope_input(opts: &serde_json::Value) -> Result<Option<ScopeInput>, SdkError> {
    match opts.get("scope") {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(ScopeInput::Single(s.clone()))),
        Some(serde_json::Value::Array(arr)) => {
            let strings: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            Ok(Some(ScopeInput::Many(strings)))
        }
        Some(other) => Err(SdkError::Validation(format!(
            "`scope` must be a string or string array, got: {other}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// cogneeSearch
// ---------------------------------------------------------------------------

/// `cogneeSearch(handle, query, opts?) -> Promise<SearchResponse>`
pub fn cognee_search(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let query = cx.argument::<JsString>(1)?.value(&mut cx);
    let opts = match cx.argument_opt(2) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_search(&state, &query, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_search(
    state: &crate::sdk::HandleState,
    query: &str,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let request = build_search_request(query, opts, owner_id)?;

    let response = svc
        .search_orchestrator
        .search(&request)
        .await
        .map_err(|e| SdkError::Runtime(format!("search failed: {e}")))?;

    serde_json::to_string(&response)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize SearchResponse: {e}")))
}

// ---------------------------------------------------------------------------
// cogneeRecall
// ---------------------------------------------------------------------------

/// `cogneeRecall(handle, query, opts?) -> Promise<RecallResult>`
///
/// `RecallResult` is NOT `Serialize` (derives only `Debug, Clone`); JSON is
/// hand-built from its fields.
pub fn cognee_recall(mut cx: FunctionContext) -> JsResult<JsPromise> {
    let handle = cx.argument::<JsBox<CogneeHandle>>(0)?;
    let state = Arc::clone(&handle.state);

    let query = cx.argument::<JsString>(1)?.value(&mut cx);
    let opts = match cx.argument_opt(2) {
        Some(arg) if !arg.is_a::<JsUndefined, _>(&mut cx) && !arg.is_a::<JsNull, _>(&mut cx) => {
            js_to_value(&mut cx, arg)?
        }
        _ => serde_json::Value::Null,
    };

    let channel = cx.channel();
    let (deferred, promise) = cx.promise();

    runtime().spawn(async move {
        let result = run_recall(&state, &query, &opts).await;
        deferred.settle_with(&channel, move |mut cx| match result {
            Ok(json_str) => parse_js(&mut cx, &json_str),
            Err(e) => throw_sdk_error(&mut cx, e),
        });
    });

    Ok(promise)
}

async fn run_recall(
    state: &crate::sdk::HandleState,
    query: &str,
    opts: &serde_json::Value,
) -> Result<String, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    // query_type from opts.searchType
    let query_type = match opts.get("searchType").and_then(|v| v.as_str()) {
        Some(s) => Some(parse_search_type(s)?),
        None => None,
    };

    // datasets from opts.datasets
    let datasets: Option<Vec<String>> = opts.get("datasets").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
    });

    // top_k from opts.topK (default 10)
    let top_k = opts
        .get("topK")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize)
        .unwrap_or(10);

    // auto_route from opts.autoRoute (default false)
    let auto_route = opts
        .get("autoRoute")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // session_id from opts.sessionId
    let session_id_owned = opts
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let session_id: Option<&str> = session_id_owned.as_deref();

    // scope from opts.scope
    let scope_input = build_scope_input(opts)?;
    let scope = normalize_scope(scope_input)
        .map_err(|e| SdkError::Validation(format!("invalid scope: {e}")))?;
    // normalize_scope returns Vec<RecallScope>. An empty vec (from an empty Many input)
    // is passed as None so recall() applies its own Auto default; any non-empty vec
    // (including vec![Auto] from a missing/null/auto scope) is passed as-is — recall()
    // treats Some(vec![Auto]) and None identically (both trigger auto-mode).
    let scope_opt = if scope.is_empty() { None } else { Some(scope) };

    // session_store and session_manager are Option<&dyn …> — borrow from Arc.
    let session_store_ref = Arc::clone(&svc.session_store);
    let session_manager_ref = Arc::clone(&svc.session_manager);

    let result = recall(
        query,
        query_type,
        datasets,
        top_k,
        auto_route,
        session_id,
        Some(&owner_str),
        &svc.search_orchestrator,
        Some(session_store_ref.as_ref()),
        Some(session_manager_ref.as_ref()),
        scope_opt,
    )
    .await
    .map_err(|e| SdkError::Runtime(format!("recall failed: {e}")))?;

    // Hand-build the JSON — RecallResult does not derive Serialize.
    let items = serde_json::to_value(&result.items)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize RecallResult.items: {e}")))?;
    let search_type_used = match result.search_type_used {
        Some(st) => serde_json::to_value(st).map_err(|e| {
            SdkError::Runtime(format!(
                "failed to serialize RecallResult.search_type_used: {e}"
            ))
        })?,
        None => serde_json::Value::Null,
    };
    let search_response = match result.search_response {
        Some(ref sr) => serde_json::to_value(sr).map_err(|e| {
            SdkError::Runtime(format!(
                "failed to serialize RecallResult.search_response: {e}"
            ))
        })?,
        None => serde_json::Value::Null,
    };

    let recall_json = json!({
        "items": items,
        "searchTypeUsed": search_type_used,
        "autoRouted": result.auto_routed,
        "searchResponse": search_response,
    });

    serde_json::to_string(&recall_json)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize RecallResult JSON: {e}")))
}
