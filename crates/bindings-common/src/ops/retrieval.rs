//! Shared async retrieval operations: `search`, `recall`.
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
//! ## Input marshalling
//!
//! `SearchType` is parsed from its SCREAMING_SNAKE_CASE serde wire name via
//! `serde_json::from_value(Value::String(s))` — the same path the HTTP server
//! uses, guaranteed to stay in sync with the `#[serde(rename_all =
//! "SCREAMING_SNAKE_CASE")]` attribute on `SearchType`.
//!
//! `opts` fields are camelCase (mirroring the TS and C API wire shapes).
//!
//! ## Result marshalling
//!
//! `SearchResponse` IS `Serialize` — serialised directly via `serde_json::to_value`.
//!
//! `RecallResult` does NOT derive `Serialize` (derives only `Debug, Clone`) —
//! JSON is hand-built with camelCase keys: `items`, `searchTypeUsed`,
//! `autoRouted`, `searchResponse`.

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use cognee::api::{RecallOptions, ScopeInput, normalize_scope, recall as cognee_recall};
use cognee::search::{SearchRequest, SearchType};

use crate::{HandleState, SdkError};

// ---------------------------------------------------------------------------
// SearchType parsing.
// ---------------------------------------------------------------------------

/// Parse a `SearchType` from a SCREAMING_SNAKE_CASE wire string.
///
/// Uses `serde_json::from_value` so the exact path matches what the HTTP
/// server uses and is guaranteed to stay in sync with the serde attribute.
///
/// Returns `SdkError::Validation` on unknown string, listing all 16 valid values.
pub fn parse_search_type(s: &str) -> Result<SearchType, SdkError> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(|_| {
        SdkError::Validation(format!(
            "unknown SearchType '{s}'. Valid values: SUMMARIES, CHUNKS, RAG_COMPLETION, \
             TRIPLET_COMPLETION, GRAPH_COMPLETION, GRAPH_SUMMARY_COMPLETION, CYPHER, \
             NATURAL_LANGUAGE, GRAPH_COMPLETION_COT, GRAPH_COMPLETION_CONTEXT_EXTENSION, \
             FEELING_LUCKY, FEEDBACK, TEMPORAL, CODING_RULES, CHUNKS_LEXICAL, HYBRID_COMPLETION"
        ))
    })
}

// ---------------------------------------------------------------------------
// SearchRequest builder from opts.
// ---------------------------------------------------------------------------

/// Parse the `datasetIds` opt — a JSON array of UUID strings — into
/// `Option<Vec<Uuid>>`.
///
/// Shared by the `search` and `recall` ops so the two cannot diverge again:
/// recall used to ignore the key outright and run unfiltered.
///
/// **Every entry must be a valid UUID string.** The previous behaviour dropped
/// unparseable entries silently, which failed in the dangerous direction: an
/// all-invalid `datasetIds` collapsed to `Some(vec![])`, and both the
/// orchestrator and `recall` read an empty list as *no filter*, so one typo
/// turned a scoped query into an unscoped one over every dataset the caller
/// can read. The HTTP DTOs reject malformed UUIDs at deserialization; binding
/// callers reach this parser directly, so it has to reject them here.
fn parse_dataset_ids(opts: &serde_json::Value) -> Result<Option<Vec<Uuid>>, SdkError> {
    let Some(value) = opts.get("datasetIds") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(arr) = value.as_array() else {
        return Err(SdkError::Validation(
            "`datasetIds` must be an array of UUID strings".to_string(),
        ));
    };
    let mut ids = Vec::with_capacity(arr.len());
    for entry in arr {
        let raw = entry.as_str().ok_or_else(|| {
            SdkError::Validation(format!(
                "`datasetIds` entries must be UUID strings, got {entry}"
            ))
        })?;
        ids.push(Uuid::parse_str(raw).map_err(|e| {
            SdkError::Validation(format!("invalid UUID in `datasetIds`: {raw}: {e}"))
        })?);
    }
    Ok(Some(ids))
}

