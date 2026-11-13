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

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use cognee::api::{ApiError, RecallOptions, ScopeInput, normalize_scope, recall as cognee_recall};
use cognee::config::CallSection;
use cognee::search::recall_scope::{
    RecallScope, has_dataset_filter, resolve_session_cache_user_id, resolve_sources,
};
use cognee::search::{
    EngineOverride, NormalizedResult, RecallItem, RecallSource, SearchOutput, SearchRequest,
    SearchResponse, SearchType, ToolsTrigger, build_graph_entry, build_structured_entry,
    entry_dataset_ids, entry_evidence, maybe_with_engine_override, only_context_system_prompt,
};
use cognee::{ComponentManager, EmbeddingConfig, LlmConfig};

use crate::{HandleState, SdkError};

// ---------------------------------------------------------------------------
// SearchType parsing.
// ---------------------------------------------------------------------------

/// Parse a `SearchType` from a SCREAMING_SNAKE_CASE wire string.
///
/// Uses `serde_json::from_value` so the exact path matches what the HTTP
/// server uses and is guaranteed to stay in sync with the serde attribute.
///
/// Returns `SdkError::Validation` on unknown string, listing all 17 valid values.
pub fn parse_search_type(s: &str) -> Result<SearchType, SdkError> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).map_err(|_| {
        SdkError::Validation(format!(
            "unknown SearchType '{s}'. Valid values: SUMMARIES, CHUNKS, RAG_COMPLETION, \
             GRAPH_COMPLETION, GRAPH_COMPLETION_DECOMPOSITION, \
             GRAPH_SUMMARY_COMPLETION, CYPHER, NATURAL_LANGUAGE, GRAPH_COMPLETION_COT, \
             GRAPH_COMPLETION_CONTEXT_EXTENSION, FEELING_LUCKY, FEEDBACK, TEMPORAL, CODING_RULES, \
             CHUNKS_LEXICAL, GRAPH_REPORT, HYBRID_COMPLETION"
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

/// Parse the `datasets` opt — a JSON array of dataset-name strings — into
/// `Option<Vec<String>>`.
///
/// Strict for the same reason [`parse_dataset_ids`] is, and it matters more
/// here: `{"datasets": [123]}` used to `filter_map` down to `Some(vec![])`,
/// and the orchestrator's name branch is guarded on `!names.is_empty()`, so an
/// empty list falls through to *no dataset filter at all* — a scoped query
/// silently becoming an unscoped one over everything the caller can read. A
/// non-array `{"datasets": "ds"}` failed `as_array()` and reached the same
/// place via `None`.
///
/// An explicitly empty array stays `Some(vec![])`: Python's `if datasets:`
/// short-circuits empty lists too, so "no filter" is the correct reading of a
/// list the caller deliberately sent empty. Only a *malformed* entry errors.
///
/// `coerce_bare_string` wraps a lone `{"datasets": "ds"}` into `["ds"]` instead
/// of erroring. It is set on the *search* path only, because only Python's
/// `search` normalizes: `cognee/api/v1/search/search.py:294-295` does
/// `if isinstance(datasets, UUID) or isinstance(datasets, str): datasets = [datasets]`.
/// Python's `recall` declares `datasets: list[str] | None`
/// (`cognee/api/v1/recall/recall.py:344`) and normalizes nothing, so a bare
/// string there iterates its characters and raises `DatasetNotFoundError` —
/// a rejection, just a worse-worded one. Rust keeps rejecting it outright.
fn parse_dataset_names(
    opts: &serde_json::Value,
    coerce_bare_string: bool,
) -> Result<Option<Vec<String>>, SdkError> {
    let Some(value) = opts.get("datasets") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if coerce_bare_string && let Some(name) = value.as_str() {
        return Ok(Some(vec![name.to_string()]));
    }
    let Some(arr) = value.as_array() else {
        return Err(SdkError::Validation(
            "`datasets` must be an array of dataset-name strings".to_string(),
        ));
    };
    arr.iter()
        .map(|entry| {
            entry.as_str().map(|s| s.to_string()).ok_or_else(|| {
                SdkError::Validation(format!("`datasets` entries must be strings, got {entry}"))
            })
        })
        .collect::<Result<Vec<String>, SdkError>>()
        .map(Some)
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
    pub neighborhood_depth: Option<usize>,
    pub neighborhood_seed_top_k: Option<usize>,
    pub include_references: bool,
    /// `toolsTrigger`: Python's `tools_trigger` (`recall.py:464`), validated
    /// on every call whatever the scope (`:596-600`). No effect until the
    /// `tools` lane exists (SDK-1537).
    pub tools_trigger: ToolsTrigger,
    /// `contextProfile`: the profile the `session_context` lane renders
    /// (Python's `context_profile`, `recall.py:453`). Not validated, as in
    /// Python; `None` is the `"qa"` default.
    pub context_profile: Option<String>,
    /// `nodeType` / `nodeName` / `nodeNameFilterOperator`: the graph lane's
    /// node scope, parsed exactly as for search.
    pub node_scope: NodeScope,
    /// `verbose`: the graph search keeps its context and graph (Python's
    /// `verbose`, `recall.py:457`).
    pub verbose: Option<bool>,
    /// `llmConfig` / `embeddingConfig`: the engines this recall runs with
    /// (Python's `llm_config` / `embedding_config`, `recall.py:467-468`).
    pub engine_configs: EngineConfigs,
}

impl RecallArgs {
    /// The [`RecallOptions`] these arguments hand to [`cognee::api::recall`].
    ///
    /// The rest of `RecallOptions` is advanced tuning the opts do not expose.
    /// The tenant has the same reasoning as the `tenant_id` on
    /// [`build_search_request`]: `add` / `remember` accept a per-call
    /// `tenant`, so a name must be resolved under the caller's tenant.
    pub fn recall_options(&self) -> RecallOptions {
        RecallOptions {
            tenant_id: self.tenant_id,
            neighborhood_depth: self.neighborhood_depth,
            neighborhood_seed_top_k: self.neighborhood_seed_top_k,
            include_references: self.include_references,
            tools_trigger: self.tools_trigger,
            context_profile: self.context_profile.clone(),
            node_type: self.node_scope.node_type.clone(),
            node_name: self.node_scope.node_name.clone(),
            node_name_filter_operator: self.node_scope.node_name_filter_operator.clone(),
            verbose: self.verbose,
            ..Default::default()
        }
    }
}

/// The node scope shared by search and recall opts: `nodeType`, `nodeName`
/// and `nodeNameFilterOperator`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeScope {
    pub node_type: Option<String>,
    pub node_name: Option<Vec<String>>,
    pub node_name_filter_operator: Option<String>,
}

/// Parse `nodeType` (a string), `nodeName` (a string array; non-string
/// entries are skipped) and `nodeNameFilterOperator` (see
/// [`parse_node_name_filter_operator`]).
fn parse_node_scope(opts: &serde_json::Value) -> Result<NodeScope, SdkError> {
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
    Ok(NodeScope {
        node_type,
        node_name,
        node_name_filter_operator: parse_node_name_filter_operator(opts)?,
    })
}

/// `verbose`: anything but a JSON boolean is unset.
fn parse_verbose(opts: &serde_json::Value) -> Option<bool> {
    opts.get("verbose").and_then(|v| v.as_bool())
}

/// `toolsTrigger`: `"always"` (the default when absent or null) or
/// `"on_empty"`, through [`ToolsTrigger::from_str`] so the message is
/// Python's. A non-string value is refused like an unknown string, as
/// Python's `tools_trigger not in ("always", "on_empty")` refuses it.
fn parse_tools_trigger(opts: &serde_json::Value) -> Result<ToolsTrigger, SdkError> {
    let raw = match opts.get("toolsTrigger") {
        None | Some(serde_json::Value::Null) => return Ok(ToolsTrigger::default()),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    raw.parse::<ToolsTrigger>()
        .map_err(|e| SdkError::Validation(format!("invalid tools_trigger: {e}")))
}

/// Parse camelCase recall `opts` into [`RecallArgs`].
pub fn build_recall_args(opts: &serde_json::Value) -> Result<RecallArgs, SdkError> {
    // query_type from opts.searchType
    let query_type = match opts.get("searchType").and_then(|v| v.as_str()) {
        Some(s) => Some(parse_search_type(s)?),
        None => None,
    };

    // datasets from opts.datasets. Strict: Python's `recall` takes
    // `datasets: list[str] | None` and normalizes nothing
    // (`cognee/api/v1/recall/recall.py:344`), so a bare string is a caller
    // error there too — it just fails later and less clearly.
    let datasets = parse_dataset_names(opts, false)?;

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
    // A missing/null scope normalizes to `[Auto]`; an explicit empty list
    // stays empty, so the recall runs nothing and returns `[]`, as Python's
    // `normalize_scope([])` -> `[]` does.
    let scope = Some(scope);
    let (neighborhood_depth, neighborhood_seed_top_k) = parse_neighborhood(opts)?;
    let include_references = parse_include_references(opts);
    let tools_trigger = parse_tools_trigger(opts)?;
    let context_profile = opts
        .get("contextProfile")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let node_scope = parse_node_scope(opts)?;
    let verbose = parse_verbose(opts);
    let engine_configs = parse_engine_configs(opts)?;

    Ok(RecallArgs {
        query_type,
        datasets,
        dataset_ids,
        tenant_id,
        top_k,
        auto_route,
        session_id,
        scope,
        neighborhood_depth,
        neighborhood_seed_top_k,
        include_references,
        tools_trigger,
        context_profile,
        node_scope,
        verbose,
        engine_configs,
    })
}

/// The per-call `llmConfig` / `embeddingConfig` of a search or recall (Python's
/// `llm_config` / `embedding_config`, `search.py:71-72`, `recall.py:467-468`).
///
/// Each is a JSON object keyed by the setting's canonical key (`"llm.model"`)
/// or its Python name (`"llm_model"`), parsed into a [`CallSection`] that keeps
/// only the settings it gives. It replaces the whole configured section for
/// the call, as Python's does: a setting it leaves out takes the value the
/// environment gave it, else the default (see
/// `ComponentManager::build_engines_with_overrides`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineConfigs {
    pub llm: Option<CallSection<LlmConfig>>,
    pub embedding: Option<CallSection<EmbeddingConfig>>,
}

impl EngineConfigs {
    /// The engines these configs select, built (and cached) through `cm`;
    /// `None` without configs.
    ///
    /// An embedding model of another width than the configured one is a
    /// [`SdkError::Validation`] (Python's `EmbeddingDimensionMismatchError`);
    /// an engine that cannot be built is a [`SdkError::Component`].
    pub async fn resolve(&self, cm: &ComponentManager) -> Result<Option<EngineOverride>, SdkError> {
        if self.llm.is_none() && self.embedding.is_none() {
            return Ok(None);
        }
        cm.build_engines_with_overrides(self.llm.as_ref(), self.embedding.as_ref())
            .await
            .map(Some)
            .map_err(|error| match error {
                ApiError::EmbeddingDimensionMismatch(e) => SdkError::Validation(e.to_string()),
                ApiError::Component(e) => SdkError::Component(e),
                other => SdkError::Runtime(other.to_string()),
            })
    }
}

/// Parse `llmConfig` / `embeddingConfig` into [`EngineConfigs`]. Absent or
/// null is no override; anything but a JSON object, or a known setting with a
/// value of the wrong type, is a validation error. Unknown keys are ignored
/// with a warning, as Python's `extra="allow"` config models accept them.
pub fn parse_engine_configs(opts: &serde_json::Value) -> Result<EngineConfigs, SdkError> {
    Ok(EngineConfigs {
        llm: parse_section(opts, "llmConfig")?,
        embedding: parse_section(opts, "embeddingConfig")?,
    })
}

fn parse_section<T: cognee::config::ConfigSection>(
    opts: &serde_json::Value,
    key: &str,
) -> Result<Option<CallSection<T>>, SdkError> {
    match opts.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Object(object)) => CallSection::from_object(object)
            .map(Some)
            .map_err(|e| SdkError::Validation(format!("invalid `{key}`: {e}"))),
        // Names only the JSON type: the value may be a misplaced API key.
        Some(other) => Err(SdkError::Validation(format!(
            "`{key}` must be an object, got {}",
            crate::process_config::type_name(other)
        ))),
    }
}

/// `includeReferences`: attach structured evidence references (and, for RAG,
/// the Evidence text block) to a completion search or recall. Python's
/// `include_references`, default `False`; anything but a JSON `true` is off.
fn parse_include_references(opts: &serde_json::Value) -> bool {
    opts.get("includeReferences")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// `neighborhoodDepth` / `neighborhoodSeedTopK`: the k-hop graph projection
/// knobs of Python's `search()` / `recall()`, which both have to be positive
/// integers there (`cognee/api/v1/search/search.py`).
fn parse_neighborhood(
    opts: &serde_json::Value,
) -> Result<(Option<usize>, Option<usize>), SdkError> {
    let positive = |key: &str, name: &str| -> Result<Option<usize>, SdkError> {
        let Some(value) = opts.get(key).filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        match value.as_u64() {
            Some(n) if n >= 1 => usize::try_from(n)
                .map(Some)
                .map_err(|_| SdkError::Validation(format!("{name} is too large: {n}"))),
            _ => Err(SdkError::Validation(format!(
                "{name} must be a positive integer."
            ))),
        }
    };
    Ok((
        positive("neighborhoodDepth", "neighborhood_depth")?,
        positive("neighborhoodSeedTopK", "neighborhood_seed_top_k")?,
    ))
}

/// `nodeNameFilterOperator`: how `nodeName` combines — `"OR"` (any of the
/// names) or `"AND"` (all of them). Python normalizes it with
/// `.strip().upper()` and rejects anything else (`search.py:311-317`).
fn parse_node_name_filter_operator(opts: &serde_json::Value) -> Result<Option<String>, SdkError> {
    let Some(value) = opts.get("nodeNameFilterOperator").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let normalized = value
        .as_str()
        .map(|s| s.trim().to_uppercase())
        .unwrap_or_default();
    match normalized.as_str() {
        "AND" | "OR" => Ok(Some(normalized)),
        _ => Err(SdkError::Validation(format!(
            "Invalid node_name_filter_operator: {value}. Must be one of ['AND', 'OR']."
        ))),
    }
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
    // searchType (default HYBRID_COMPLETION)
    let search_type = match opts.get("searchType").and_then(|v| v.as_str()) {
        Some(s) => parse_search_type(s)?,
        None => SearchType::default(),
    };

    // datasets: string array, or a bare string that Python's `search`
    // normalizes into a one-element list
    // (`cognee/api/v1/search/search.py:294-295`). The Python shim forwards the
    // value verbatim (`python/cognee_py/compat.py:325-326`), so
    // `cognee.search("q", datasets="docs")` has to scope here the way it does
    // upstream.
    let datasets = parse_dataset_names(opts, true)?;

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
    let NodeScope {
        node_type,
        node_name,
        node_name_filter_operator,
    } = parse_node_scope(opts)?;
    let only_context = opts.get("onlyContext").and_then(|v| v.as_bool());
    let use_combined_context = opts.get("useCombinedContext").and_then(|v| v.as_bool());
    let verbose = parse_verbose(opts);
    let auto_feedback_detection = opts.get("autoFeedbackDetection").and_then(|v| v.as_bool());
    let (neighborhood_depth, neighborhood_seed_top_k) = parse_neighborhood(opts)?;
    // Per-retriever knobs, forwarded verbatim. `SearchRequest` documents these
    // as caller-defined and silently ignores unknown keys, so the binding has
    // no table to keep in step — it only has to stop dropping the object. Until
    // it was wired, a knob like the hybrid retriever's `text_summaries_top_k`
    // was reachable from the HTTP API and from Rust but not from any SDK
    // binding, and a caller setting it saw no error and no effect.
    let retriever_specific_config = opts
        .get("retrieverSpecificConfig")
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        });

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
        node_name_filter_operator,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        // Always populate user_id from owner_id so dataset-name resolution works.
        user_id: Some(owner_id),
        // Read from the same `tenant` opt `add` / `remember` write with
        // (`ops::pipeline::opts_tenant`). One handle can therefore hold
        // same-named datasets in several tenants, so resolving a `datasets`
        // name without this predicate could pick the wrong tenant's row.
        // Absent, it stays `None`: a tenant-less caller, which reaches only
        // tenant-less datasets (strict equality, Python
        // `get_all_user_permission_datasets.py:49-53@37f788bd2`) — every row
        // in the single-tenant default carries `tenant_id = NULL`.
        tenant_id,
        verbose,
        feedback_influence: None,
        retriever_specific_config,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection,
        neighborhood_depth,
        neighborhood_seed_top_k,
        include_references: parse_include_references(opts),
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
            // A non-string element is refused rather than dropped: dropping
            // it could turn `[1]` into an empty list, which runs nothing.
            let strings = arr
                .iter()
                .map(|v| {
                    v.as_str().map(str::to_string).ok_or_else(|| {
                        SdkError::Validation(format!(
                            "`scope` must be a string or string array, got element: {v}"
                        ))
                    })
                })
                .collect::<Result<Vec<String>, SdkError>>()?;
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

/// The user whose session cache recall's session, trace and session_context
/// lanes read: the session's owner when the caller may read it through a
/// dataset grant, else the caller (Python `_resolve_session_cache_user_id`,
/// `recall.py:181-250`, which local `recall()` calls at `:272`, `:330`,
/// `:401`). The grants come from the handle's [`ReadableDatasets`] hook; with
/// none (OSS), or when no session lane runs, it is `None` and recall reads
/// the caller's cache. A failing grant lookup also falls back to the caller,
/// as Python does.
///
/// [`ReadableDatasets`]: crate::ReadableDatasets
#[allow(clippy::too_many_arguments)]
async fn session_cache_user_id(
    state: &HandleState,
    svc: &crate::CogneeServices,
    caller: Uuid,
    tenant_id: Option<Uuid>,
    session_id: Option<&str>,
    scope: Option<&[RecallScope]>,
    datasets: Option<&[String]>,
    dataset_ids: Option<&[Uuid]>,
    query_type: Option<SearchType>,
) -> Option<String> {
    let hook = state.readable_datasets()?;
    session_id.filter(|id| !id.is_empty())?;
    // Only when a session-cache lane runs, as the HTTP router does: a
    // graph-only recall needs no grant lookup. A refused scope fails in
    // recall itself.
    let scope = scope.map_or_else(|| vec![RecallScope::Auto], <[RecallScope]>::to_vec);
    let sources = resolve_sources(
        scope,
        session_id,
        has_dataset_filter(datasets, dataset_ids),
        query_type,
    )
    .ok()?
    .sources;
    if !sources.iter().any(|source| {
        matches!(
            source,
            RecallScope::Session | RecallScope::Trace | RecallScope::SessionContext
        )
    }) {
        return None;
    }
    let permitted = match hook
        .readable_dataset_ids(&svc.database, caller, tenant_id)
        .await
    {
        Ok(ids) => ids,
        Err(error) => {
            tracing::warn!(%error, "recall: dataset grants not resolved; reading the caller's session cache");
            Vec::new()
        }
    };
    let caller = caller.to_string();
    resolve_session_cache_user_id(&*svc.database, session_id, Some(&caller), &permitted).await
}

/// Run search and return the `SearchResponse` as a JSON value.
///
/// `opts` may be `serde_json::Value::Null` when no options were provided.
/// All `opts` keys must be camelCase (e.g. `searchType`, `topK`, `datasets`).
/// A `userId` key in opts is ignored; `user_id` is always set from the handle's
/// `owner_id` so dataset-name resolution works correctly.
///
/// `llmConfig` / `embeddingConfig` run the search with the call's own LLM and
/// embedding engine (see [`EngineConfigs`]).
pub async fn search(
    state: &HandleState,
    query: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;

    let request = build_search_request(query, opts, owner_id)?;
    let engines = parse_engine_configs(opts)?.resolve(&state.cm).await?;

    let response = maybe_with_engine_override(engines, svc.search_orchestrator.search(&request))
        .await
        .map_err(|e| SdkError::Runtime(format!("search failed: {e}")))?;

    serde_json::to_value(&response)
        .map_err(|e| SdkError::Runtime(format!("failed to serialize SearchResponse: {e}")))
}

/// Run recall and return hand-built camelCase JSON value.
///
/// `RecallResult` does not derive `Serialize`; JSON is hand-built with
/// camelCase keys to match the TS wire shape: `items`, `searchTypeUsed`,
/// `autoRouted`, `searchResponse`, `normalizedResults`.
///
/// `normalizedResults` is additive: one typed entry (kind, text, score,
/// provenance metadata, raw; see [`normalize_recall_items`]) per graph-source
/// item, in the same order. `items` is unchanged. Session, trace and
/// session_context items stay out of it, as Python's
/// `normalize_search_payload` normalizes graph results only; they are in
/// `items`, tagged with their `source`.
///
/// `opts` may be `serde_json::Value::Null` when no options were provided.
/// `llmConfig` / `embeddingConfig` run the recall with the call's own LLM and
/// embedding engine (see [`EngineConfigs`]).
pub async fn recall(
    state: &HandleState,
    query: &str,
    opts: &serde_json::Value,
) -> Result<serde_json::Value, SdkError> {
    let svc = state.services().await?;
    let owner_id = state.owner_id().await?;
    let owner_str = owner_id.to_string();

    let args = build_recall_args(opts)?;
    let mut options = RecallOptions {
        engine_override: args.engine_configs.resolve(&state.cm).await?,
        ..args.recall_options()
    };
    let RecallArgs {
        query_type,
        datasets,
        dataset_ids,
        tenant_id,
        top_k,
        auto_route,
        session_id: session_id_owned,
        scope: scope_opt,
        ..
    } = args;
    let session_id: Option<&str> = session_id_owned.as_deref();

    // session_store and session_manager are Option<&dyn …> — borrow from Arc.
    let session_store_ref = Arc::clone(&svc.session_store);
    let session_manager_ref = Arc::clone(&svc.session_manager);

    options.session_cache_user_id = session_cache_user_id(
        state,
        &svc,
        owner_id,
        tenant_id,
        session_id,
        scope_opt.as_deref(),
        datasets.as_deref(),
        dataset_ids.as_deref(),
        query_type,
    )
    .await;

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
        Some(options),
    )
    .await
    .map_err(recall_error)?;

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

    let dataset_names = recall_dataset_names(
        svc.database.as_ref(),
        &result.items,
        result.search_response.as_ref(),
    )
    .await;
    let normalized_results = match result.search_type_used {
        Some(st) => serde_json::to_value(normalize_recall_items(
            &result.items,
            st,
            result.search_response.as_ref(),
            &dataset_names,
        ))
        .map_err(|e| {
            SdkError::Runtime(format!(
                "failed to serialize normalized recall results: {e}"
            ))
        })?,
        None => json!([]),
    };

    Ok(json!({
        "items": items,
        "searchTypeUsed": search_type_used,
        "autoRouted": result.auto_routed,
        "searchResponse": search_response,
        "normalizedResults": normalized_results,
    }))
}