/// Everything [`recall`] reads out of its camelCase `opts`, in the order
/// [`cognee::api::recall`] takes them.
///
/// Extracted from the body of [`recall`] so the opts → arguments mapping is
/// assertable without a warmed [`HandleState`]. That mapping is exactly where
/// the `datasetIds` regression lived: the key parsed fine and was then never
/// handed on, so a recall silently ran unscoped. A test over
/// [`build_recall_args`] catches that re-appearing; a test over
/// [`parse_dataset_ids`] alone does not.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallArgs {
    pub query_type: Option<SearchType>,
    pub datasets: Option<Vec<String>>,
    pub dataset_ids: Option<Vec<Uuid>>,
    pub tenant_id: Option<Uuid>,
    pub top_k: usize,
    pub auto_route: bool,
    pub session_id: Option<String>,
    pub scope: Option<Vec<cognee::api::RecallScope>>,
}

/// Parse camelCase recall `opts` into [`RecallArgs`].
pub fn build_recall_args(opts: &serde_json::Value) -> Result<RecallArgs, SdkError> {
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

    // datasetIds from opts.datasetIds, parsed exactly as the search op does.
    // Recall opts are read key-by-key, so before this was wired an unknown
    // `datasetIds` was silently dropped and the recall ran unscoped.
    let dataset_ids = parse_dataset_ids(opts)?;
    let tenant_id = crate::ops::pipeline::opts_tenant(opts)?;

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
    let session_id = opts
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // scope from opts.scope
    let scope_input = build_scope_input(opts)?;
    let scope = normalize_scope(scope_input)
        .map_err(|e| SdkError::Validation(format!("invalid scope: {e}")))?;
    // normalize_scope returns Vec<RecallScope>. An empty vec (from an empty Many
    // input) is passed as None so recall() applies its own Auto default; any
    // non-empty vec (including vec![Auto] from a missing/null/auto scope) is
    // passed as-is — recall() treats Some(vec![Auto]) and None identically.
    let scope = if scope.is_empty() { None } else { Some(scope) };

    Ok(RecallArgs {
        query_type,
        datasets,
        dataset_ids,
        tenant_id,
        top_k,
        auto_route,
        session_id,
        scope,
    })
}

/// Build a `SearchRequest` from camelCase opts.
///
/// `user_id` is always set from `owner_id` — required when `datasets` is
/// supplied (the orchestrator's dataset-resolution path errors with
/// `InvalidInput` when `user_id` is `None` and `datasets` is set).
pub fn build_search_request(
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

    let dataset_ids = parse_dataset_ids(opts)?;
    let tenant_id = crate::ops::pipeline::opts_tenant(opts)?;

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
        // Read from the same `tenant` opt `add` / `remember` write with
        // (`ops::pipeline::opts_tenant`). One handle can therefore hold
        // same-named datasets in several tenants, so resolving a `datasets`
        // name without this predicate could pick the wrong tenant's row.
        // Absent, it stays `None` — "no tenant filter" — which is right for
        // the single-tenant default where every row carries `tenant_id = NULL`.
        tenant_id,
        verbose,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
        summarize_context: None,
    })
}

// ---------------------------------------------------------------------------
// ScopeInput builder from opts.
// ---------------------------------------------------------------------------

/// Build a `ScopeInput` from the `opts.scope` field (a string or string array).
///
/// Returns `None` when the field is absent (caller gets `[Auto]` from
/// `normalize_scope(None)`).
pub fn build_scope_input(opts: &serde_json::Value) -> Result<Option<ScopeInput>, SdkError> {
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
// Public top-level retrieval operations.
// ---------------------------------------------------------------------------

/// Run search and return the `SearchResponse` as a JSON value.
///
/// `opts` may be `serde_json::Value::Null` when no options were provided.
/// All `opts` keys must be camelCase (e.g. `searchType`, `topK`, `datasets`).
/// A `userId` key in opts is ignored; `user_id` is always set from the handle's
/// `owner_id` so dataset-name resolution works correctly.
pub async fn search(
    state: &HandleState,
    query: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let request = build_search_request(query, opts, owner_id)?;

    let response = svc
        .search_orchestrator
        .search(&request)
        .await
        .map_err(|e| SdkError::Runtime(format!("search failed: {e}")))?;

    serde_json::to_value(&response)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize SearchResponse: {e}")))
}

/// Run recall and return hand-built camelCase JSON value.
///
/// `RecallResult` does not derive `Serialize`; JSON is hand-built with
/// camelCase keys to match the TS wire shape: `items`, `searchTypeUsed`,
/// `autoRouted`, `searchResponse`.
///
/// `opts` may be `serde_json::Value::Null` when no options were provided.
pub async fn recall(
    state: &HandleState,
    query: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let RecallArgs {
        query_type,
        datasets,
        dataset_ids,
        tenant_id,
        top_k,
        auto_route,
        session_id: session_id_owned,
        scope: scope_opt,
    } = build_recall_args(opts)?;
    let session_id: Option<&str> = session_id_owned.as_deref();

    // session_store and session_manager are Option<&dyn …> — borrow from Arc.
    let session_store_ref = Arc::clone(&svc.session_store);
    let session_manager_ref = Arc::clone(&svc.session_manager);

    let result = cognee_recall(
        query,
        query_type,
        datasets,
        dataset_ids,
        top_k,
        auto_route,
        session_id,
        Some(&owner_str),
        &svc.search_orchestrator,
        Some(session_store_ref.as_ref()),
        Some(session_manager_ref.as_ref()),
        scope_opt,
        // Only the tenant is set; the rest of `RecallOptions` is advanced
        // tuning these opts do not expose. Same reasoning as the `tenant_id`
        // on `build_search_request`: `add` / `remember` accept a per-call
        // `tenant`, so a name must be resolved under the caller's tenant.
        Some(RecallOptions {
            tenant_id,
            ..Default::default()
        }),
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

    Ok(json!({
        "items": items,
        "searchTypeUsed": search_type_used,
        "autoRouted": result.auto_routed,
        "searchResponse": search_response,
    }))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    const ID_A: &str = "11111111-1111-4111-8111-111111111111";
    const ID_B: &str = "22222222-2222-4222-8222-222222222222";

    /// The `recall` op reads its opts key by key, so an unrecognised
    /// `datasetIds` used to vanish silently and the recall ran unscoped —
    /// exactly the failure PR 204 existed to fix, on the sibling op. Both ops
    /// now parse through this one helper.
    #[test]
    fn parse_dataset_ids_reads_a_uuid_array() {
        let opts = json!({ "datasetIds": [ID_A, ID_B] });
        assert_eq!(
            parse_dataset_ids(&opts).unwrap(),
            Some(vec![
                Uuid::parse_str(ID_A).unwrap(),
                Uuid::parse_str(ID_B).unwrap()
            ])
        );
    }

    #[test]
    fn parse_dataset_ids_is_none_when_absent_or_null() {
        assert_eq!(parse_dataset_ids(&json!({})).unwrap(), None);
        assert_eq!(
            parse_dataset_ids(&json!({ "datasetIds": null })).unwrap(),
            None
        );
    }

    /// A malformed entry is rejected, never dropped. Dropping failed in the
    /// dangerous direction: `["bad"]` collapsed to `Some(vec![])`, which every
    /// consumer reads as *no filter*, so one typo silently widened a scoped
    /// query to every dataset the caller can read.
    #[test]
    fn parse_dataset_ids_rejects_malformed_entries_instead_of_widening_scope() {
        assert!(parse_dataset_ids(&json!({ "datasetIds": ["not-a-uuid"] })).is_err());
        assert!(parse_dataset_ids(&json!({ "datasetIds": [ID_A, "not-a-uuid"] })).is_err());
        assert!(parse_dataset_ids(&json!({ "datasetIds": [ID_A, 7] })).is_err());
        // A non-array is a caller mistake too, not "no filter".
        assert!(parse_dataset_ids(&json!({ "datasetIds": ID_A })).is_err());
    }

    /// Both ops must reject it, not just the parser in isolation.
    #[test]
    fn a_malformed_dataset_id_fails_both_ops() {
        let opts = json!({ "datasetIds": ["not-a-uuid"] });
        assert!(build_recall_args(&opts).is_err());
        assert!(build_search_request("q", &opts, Uuid::new_v4()).is_err());
    }

    /// An empty array stays `Some(vec![])`, which every downstream consumer
    /// treats as "no filter" — not as "match nothing". Explicitly asking for
    /// no filter is fine; arriving there by way of a typo is not.
    #[test]
    fn parse_dataset_ids_keeps_an_empty_array_distinct_from_absent() {
        assert_eq!(
            parse_dataset_ids(&json!({ "datasetIds": [] })).unwrap(),
            Some(vec![])
        );
    }

    /// The search op's own wiring, asserted here so a future refactor cannot
    /// drop the field on this side either.
    #[test]
    fn build_search_request_carries_dataset_ids() {
        let owner = Uuid::new_v4();
        let opts = json!({ "datasetIds": [ID_A] });
        let req = build_search_request("q", &opts, owner).unwrap();
        assert_eq!(req.dataset_ids, Some(vec![Uuid::parse_str(ID_A).unwrap()]));
        assert_eq!(req.user_id, Some(owner));
    }

    /// The regression this ticket exists for: `recall` parsed nothing for
    /// `datasetIds` and handed `None` to `cognee::api::recall`, so the filter
    /// was silently dropped. Asserting the whole argument bundle — not just
    /// the parse — is what makes a repeat of that failure visible.
    #[test]
    fn build_recall_args_carries_dataset_ids() {
        let opts = json!({ "datasetIds": [ID_A, ID_B] });
        let args = build_recall_args(&opts).unwrap();
        assert_eq!(
            args.dataset_ids,
            Some(vec![
                Uuid::parse_str(ID_A).unwrap(),
                Uuid::parse_str(ID_B).unwrap()
            ]),
            "recall must forward datasetIds, not drop them"
        );
    }

    /// `add` / `remember` accept a per-call `tenant`, so one handle can hold
    /// same-named datasets in several tenants. Both retrieval ops must read
    /// the same key or a `datasets` name resolves against the wrong tenant.
    #[test]
    fn both_ops_carry_the_per_call_tenant() {
        let tenant = Uuid::new_v4();
        let opts = json!({ "tenant": tenant.to_string(), "datasets": ["ds"] });

        let args = build_recall_args(&opts).unwrap();
        assert_eq!(args.tenant_id, Some(tenant), "recall must scope by tenant");

        let req = build_search_request("q", &opts, Uuid::new_v4()).unwrap();
        assert_eq!(req.tenant_id, Some(tenant), "search must scope by tenant");
    }

    /// An absent `tenant` stays `None` — "no tenant filter" — which is what
    /// the single-tenant default needs, since those rows carry a NULL tenant.
    #[test]
    fn tenant_is_none_when_absent() {
        let opts = json!({});
        assert_eq!(build_recall_args(&opts).unwrap().tenant_id, None);
        assert_eq!(
            build_search_request("q", &opts, Uuid::new_v4())
                .unwrap()
                .tenant_id,
            None
        );
    }

    /// A malformed tenant is rejected rather than silently ignored, matching
    /// `ops::pipeline::opts_tenant`'s behaviour on the write side — otherwise
    /// a typo would widen the scope instead of failing.
    ///
    /// The non-string cases matter as much as the bad-UUID one: `opts_tenant`
    /// used `and_then(as_str)`, so `{"tenant": 123}` read as *absent* and the
    /// query silently lost its tenant predicate.
    #[test]
    fn a_malformed_tenant_is_rejected() {
        for bad in [json!("not-a-uuid"), json!(123), json!({}), json!([])] {
            let opts = json!({ "tenant": bad });
            assert!(
                build_recall_args(&opts).is_err(),
                "recall must reject tenant {bad}"
            );
            assert!(
                build_search_request("q", &opts, Uuid::new_v4()).is_err(),
                "search must reject tenant {bad}"
            );
        }
    }

    /// The rest of the recall bundle, so an extraction refactor cannot quietly
    /// change a default (`topK` 10, `autoRoute` false).
    #[test]
    fn build_recall_args_defaults_match_the_documented_wire_shape() {
        let args = build_recall_args(&json!({})).unwrap();
        assert_eq!(args.top_k, 10);
        assert!(!args.auto_route);
        assert_eq!(args.datasets, None);
        assert_eq!(args.dataset_ids, None);
        assert_eq!(args.session_id, None);
        assert_eq!(args.query_type, None);
        // A missing `scope` normalizes to `[Auto]`, which recall() treats the
        // same as `None`; it must not arrive as an empty vec.
        assert_eq!(args.scope, Some(vec![cognee::api::RecallScope::Auto]));
    }
}