/// Map a [`cognee::api::recall`] failure onto the binding error.
///
/// Recall refuses the caller's own input with [`ApiError::InvalidArgument`]:
/// a `tools` / `code` scope that is not supported yet (SDK-1537) or node
/// names with an empty `nodeType`, both before any source runs, and a pinned
/// `searchType` that cannot apply the `nodeName` filter. That is a
/// developer error, so it surfaces as [`SdkError::Validation`] (the bindings'
/// `VALIDATION_ERROR`) rather than as an infrastructure failure.
fn recall_error(error: ApiError) -> SdkError {
    match error {
        ApiError::InvalidArgument(message) => SdkError::Validation(message),
        other => SdkError::Runtime(format!("recall failed: {other}")),
    }
}

/// The graph-source items of a recall, in order.
fn graph_items(items: &[RecallItem]) -> Vec<&RecallItem> {
    items
        .iter()
        .filter(|item| item.source == RecallSource::Graph)
        .collect()
}

/// The dataset of each graph-source item ([`entry_dataset_ids`]); all `None`
/// without a search response.
fn graph_item_datasets(
    graph_items: &[&RecallItem],
    search_response: Option<&SearchResponse>,
) -> Vec<Option<Uuid>> {
    match search_response {
        Some(response) => {
            let contents: Vec<&serde_json::Value> =
                graph_items.iter().map(|item| &item.content).collect();
            entry_dataset_ids(&contents, response)
        }
        None => vec![None; graph_items.len()],
    }
}

/// The names of the datasets the graph-source items came from, for
/// `normalizedResults`' `dataset_name` (Python's per-dataset payload carries
/// it). A lookup failure only leaves the names out: the recall already
/// succeeded, and the ids are still reported.
async fn recall_dataset_names(
    database: &dyn cognee::database::RelationalDb,
    items: &[RecallItem],
    search_response: Option<&SearchResponse>,
) -> HashMap<Uuid, String> {
    let mut ids: Vec<Uuid> = graph_item_datasets(&graph_items(items), search_response)
        .into_iter()
        .flatten()
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return HashMap::new();
    }
    match cognee::database::IngestDb::get_datasets_by_ids(database, &ids).await {
        Ok(datasets) => datasets.into_iter().map(|ds| (ds.id, ds.name)).collect(),
        Err(error) => {
            tracing::warn!(%error, "recall: dataset names for normalizedResults not resolved");
            HashMap::new()
        }
    }
}

/// Normalize the graph-source recall items into typed entries.
///
/// Session, trace and system items are not retriever output and carry no
/// search type, so they are skipped (Python's `normalize_search_payload`
/// likewise only runs over search payloads). Each entry's `dataset_id` is
/// the dataset it came from ([`entry_dataset_ids`]: its own membership, its
/// position in a per-dataset fan-out, or the response's only dataset) and
/// its `dataset_name` that dataset's name in `dataset_names`; both stay
/// `None` where the dataset cannot be recovered (a shared-store search over
/// several datasets whose answers carry no membership). For an `only_context` recall the graph
/// response's system prompt is stamped on each entry, as Python's
/// `normalize_search_payload` (`normalize_search_payload.py:149-157`) and
/// the HTTP `/api/v1/recall` route do.
///
/// `metadata.evidence` is split per dataset, as Python attaches each
/// dataset's payload evidence to that payload's entries only: an entry of a
/// response merged over several datasets keeps only the references stamped
/// with its own dataset ([`entry_evidence`]), the dataset being recovered from
/// the entry's own membership or its position ([`entry_dataset_ids`]). Where
/// it cannot be recovered the entry keeps the whole list.
pub fn normalize_recall_items(
    items: &[RecallItem],
    search_type: SearchType,
    search_response: Option<&SearchResponse>,
    dataset_names: &HashMap<Uuid, String>,
) -> Vec<NormalizedResult> {
    let system_prompt = search_response.and_then(only_context_system_prompt);
    let structured =
        search_response.is_some_and(|r| matches!(r.result, SearchOutput::Structured(_)));
    let graph_items = graph_items(items);
    let entry_datasets = graph_item_datasets(&graph_items, search_response);
    let (evidence, dataset_count) = match search_response {
        Some(response) => (
            response.evidence.as_slice(),
            response.datasets.as_ref().map_or(0, Vec::len),
        ),
        None => (&[][..], 0),
    };
    graph_items
        .into_iter()
        .zip(entry_datasets)
        .map(|(item, dataset)| {
            let name = dataset.and_then(|id| dataset_names.get(&id).cloned());
            let dataset = dataset.map(|id| id.to_string());
            let build = if structured {
                build_structured_entry
            } else {
                build_graph_entry
            };
            let mut entry = build(item.content.clone(), search_type, dataset.clone(), name);
            entry.system_prompt = system_prompt.clone();
            entry.attach_evidence(&entry_evidence(evidence, dataset_count, dataset.as_deref()));
            entry
        })
        .collect()
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

    /// `recall` adds `normalizedResults` next to the untouched raw `items`:
    /// graph items are normalized with the used search type's kind, other
    /// sources are skipped.
    #[test]
    fn normalize_recall_items_types_graph_items_only() {
        let items = vec![
            RecallItem {
                source: RecallSource::Session,
                content: json!({"question": "q"}),
                score: 1.0,
            },
            RecallItem {
                source: RecallSource::Graph,
                content: json!({"id": "c1", "document_id": "d1", "text": "body", "score": 0.5}),
                score: 1.0,
            },
        ];
        let normalized = normalize_recall_items(&items, SearchType::Chunks, None, &HashMap::new());
        assert_eq!(normalized.len(), 1);
        let v = serde_json::to_value(&normalized[0]).unwrap();
        assert_eq!(v["kind"], "chunk");
        assert_eq!(v["search_type"], "CHUNKS");
        assert_eq!(v["text"], "body");
        assert_eq!(v["score"], 0.5);
        assert_eq!(v["metadata"], json!({"data_id": "d1", "chunk_id": "c1"}));
        assert_eq!(v["source"], "graph");
        assert!(v["system_prompt"].is_null());
    }

    /// An `only_context` graph response (user prompt set) stamps its system
    /// prompt on the normalized entry, like Python's `normalize_search_payload`.
    #[test]
    fn normalize_recall_items_carries_only_context_system_prompt() {
        let mut response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            cognee::search::SearchOutput::Text("rendered user prompt".into()),
        );
        response.user_prompt = Some("rendered user prompt".into());
        response.system_prompt = Some("system message".into());
        let items = vec![RecallItem {
            source: RecallSource::Graph,
            content: json!("rendered user prompt"),
            score: 1.0,
        }];
        let normalized = normalize_recall_items(
            &items,
            SearchType::GraphCompletion,
            Some(&response),
            &HashMap::new(),
        );
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0].text, "rendered user prompt");
        assert_eq!(
            normalized[0].system_prompt.as_deref(),
            Some("system message")
        );
        assert_eq!(
            normalized[0].kind,
            cognee::search::RecallResultKind::GraphCompletion
        );
    }

    /// A recall merged over two datasets attaches each answer only its own
    /// dataset's evidence (Python's per-dataset payloads); a response over one
    /// dataset shares its list with every entry.
    #[test]
    fn normalize_recall_items_splits_evidence_per_dataset() {
        use cognee::search::{EvidenceKind, EvidenceReference};
        let stamped = |id: &str, dataset: &str| {
            let mut reference = EvidenceReference::new(EvidenceKind::Segment, id);
            reference.dataset_id = Some(dataset.to_string());
            reference
        };
        let mut response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            cognee::search::SearchOutput::Texts(vec!["a".into(), "b".into()]),
        );
        response.datasets = Some(vec![
            Uuid::parse_str(ID_A).unwrap(),
            Uuid::parse_str(ID_B).unwrap(),
        ]);
        response.evidence = vec![stamped("chunk-a", ID_A), stamped("chunk-b", ID_B)];
        let items: Vec<RecallItem> = ["a", "b"]
            .into_iter()
            .map(|text| RecallItem {
                source: RecallSource::Graph,
                content: json!(text),
                score: 1.0,
            })
            .collect();
        let evidence_ids = |entry: &NormalizedResult| -> Vec<String> {
            entry.metadata["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["artifact_id"].as_str().unwrap().to_string())
                .collect()
        };

        let normalized = normalize_recall_items(
            &items,
            SearchType::GraphCompletion,
            Some(&response),
            &HashMap::new(),
        );
        assert_eq!(evidence_ids(&normalized[0]), ["chunk-a"]);
        assert_eq!(evidence_ids(&normalized[1]), ["chunk-b"]);

        // Unaligned (one answer, two datasets): the dataset is unknown, so
        // the entry keeps every reference rather than none.
        let normalized = normalize_recall_items(
            &items[..1],
            SearchType::GraphCompletion,
            Some(&response),
            &HashMap::new(),
        );
        assert_eq!(evidence_ids(&normalized[0]), ["chunk-a", "chunk-b"]);
    }

    /// Each entry names the dataset it came from, as Python's per-dataset
    /// payload does: by position in a per-dataset fan-out, by its own
    /// membership, or as the response's only dataset. The name comes from
    /// the looked-up names; an id without one keeps a `null` name.
    /// A structured graph answer becomes a `STRUCTURED` entry carrying it.
    #[test]
    fn normalize_recall_items_marks_a_structured_answer() {
        let answer = json!({ "answer": "yes" });
        let response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            SearchOutput::Structured(answer.clone()),
        );
        let items = [RecallItem {
            source: RecallSource::Graph,
            content: answer.clone(),
            score: 1.0,
        }];
        let normalized = normalize_recall_items(
            &items,
            SearchType::GraphCompletion,
            Some(&response),
            &HashMap::new(),
        );
        assert_eq!(normalized.len(), 1);
        let wire = serde_json::to_value(&normalized[0]).unwrap();
        assert_eq!(wire["kind"], "structured");
        assert_eq!(wire["structured"], answer);
    }

    #[test]
    fn normalize_recall_items_name_each_entrys_dataset() {
        let (a, b) = (
            Uuid::parse_str(ID_A).unwrap(),
            Uuid::parse_str(ID_B).unwrap(),
        );
        let names = HashMap::from([(a, "alpha".to_string())]);
        let graph = |content: serde_json::Value| RecallItem {
            source: RecallSource::Graph,
            content,
            score: 1.0,
        };
        let dataset_of =
            |entry: &NormalizedResult| (entry.dataset_id.clone(), entry.dataset_name.clone());

        // Per-dataset fan-out of answers: by position.
        let mut response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            cognee::search::SearchOutput::Texts(vec!["a".into(), "b".into()]),
        );
        response.datasets = Some(vec![a, b]);
        let items = [graph(json!("a")), graph(json!("b"))];
        let normalized =
            normalize_recall_items(&items, SearchType::GraphCompletion, Some(&response), &names);
        assert_eq!(
            dataset_of(&normalized[0]),
            (Some(ID_A.to_string()), Some("alpha".to_string()))
        );
        assert_eq!(dataset_of(&normalized[1]), (Some(ID_B.to_string()), None));
        let wire = serde_json::to_value(&normalized[0]).unwrap();
        assert_eq!(wire["dataset_id"], ID_A);
        assert_eq!(wire["dataset_name"], "alpha");

        // Items carrying their own membership.
        let items = [graph(json!({"id": "c1", "text": "t", "dataset_id": ID_A}))];
        let normalized =
            normalize_recall_items(&items, SearchType::Chunks, Some(&response), &names);
        assert_eq!(
            dataset_of(&normalized[0]),
            (Some(ID_A.to_string()), Some("alpha".to_string()))
        );

        // One dataset searched: every entry is from it.
        response.datasets = Some(vec![a]);
        let items = [
            graph(json!({"id": "c1", "text": "t"})),
            graph(json!({"id": "c2", "text": "u"})),
        ];
        let normalized =
            normalize_recall_items(&items, SearchType::Chunks, Some(&response), &names);
        assert!(
            normalized
                .iter()
                .all(|entry| dataset_of(entry)
                    == (Some(ID_A.to_string()), Some("alpha".to_string())))
        );

        // Nothing recovers it: both stay null.
        let normalized = normalize_recall_items(&items, SearchType::Chunks, None, &names);
        assert!(
            normalized
                .iter()
                .all(|entry| dataset_of(entry) == (None, None))
        );
    }

    /// `TRIPLET_COMPLETION` was removed (SDK-1039); it is an unknown type, and
    /// the error does not list it among the valid values.
    #[test]
    fn parse_search_type_rejects_triplet_completion() {
        let err = parse_search_type("TRIPLET_COMPLETION").expect_err("removed type");
        let message = err.to_string();
        assert!(message.contains("unknown SearchType"), "{message}");
        let (_, valid) = message
            .split_once("Valid values:")
            .expect("the error lists the valid values");
        assert!(!valid.contains("TRIPLET"), "{message}");
    }

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

    /// `datasets` is the more dangerous half of the same bug: the
    /// orchestrator's name branch is guarded on `!names.is_empty()`, so a list
    /// that `filter_map`s down to empty does not merely lose one name — it
    /// falls through to *no dataset filter at all*.
    #[test]
    fn parse_dataset_names_rejects_malformed_entries_instead_of_widening_scope() {
        for coerce in [false, true] {
            assert!(parse_dataset_names(&json!({ "datasets": [123] }), coerce).is_err());
            assert!(parse_dataset_names(&json!({ "datasets": ["ok", null] }), coerce).is_err());
            // A non-string, non-array scalar is a caller mistake on both paths:
            // coercion only ever wraps a *string*.
            assert!(parse_dataset_names(&json!({ "datasets": 7 }), coerce).is_err());
            assert!(parse_dataset_names(&json!({ "datasets": {"a": 1} }), coerce).is_err());
        }
    }

    #[test]
    fn parse_dataset_names_reads_a_string_array() {
        for coerce in [false, true] {
            assert_eq!(
                parse_dataset_names(&json!({ "datasets": ["a", "b"] }), coerce).unwrap(),
                Some(vec!["a".to_string(), "b".to_string()])
            );
            assert_eq!(parse_dataset_names(&json!({}), coerce).unwrap(), None);
            assert_eq!(
                parse_dataset_names(&json!({ "datasets": null }), coerce).unwrap(),
                None
            );
            // Deliberately empty stays "no filter", matching Python's `if datasets:`.
            assert_eq!(
                parse_dataset_names(&json!({ "datasets": [] }), coerce).unwrap(),
                Some(vec![])
            );
        }
    }

    /// Python's `search` wraps a bare string into a one-element list
    /// (`cognee/api/v1/search/search.py:294-295`), and the Python shim forwards
    /// `datasets` verbatim (`python/cognee_py/compat.py:325-326`), so
    /// `cognee.search("q", datasets="docs")` scopes to `docs` upstream.
    ///
    /// This never worked in Rust: before PR #214 a bare string failed
    /// `as_array()`, fell through to `None`, and ran a *silently unscoped*
    /// search over every dataset the caller can read; #214 turned that into a
    /// hard error. Neither scoped. This is the first version that does.
    #[test]
    fn a_bare_dataset_string_scopes_a_search_the_way_python_does() {
        assert_eq!(
            parse_dataset_names(&json!({ "datasets": "docs" }), true).unwrap(),
            Some(vec!["docs".to_string()])
        );

        let req = build_search_request("q", &json!({ "datasets": "docs" }), Uuid::new_v4())
            .expect("a bare `datasets` string is what Python accepts");
        assert_eq!(
            req.datasets,
            Some(vec!["docs".to_string()]),
            "a bare string must scope to that one dataset, not widen to all of them"
        );
    }

    /// `recall` stays strict. Python's `recall` declares
    /// `datasets: list[str] | None` (`cognee/api/v1/recall/recall.py:344`) and
    /// normalizes nothing, so a bare string iterates its characters and raises
    /// `DatasetNotFoundError`. Python rejects it; Rust's explicit error is the
    /// same verdict, earlier and better worded.
    #[test]
    fn recall_still_rejects_a_bare_dataset_string() {
        assert!(parse_dataset_names(&json!({ "datasets": "docs" }), false).is_err());
        assert!(build_recall_args(&json!({ "datasets": "docs" })).is_err());
    }

    /// `dataset_ids` stays strict on both paths. Python declares
    /// `Optional[Union[list[UUID], UUID]]` (`search.py:46`) but only ever
    /// normalizes a bare UUID for *telemetry* (`search.py:340-341`,
    /// `modules/search/methods/search.py:50`); the scope value handed on at
    /// `search.py:377` is the raw one, and
    /// `modules/search/methods/search.py:111` calls `len()` on it — a bare
    /// UUID `TypeError`s. That is a bug upstream, not a behaviour to port.
    #[test]
    fn a_bare_dataset_id_stays_rejected_on_both_paths() {
        let opts = json!({ "datasetIds": ID_A });
        assert!(parse_dataset_ids(&opts).is_err());
        assert!(build_recall_args(&opts).is_err());
        assert!(build_search_request("q", &opts, Uuid::new_v4()).is_err());
    }

    #[test]
    fn a_malformed_dataset_name_fails_both_ops() {
        let opts = json!({ "datasets": [123] });
        assert!(build_recall_args(&opts).is_err());
        assert!(build_search_request("q", &opts, Uuid::new_v4()).is_err());
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

    /// Per-retriever knobs must survive the camelCase hop verbatim: the
    /// retriever reads them out of `retriever_specific_config` by name, so a
    /// binding that parses the object into a typed subset would silently drop
    /// every knob it had not heard of. `text_summaries_top_k` is the one the
    /// hybrid retriever's chunk lane reads; the nonsense key alongside it
    /// asserts the pass-through is not a whitelist.
    #[test]
    fn build_search_request_carries_retriever_specific_config() {
        let opts = json!({
            "retrieverSpecificConfig": {
                "text_summaries_top_k": 3,
                "include_external_metadata": true,
                "external_metadata_keys": ["created_at"],
                "not_a_real_knob": "x"
            }
        });
        let req = build_search_request("q", &opts, Uuid::new_v4()).unwrap();
        let config = req
            .retriever_specific_config
            .expect("retrieverSpecificConfig must reach the request");
        assert_eq!(config.get("text_summaries_top_k"), Some(&json!(3)));
        assert_eq!(config.get("include_external_metadata"), Some(&json!(true)));
        assert_eq!(
            config.get("external_metadata_keys"),
            Some(&json!(["created_at"]))
        );
        assert_eq!(config.get("not_a_real_knob"), Some(&json!("x")));
    }

    /// `nodeNameFilterOperator` reaches the request normalized the way Python
    /// normalizes it (`.strip().upper()`, `search.py:311-317`); it was
    /// hard-coded to `None`, so a binding caller could not ask for AND.
    #[test]
    fn build_search_request_carries_the_node_name_filter_operator() {
        let opts = json!({ "nodeName": ["a", "b"], "nodeNameFilterOperator": " and " });
        let req = build_search_request("q", &opts, Uuid::new_v4()).unwrap();
        assert_eq!(req.node_name_filter_operator.as_deref(), Some("AND"));

        let req = build_search_request("q", &json!({}), Uuid::new_v4()).unwrap();
        assert_eq!(req.node_name_filter_operator, None);
    }

    #[test]
    fn build_search_request_rejects_an_unknown_node_name_filter_operator() {
        for bad in [json!("XOR"), json!(1)] {
            let opts = json!({ "nodeNameFilterOperator": bad });
            assert!(
                matches!(
                    build_search_request("q", &opts, Uuid::new_v4()),
                    Err(SdkError::Validation(_))
                ),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn neighborhood_opts_reach_search_and_recall() {
        let opts = json!({ "neighborhoodDepth": 2, "neighborhoodSeedTopK": 5 });
        let req = build_search_request("q", &opts, Uuid::new_v4()).unwrap();
        assert_eq!(req.neighborhood_depth, Some(2));
        assert_eq!(req.neighborhood_seed_top_k, Some(5));
        let args = build_recall_args(&opts).unwrap();
        assert_eq!(args.neighborhood_depth, Some(2));
        assert_eq!(args.neighborhood_seed_top_k, Some(5));

        let req = build_search_request("q", &json!({}), Uuid::new_v4()).unwrap();
        assert_eq!(
            (req.neighborhood_depth, req.neighborhood_seed_top_k),
            (None, None)
        );
    }

    #[test]
    fn non_positive_neighborhood_opts_are_rejected() {
        for opts in [
            json!({ "neighborhoodDepth": 0 }),
            json!({ "neighborhoodDepth": -1 }),
            json!({ "neighborhoodDepth": 1.5 }),
            json!({ "neighborhoodSeedTopK": 0 }),
            json!({ "neighborhoodSeedTopK": "3" }),
        ] {
            assert!(
                matches!(
                    build_search_request("q", &opts, Uuid::new_v4()),
                    Err(SdkError::Validation(_))
                ),
                "search must reject {opts}"
            );
            assert!(
                matches!(build_recall_args(&opts), Err(SdkError::Validation(_))),
                "recall must reject {opts}"
            );
        }
    }

    /// Absent, the field stays `None` rather than becoming an empty map — the
    /// retrievers branch on `Option`, and an empty map is a different statement
    /// ("caller sent an empty config") from "caller sent none".
    #[test]
    fn build_search_request_leaves_retriever_specific_config_unset() {
        let req = build_search_request("q", &json!({}), Uuid::new_v4()).unwrap();
        assert_eq!(req.retriever_specific_config, None);
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

    /// An absent `tenant` stays `None` — a tenant-less caller, which reaches
    /// only tenant-less datasets (strict equality) — which is what the
    /// single-tenant default needs, since those rows carry a NULL tenant.
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
        assert_eq!(args.tools_trigger, ToolsTrigger::Always);
        assert_eq!(args.context_profile, None);
        assert_eq!(args.node_scope, NodeScope::default());
        assert_eq!(args.verbose, None);
    }

    /// The SDK-1040 recall opts reach the [`RecallOptions`] that
    /// `cognee::api::recall` receives — parsing them is not enough, the
    /// `datasetIds` regression was a parsed key that was never handed on.
    #[test]
    fn sdk_1040_recall_opts_reach_the_recall_options() {
        let opts = json!({
            "toolsTrigger": "on_empty",
            "contextProfile": "agent",
            "nodeType": "Entity",
            "nodeName": ["Alice", 7, "Bob"],
            "nodeNameFilterOperator": " and ",
            "verbose": true,
        });
        let options = build_recall_args(&opts).unwrap().recall_options();
        assert_eq!(options.tools_trigger, ToolsTrigger::OnEmpty);
        assert_eq!(options.context_profile.as_deref(), Some("agent"));
        assert_eq!(options.node_type.as_deref(), Some("Entity"));
        assert_eq!(
            options.node_name,
            Some(vec!["Alice".to_string(), "Bob".to_string()])
        );
        assert_eq!(options.node_name_filter_operator.as_deref(), Some("AND"));
        assert_eq!(options.verbose, Some(true));

        let defaults = build_recall_args(&json!({})).unwrap().recall_options();
        assert_eq!(defaults.tools_trigger, ToolsTrigger::Always);
        assert_eq!(defaults.context_profile, None);
        assert_eq!(defaults.node_type, None);
        assert_eq!(defaults.node_name, None);
        assert_eq!(defaults.verbose, None);
    }

    /// The pre-existing opts are carried into the options too, so moving the
    /// `RecallOptions` construction out of `recall` dropped none of them.
    #[test]
    fn recall_options_keep_the_existing_opts() {
        let tenant = Uuid::new_v4();
        let opts = json!({
            "tenant": tenant.to_string(),
            "neighborhoodDepth": 2,
            "neighborhoodSeedTopK": 3,
            "includeReferences": true,
        });
        let options = build_recall_args(&opts).unwrap().recall_options();
        assert_eq!(options.tenant_id, Some(tenant));
        assert_eq!(options.neighborhood_depth, Some(2));
        assert_eq!(options.neighborhood_seed_top_k, Some(3));
        assert!(options.include_references);
    }

    /// Search and recall read the node scope and `verbose` through the same
    /// parsers, so the two ops cannot drift.
    #[test]
    fn search_and_recall_parse_the_node_scope_alike() {
        let opts = json!({
            "nodeType": "TextDocument",
            "nodeName": ["a"],
            "nodeNameFilterOperator": "or",
            "verbose": false,
        });
        let req = build_search_request("q", &opts, Uuid::new_v4()).unwrap();
        let args = build_recall_args(&opts).unwrap();
        assert_eq!(args.node_scope.node_type, req.node_type);
        assert_eq!(args.node_scope.node_name, req.node_name);
        assert_eq!(
            args.node_scope.node_name_filter_operator,
            req.node_name_filter_operator
        );
        assert_eq!(args.verbose, req.verbose);

        let bad = json!({ "nodeNameFilterOperator": "XOR" });
        assert!(matches!(
            build_recall_args(&bad),
            Err(SdkError::Validation(_))
        ));
    }

    /// `toolsTrigger` is validated on every call with Python's message
    /// (`recall.py:596-600`); null is the default, and a non-string is refused
    /// like an unknown string.
    #[test]
    fn tools_trigger_is_validated_with_pythons_message() {
        assert_eq!(
            build_recall_args(&json!({ "toolsTrigger": "always" }))
                .unwrap()
                .tools_trigger,
            ToolsTrigger::Always
        );
        assert_eq!(
            build_recall_args(&json!({ "toolsTrigger": null }))
                .unwrap()
                .tools_trigger,
            ToolsTrigger::Always
        );
        for (bad, shown) in [
            (json!("sometimes"), "sometimes"),
            (json!("ALWAYS"), "ALWAYS"),
            (json!(true), "true"),
        ] {
            match build_recall_args(&json!({ "toolsTrigger": bad })) {
                Err(SdkError::Validation(message)) => assert!(
                    message.ends_with(&format!(
                        "Invalid tools_trigger '{shown}'. Valid values: 'always', 'on_empty'."
                    )),
                    "unexpected message: {message}"
                ),
                other => panic!("expected a validation error for {bad}, got {other:?}"),
            }
        }
    }

    /// `llmConfig` / `embeddingConfig` take Python's names and canonical keys,
    /// keep only the settings they give (the rest comes from the environment
    /// layer when the engines are built), ignore unknown keys, and refuse a
    /// non-object or a wrongly typed value.
    #[test]
    fn engine_configs_parse_into_the_settings_they_give() {
        let opts = json!({
            "llmConfig": { "llm_model": "gpt-4o", "llm.api_key": "sk-call", "unknown": 1 },
            "embeddingConfig": { "embedding_dimensions": 384, "embedding_provider": "onnx" },
        });
        let args = build_recall_args(&opts).unwrap();
        let llm = args.engine_configs.llm.as_ref().unwrap();
        assert_eq!(
            llm.values().keys().collect::<Vec<_>>(),
            ["llm.api_key", "llm.model"]
        );
        let section = llm.section();
        assert_eq!(section.model, "gpt-4o");
        assert_eq!(section.api_key, "sk-call");
        assert_eq!(section.provider, LlmConfig::default().provider);
        // The key never reaches `Debug`.
        assert!(!format!("{:?}", args.engine_configs).contains("sk-call"));
        let embedding = args.engine_configs.embedding.as_ref().unwrap().section();
        assert_eq!(embedding.dimensions, 384);
        assert_eq!(embedding.provider, "onnx");
        assert_eq!(embedding.batch_size, EmbeddingConfig::default().batch_size);
        // Search reads them with the same parser.
        assert_eq!(parse_engine_configs(&opts).unwrap(), args.engine_configs);

        let none = build_recall_args(&json!({ "llmConfig": null })).unwrap();
        assert_eq!(none.engine_configs, EngineConfigs::default());

        // A misplaced key is never echoed back in the error.
        let err = parse_engine_configs(&json!({ "llmConfig": "sk-secret" })).unwrap_err();
        assert!(
            matches!(&err, SdkError::Validation(m) if m == "`llmConfig` must be an object, got a string"),
            "{err:?}"
        );

        for bad in [
            json!({ "llmConfig": "gpt-4o" }),
            json!({ "embeddingConfig": { "embedding_dimensions": "wide" } }),
        ] {
            assert!(
                matches!(build_recall_args(&bad), Err(SdkError::Validation(_))),
                "{bad}"
            );
            assert!(matches!(
                parse_engine_configs(&bad),
                Err(SdkError::Validation(_))
            ));
        }
    }

    /// The configs resolve through the handle's component manager: none is
    /// no override, an embedding model of the wrong width is a validation
    /// error, and an unknown provider a component error.
    #[tokio::test]
    async fn engine_configs_resolve_through_the_component_manager() {
        let settings = cognee::Settings {
            sections: [
                ("embedding.mock".to_string(), json!("true")),
                ("embedding.dimensions".to_string(), json!(4)),
            ]
            .into(),
            ..cognee::Settings::default()
        };
        let cm = ComponentManager::new(cognee::ConfigManager::new(settings));
        assert!(
            EngineConfigs::default()
                .resolve(&cm)
                .await
                .unwrap()
                .is_none()
        );

        let embedding = |dimensions: u32| {
            CallSection::<EmbeddingConfig>::from_object(
                json!({ "mock_embedding": "true", "embedding_dimensions": dimensions })
                    .as_object()
                    .unwrap(),
            )
            .unwrap()
        };
        let fits = EngineConfigs {
            embedding: Some(embedding(4)),
            ..Default::default()
        };
        let engines = fits.resolve(&cm).await.unwrap().unwrap();
        assert_eq!(engines.embedding.unwrap().dimension(), 4);

        let too_wide = EngineConfigs {
            embedding: Some(embedding(8)),
            ..Default::default()
        };
        assert!(matches!(
            too_wide.resolve(&cm).await,
            Err(SdkError::Validation(_))
        ));

        let unknown = EngineConfigs {
            llm: Some(
                CallSection::from_object(
                    json!({ "llm_provider": "no-such-provider", "llm_api_key": "k" })
                        .as_object()
                        .unwrap(),
                )
                .unwrap(),
            ),
            ..Default::default()
        };
        assert!(matches!(
            unknown.resolve(&cm).await,
            Err(SdkError::Component(_))
        ));
    }

    /// The new scope names flow through `normalize_scope` unchanged.
    #[test]
    fn recall_accepts_the_sdk_1040_scope_names() {
        use cognee::api::RecallScope;
        let args =
            build_recall_args(&json!({ "scope": ["session_first", "session_context"] })).unwrap();
        assert_eq!(
            args.scope,
            Some(vec![RecallScope::SessionFirst, RecallScope::SessionContext])
        );
        let args = build_recall_args(&json!({ "scope": "tools" })).unwrap();
        assert_eq!(args.scope, Some(vec![RecallScope::Tools]));
    }

    /// Recall's refusals of the caller's input are validation errors; any
    /// other failure stays a runtime one.
    #[test]
    fn recall_errors_map_invalid_arguments_to_validation() {
        let message = "Recall scope 'tools' is not supported yet in the Rust SDK \
                       (tracked in SDK-1537).";
        match recall_error(ApiError::InvalidArgument(message.to_string())) {
            SdkError::Validation(m) => assert_eq!(m, message),
            other => panic!("expected Validation, got {other:?}"),
        }
        match recall_error(ApiError::Search("boom".to_string())) {
            SdkError::Runtime(m) => assert_eq!(m, "recall failed: Search error: boom"),
            other => panic!("expected Runtime, got {other:?}"),
        }
    }

    /// An explicit empty `scope` stays empty (the recall runs nothing, as
    /// Python's `normalize_scope([])` -> `[]`), distinct from a missing or
    /// null one (auto); a non-string element is refused, not dropped.
    #[test]
    fn build_recall_args_keeps_an_empty_scope_distinct_from_a_missing_one() {
        assert_eq!(
            build_recall_args(&json!({ "scope": [] })).unwrap().scope,
            Some(vec![])
        );
        assert_eq!(
            build_recall_args(&json!({ "scope": null })).unwrap().scope,
            Some(vec![cognee::api::RecallScope::Auto])
        );
        assert!(build_recall_args(&json!({ "scope": [1] })).is_err());
        assert!(build_recall_args(&json!({ "scope": ["graph", null] })).is_err());
    }

    /// `includeReferences` reaches both ops, defaults off, and a non-boolean
    /// is off rather than an error (the other boolean opts read the same way).
    #[test]
    fn include_references_is_threaded_through_search_and_recall() {
        let on = json!({ "includeReferences": true });
        assert!(
            build_search_request("q", &on, Uuid::new_v4())
                .unwrap()
                .include_references
        );
        assert!(build_recall_args(&on).unwrap().include_references);

        for off in [
            json!({}),
            json!({ "includeReferences": false }),
            json!({ "includeReferences": "yes" }),
        ] {
            assert!(
                !build_search_request("q", &off, Uuid::new_v4())
                    .unwrap()
                    .include_references
            );
            assert!(!build_recall_args(&off).unwrap().include_references);
        }
    }

    /// Recall's normalized entries carry the response's evidence as
    /// `metadata.evidence` (snake_case), and only when there is any.
    #[test]
    fn normalized_recall_entries_carry_the_evidence() {
        use cognee::search::{EvidenceKind, EvidenceReference, SearchOutput};

        let items = vec![RecallItem {
            source: RecallSource::Graph,
            content: json!("an answer"),
            score: 1.0,
        }];
        let mut response = SearchResponse::from_output(
            SearchType::GraphCompletion,
            SearchOutput::Text("x".into()),
        );

        let bare = normalize_recall_items(
            &items,
            SearchType::GraphCompletion,
            Some(&response),
            &HashMap::new(),
        );
        assert!(!bare[0].metadata.contains_key("evidence"));

        let mut reference = EvidenceReference::new(EvidenceKind::GraphNode, "node-a");
        reference.label = Some("Alpha".into());
        response.evidence = vec![reference];
        let entries = normalize_recall_items(
            &items,
            SearchType::GraphCompletion,
            Some(&response),
            &HashMap::new(),
        );
        let evidence = entries[0].metadata["evidence"].as_array().unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0]["kind"], "graph_node");
        assert_eq!(evidence[0]["artifact_id"], "node-a");
        assert_eq!(evidence[0]["label"], "Alpha");
        assert!(evidence[0].get("artifactId").is_none());
    }
}
