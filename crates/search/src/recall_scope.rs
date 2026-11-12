//! Recall-scope primitives and source helpers, lifted from
//! `cognee::api::recall` so that `cognee-http-server` can consume them
//! directly without
//! triggering the http-server -> lib cycle. See
//! `docs/http-api-v2/tasks/lib-08-recall-scope-lift.md` (Decision 18).
//!
//! This is a pure relocation of the LIB-07 work — no behavior change. The
//! only material adjustment is `normalize_scope`'s error type, which pivots
//! from `cognee`'s `ApiError::InvalidArgument` to this crate's
//! `SearchError::InvalidInput`. The error-message string stays byte-identical
//! to LIB-07 / Python parity.

use std::collections::HashSet;

use cognee_session::{
    CONTEXT_PROFILE_QA, SessionManager, SessionStore, build_active_context_block_for_profile_safe,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::graph_warmup::{ReadinessState, WarmupProbe};
use crate::observability::{
    COGNEE_RECALL_ROUTE_RULE, COGNEE_RECALL_WARMUP_SHORTCIRCUIT, COGNEE_RECALL_WARMUP_STATE,
    COGNEE_SEARCH_TYPE,
};
use crate::types::SearchError;
use crate::{
    ROUTER_FALLBACK_TYPE, RouteDecision, SearchOrchestrator, SearchRequest, SearchResponse,
    SearchType, default_route, pinned_route, route_query,
};

/// Advanced tuning options forwarded from `cognee::api::recall::RecallOptions`.
///
/// Kept as a separate struct so `cognee-search` does not need to depend on
/// `cognee`. The HTTP server and language bindings that build a
/// `SearchRequest` directly can use this type too.
#[derive(Debug, Clone, Default)]
pub struct RecallOptions {
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub node_name: Option<Vec<String>>,
    pub node_name_filter_operator: Option<String>,
    pub only_context: Option<bool>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    pub feedback_influence: Option<f32>,
    /// Per-call override of the deployment's automatic feedback setting.
    /// `None` delegates to `SearchTuning::auto_feedback_detection`.
    pub auto_feedback_detection: Option<bool>,
    pub neighborhood_depth: Option<usize>,
    pub neighborhood_seed_top_k: Option<usize>,
    /// Attach evidence references to the graph search
    /// (`SearchRequest::include_references`); they come back on the raw
    /// [`SearchResponse::evidence`] and in each normalized entry's
    /// `metadata.evidence`.
    pub include_references: bool,
    /// JSON Schema for a structured answer from the graph search
    /// (`SearchRequest::response_schema`): completion types return it as
    /// `SearchOutput::Structured`, conformed to the schema.
    pub response_schema: Option<serde_json::Value>,
    /// Retriever-specific per-request overrides forwarded unchanged to the
    /// selected graph retriever. HTTP and binding adapters should expose a
    /// typed, allowlisted surface rather than accepting arbitrary JSON.
    pub retriever_specific_config: Option<std::collections::HashMap<String, serde_json::Value>>,
    /// Tenant the requester belongs to. Forwarded to
    /// [`SearchRequest::tenant_id`], which scopes dataset names, explicit ids
    /// and the readable set to the caller's tenant by strict equality, as
    /// Python's `get_dataset_ids` does (`get_dataset_ids.py:34@37f788bd2`).
    /// `None` means a tenant-less caller: it reaches only datasets that have
    /// no tenant.
    pub tenant_id: Option<Uuid>,
    /// When the `tools` scope runs (Python's `tools_trigger`,
    /// `recall.py:464`). Validated where it is parsed (bindings, HTTP); it has
    /// no effect until the `tools` lane exists (SDK-1537), because a recall
    /// whose sources include `tools` is refused by [`resolve_sources`].
    pub tools_trigger: ToolsTrigger,
    /// The profile the `session_context` lane renders (Python's
    /// `context_profile`, `recall.py:453`): `"qa"` or `"agent"`. `None` is
    /// [`DEFAULT_CONTEXT_PROFILE`]. Not validated, as in Python: an unknown
    /// profile renders nothing. Other lanes ignore it.
    pub context_profile: Option<String>,
}

/// The `session_context` lane's profile when the caller names none (Python's
/// `context_profile: str = "qa"`, `recall.py:453`).
pub const DEFAULT_CONTEXT_PROFILE: &str = CONTEXT_PROFILE_QA;

impl RecallOptions {
    /// The profile the `session_context` lane renders: the caller's, or
    /// [`DEFAULT_CONTEXT_PROFILE`].
    pub fn context_profile_or_default(options: Option<&Self>) -> &str {
        options
            .and_then(|o| o.context_profile.as_deref())
            .unwrap_or(DEFAULT_CONTEXT_PROFILE)
    }
}

/// When recall's `tools` scope runs. Mirrors Python's `tools_trigger`
/// argument (`recall.py:464`, docstring at `:518-521`, validation at
/// `:596-600`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ToolsTrigger {
    /// Run the `tools` lane on every recall that includes it (Python's
    /// default).
    #[default]
    Always,
    /// Run the `tools` lane only when every other source returned nothing.
    OnEmpty,
}

impl ToolsTrigger {
    /// The wire name, as [`ToolsTrigger::from_str`] accepts it.
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolsTrigger::Always => "always",
            ToolsTrigger::OnEmpty => "on_empty",
        }
    }
}

impl std::str::FromStr for ToolsTrigger {
    type Err = SearchError;

    /// Parse `"always"` / `"on_empty"`. Anything else is
    /// [`SearchError::InvalidInput`] with Python's message, byte for byte
    /// (`recall.py:596-600`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "always" => Ok(ToolsTrigger::Always),
            "on_empty" => Ok(ToolsTrigger::OnEmpty),
            other => Err(SearchError::InvalidInput(format!(
                "Invalid tools_trigger '{other}'. Valid values: 'always', 'on_empty'."
            ))),
        }
    }
}

/// Source tag for recall results. Mirrors the discriminator strings emitted
/// by Python's `_search_session`, `_search_trace` and `_run_graph` helpers in
/// `cognee/api/v1/recall/recall.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallSource {
    Session,
    Graph,
    Trace,
    /// A note from recall itself, such as the memory still warming up.
    System,
    /// The session's active context block, from [`search_session_context`]
    /// (Python `_fetch_session_context`, `recall.py:384-424`).
    SessionContext,
}

impl RecallSource {
    /// Lowercase wire name (matches Python's `_source` literal).
    pub fn as_str(&self) -> &'static str {
        match self {
            RecallSource::Session => "session",
            RecallSource::Graph => "graph",
            RecallSource::Trace => "trace",
            RecallSource::System => "system",
            RecallSource::SessionContext => "session_context",
        }
    }
}

/// Scope selector. Mirrors Python's `RecallScope` literal
/// (`cognee/memory/entries.py:171-182`). `Auto` is the sentinel returned by
/// `normalize_scope(None)` and is resolved into concrete sources by
/// [`resolve_sources`]. `All` never appears in a normalized list — it expands
/// to the concrete sources during normalization.
///
/// `"graph_context"` is a deprecated alias of `"graph"` (Python `3401da38a`
/// removed the graph-context lane): [`normalize_scope`] maps it with a
/// warning, and serde reads it as [`RecallScope::Graph`].
///
/// `SessionFirst` is a strategy, not a source: [`resolve_sources`] turns it
/// into `[Session, Graph, ..]` with a session hit short-circuiting the graph.
/// `Tools` and `Code` are accepted names whose lanes Rust does not have yet:
/// [`resolve_sources`] refuses them with [`SearchError::NotSupported`]
/// (SDK-1537).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallScope {
    Auto,
    #[serde(alias = "graph_context")]
    Graph,
    Session,
    SessionFirst,
    Trace,
    SessionContext,
    Tools,
    Code,
}

/// Deprecated scope names and the scope each now means (Python's
/// `_DEPRECATED_SCOPE_ALIASES`).
const DEPRECATED_SCOPE_ALIASES: &[(&str, &str)] = &[("graph_context", "graph")];

impl RecallScope {
    /// The concrete sources, in the canonical order Python uses when
    /// expanding `"all"` (`entries.py:242`).
    pub const ALL: &'static [Self] = &[
        Self::Graph,
        Self::Session,
        Self::Trace,
        Self::SessionContext,
    ];

    /// The scope's wire name, as `normalize_scope` accepts it.
    pub fn as_wire(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Graph => "graph",
            Self::Session => "session",
            Self::SessionFirst => "session_first",
            Self::Trace => "trace",
            Self::SessionContext => "session_context",
            Self::Tools => "tools",
            Self::Code => "code",
        }
    }

    fn from_wire(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "graph" => Some(Self::Graph),
            "session" => Some(Self::Session),
            "session_first" => Some(Self::SessionFirst),
            "trace" => Some(Self::Trace),
            "session_context" => Some(Self::SessionContext),
            "tools" => Some(Self::Tools),
            "code" => Some(Self::Code),
            _ => None,
        }
    }

    /// Map an already-resolved `RecallScope` to the `RecallSource` its lane
    /// tags results with. `None` for the sentinels (`Auto`, `SessionFirst`)
    /// and for the scopes without a Rust lane yet (`Tools`, `Code`).
    pub fn as_source(&self) -> Option<RecallSource> {
        match self {
            RecallScope::Auto
            | RecallScope::SessionFirst
            | RecallScope::Tools
            | RecallScope::Code => None,
            RecallScope::Graph => Some(RecallSource::Graph),
            RecallScope::Session => Some(RecallSource::Session),
            RecallScope::Trace => Some(RecallSource::Trace),
            RecallScope::SessionContext => Some(RecallSource::SessionContext),
        }
    }

    /// Whether Rust has a lane for this scope. `Tools` and `Code` are valid
    /// names that [`resolve_sources`] refuses until SDK-1537.
    fn is_supported(&self) -> bool {
        !matches!(self, RecallScope::Tools | RecallScope::Code)
    }
}

/// Untyped input accepted by [`normalize_scope`] -- mirrors Python's
/// `str | list[str] | None` (`entries.py:207`). HTTP/CLI layers can
/// build a `ScopeInput` directly from JSON; callers with a strongly-typed
/// `Vec<RecallScope>` can pass it via `recall()`'s `scope` parameter without
/// going through this helper.
#[derive(Debug, Clone)]
pub enum ScopeInput {
    Single(String),
    Many(Vec<String>),
}

impl From<&str> for ScopeInput {
    fn from(s: &str) -> Self {
        ScopeInput::Single(s.to_string())
    }
}

impl From<String> for ScopeInput {
    fn from(s: String) -> Self {
        ScopeInput::Single(s)
    }
}

impl From<Vec<String>> for ScopeInput {
    fn from(v: Vec<String>) -> Self {
        ScopeInput::Many(v)
    }
}

/// A scope list with each repeat dropped, first occurrence kept
/// (`entries.py:251-258`). `Auto` is kept like any other scope.
pub fn dedupe_scopes(scopes: impl IntoIterator<Item = RecallScope>) -> Vec<RecallScope> {
    let mut seen = HashSet::new();
    scopes
        .into_iter()
        .filter(|scope| seen.insert(*scope))
        .collect()
}

/// Python's `repr(str)`: single quotes, unless the text holds a `'` and no
/// `"`, then double quotes. Backslashes and the chosen quote are escaped, as
/// are `\n`, `\r`, `\t` and the other ASCII control characters (`\xNN`).
/// Non-ASCII text is kept as is, which matches Python for printable
/// characters; Python would also escape non-printable non-ASCII code points
/// (`\x85`, `\u2028`, ...), which this does not, as scope names never hold
/// them in practice.
fn python_str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr(list[str])`: `['a', 'b']`.
fn python_list_repr(items: &[&str]) -> String {
    let inner: Vec<String> = items.iter().map(|item| python_str_repr(item)).collect();
    format!("[{}]", inner.join(", "))
}

/// Normalize the recall ``scope`` parameter to a concrete source list,
/// mirroring Python's `normalize_scope` at `cognee/memory/entries.py:207-258`.
///
/// - `None` -> `[Auto]` (Python: `["auto"]`).
/// - `"all"` -> `[Graph, Session, Trace, SessionContext]`, plus `Tools` and then
///   `SessionFirst` when the caller supplied them next to `"all"`; `"code"`
///   never rides along (`entries.py:238-249`).
/// - `"graph_context"` -> `Graph`, with a deprecation warning (Python's
///   `_DEPRECATED_SCOPE_ALIASES`).
/// - Single string -> singleton list.
/// - List of strings -> order-preserving dedup (`entries.py:251-258`).
/// - Unknown values -> `Err(SearchError::InvalidInput(...))` with the
///   Python-parity error message (`entries.py:225-229`).
pub fn normalize_scope(input: Option<ScopeInput>) -> Result<Vec<RecallScope>, SearchError> {
    let raw: Vec<String> = match input {
        None => return Ok(vec![RecallScope::Auto]),
        Some(ScopeInput::Single(s)) => vec![s],
        Some(ScopeInput::Many(v)) => v,
    };

    if raw.is_empty() {
        // Python passes `[]` through to the unknown-check (which finds none)
        // and the dedup loop (which yields `[]`). An empty vector is a valid
        // (if useless) result. Match that exactly: empty input -> empty list.
        return Ok(vec![]);
    }

    // Collect unknowns *in encounter order* — matches Python's
    // `[s for s in scopes if s not in _VALID_SCOPES]` (`entries.py:225`).
    // `_VALID_SCOPES` includes `"all"` even though it doesn't map to a
    // `RecallScope` variant — it's the expansion sentinel.
    fn is_valid_wire(s: &str) -> bool {
        s == "all"
            || DEPRECATED_SCOPE_ALIASES.iter().any(|(old, _)| *old == s)
            || RecallScope::from_wire(s).is_some()
    }
    let unknown: Vec<&str> = raw
        .iter()
        .filter(|s| !is_valid_wire(s))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        // Python sorts `_VALID_SCOPES` for the error message
        // (`entries.py:228` -- `sorted(_VALID_SCOPES)`).
        let valid_sorted = [
            "all",
            "auto",
            "code",
            "graph",
            "graph_context",
            "session",
            "session_context",
            "session_first",
            "tools",
            "trace",
        ];
        // Match Python's `f"Unknown recall scope(s): {unknown}. Valid values: {sorted(_VALID_SCOPES)}"`
        // byte for byte: both lists render as Python's `repr(list)` does,
        // single-quoted (`['foo']`), not Rust's double-quoted `{:?}`.
        return Err(SearchError::InvalidInput(format!(
            "Unknown recall scope(s): {}. Valid values: {}",
            python_list_repr(&unknown),
            python_list_repr(&valid_sorted)
        )));
    }

    // Deprecated names map to their replacement, with one warning per call
    // (Python's `normalize_scope`).
    let raw: Vec<String> = if raw
        .iter()
        .any(|s| DEPRECATED_SCOPE_ALIASES.iter().any(|(old, _)| old == s))
    {
        warn!(
            "Recall scope 'graph_context' is deprecated and now maps to 'graph'. \
             Use scope='graph' instead."
        );
        raw.into_iter()
            .map(|s| {
                DEPRECATED_SCOPE_ALIASES
                    .iter()
                    .find(|(old, _)| *old == s)
                    .map_or(s, |(_, new)| (*new).to_string())
            })
            .collect()
    } else {
        raw
    };

    // `"all"` short-circuits to the canonical source list, in fixed order
    // (`entries.py:238-249`). `tools` is explicit opt-in, so it stays only
    // when supplied next to `"all"`; `session_first` is a strategy that
    // `"all"` neither implies nor replaces, so it is carried the same way.
    // `code` is dropped.
    if raw.iter().any(|s| s == "all") {
        let mut expanded = RecallScope::ALL.to_vec();
        for carried in [RecallScope::Tools, RecallScope::SessionFirst] {
            if raw.iter().any(|s| s == carried.as_wire()) {
                expanded.push(carried);
            }
        }
        return Ok(expanded);
    }

    // Every name is valid here, so each maps to its scope.
    Ok(dedupe_scopes(
        raw.iter().filter_map(|s| RecallScope::from_wire(s)),
    ))
}

/// The concrete sources a recall runs, and whether a session hit skips the
/// graph. Built by [`resolve_sources`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSources {
    /// The scopes to run, in order.
    pub sources: Vec<RecallScope>,
    /// `true` when a session hit short-circuits the graph lane (the `auto`
    /// session-only case, or an explicit `session_first`).
    pub auto_fallthrough: bool,
}

/// Resolve a normalized scope list into the sources a recall runs, as
/// Python's `recall` does (`recall.py:574-594`). The one shared copy of these
/// rules: `cognee::api::recall` and the HTTP recall router both call it.
///
/// - An empty list resolves to no sources at all, so the recall runs nothing
///   and returns `[]`, as Python's `normalize_scope([])` -> `[]` does. Only
///   an absent scope means auto: callers map `None` to `[Auto]` before
///   calling this. Repeats are dropped.
/// - `[Auto]` resolves by what the caller supplied (`recall.py:575-585`):
///   - `session_id` with no dataset filter and no `query_type` ->
///     `[Session, Graph]`, a session hit skipping the graph;
///   - `session_id` with a dataset filter and no `query_type` ->
///     `[Session, Graph]`, both contributing;
///   - otherwise (no `session_id`, or a `query_type`) -> `[Graph]`.
///
///   An empty `session_id` counts as none, as Python's truthiness check
///   does.
/// - Any other list containing `SessionFirst` -> `[Session, Graph]` followed
///   by the rest of the list without `SessionFirst`, `Session` and `Graph`,
///   a session hit skipping the graph (`recall.py:586-594`).
/// - Anything else runs as given.
///
/// A resolved list that names `Tools` or `Code` is refused with
/// [`SearchError::NotSupported`] naming the first such scope, so a recall
/// fails before any lane runs (SDK-1537 adds the lanes).
pub fn resolve_sources(
    normalized: Vec<RecallScope>,
    session_id: Option<&str>,
    has_dataset_filter: bool,
    query_type: Option<SearchType>,
) -> Result<ResolvedSources, SearchError> {
    let normalized = dedupe_scopes(normalized);

    let resolved = if normalized.as_slice() == [RecallScope::Auto] {
        // `if session_id and not has_dataset_scope and query_type is None`
        // / `elif session_id and query_type is None` / `else`.
        let has_session = session_id.is_some_and(|id| !id.is_empty());
        let (sources, auto_fallthrough) = match (has_session, has_dataset_filter, query_type) {
            (true, false, None) => (vec![RecallScope::Session, RecallScope::Graph], true),
            (true, true, None) => (vec![RecallScope::Session, RecallScope::Graph], false),
            _ => (vec![RecallScope::Graph], false),
        };
        ResolvedSources {
            sources,
            auto_fallthrough,
        }
    } else if normalized.contains(&RecallScope::SessionFirst) {
        let rest = normalized.into_iter().filter(|scope| {
            !matches!(
                scope,
                RecallScope::SessionFirst | RecallScope::Session | RecallScope::Graph
            )
        });
        ResolvedSources {
            sources: [RecallScope::Session, RecallScope::Graph]
                .into_iter()
                .chain(rest)
                .collect(),
            auto_fallthrough: true,
        }
    } else {
        ResolvedSources {
            sources: normalized,
            auto_fallthrough: false,
        }
    };

    if let Some(unsupported) = resolved.sources.iter().find(|scope| !scope.is_supported()) {
        return Err(SearchError::NotSupported(format!(
            "Recall scope '{}' is not supported yet in the Rust SDK (tracked in SDK-1537).",
            unsupported.as_wire()
        )));
    }
    Ok(resolved)
}

/// A single recall result item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallItem {
    /// The source of this result.
    pub source: RecallSource,
    /// Content (question, answer, search result text, or trace fields).
    pub content: serde_json::Value,
    /// Relevance score (keyword overlap for session/trace, similarity for graph).
    pub score: f64,
}

/// Mirrors Python `_search_session` (`recall.py:146-208`).
///
/// Returns empty when `session_id` is missing or the backend isn't wired in
/// (Python `recall.py:170-171`).
pub async fn search_session(
    query_text: &str,
    session_id: Option<&str>,
    user_id: Option<&str>,
    top_k: usize,
    store: Option<&dyn SessionStore>,
) -> Result<Vec<RecallItem>, SearchError> {
    let (Some(sid), Some(store)) = (session_id, store) else {
        // Python `_search_session`: missing session_id => caller resolves
        // empty per `_run_session` (`recall.py:431-432`); missing backend
        // => `is_available` short-circuit (`recall.py:170-171`).
        return Ok(vec![]);
    };

    let query_tokens = tokenize(query_text);
    if query_tokens.is_empty() {
        return Ok(vec![]);
    }

    let entries = store.get_all_qa_entries(sid, user_id).await?;
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let mut scored: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            // Python recall.py:191-194 — concat question + context + answer.
            let entry_text = format!(
                "{} {} {}",
                entry.question,
                entry.context.as_deref().unwrap_or(""),
                entry.answer,
            );
            let entry_tokens = tokenize(&entry_text);
            let overlap = query_tokens.intersection(&entry_tokens).count();
            (idx, overlap)
        })
        .filter(|(_, overlap)| *overlap > 0)
        .collect();

    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
    scored.truncate(top_k);

    Ok(scored
        .into_iter()
        .map(|(idx, overlap)| {
            let entry = &entries[idx];
            RecallItem {
                source: RecallSource::Session,
                content: serde_json::json!({
                    "question": entry.question,
                    "answer": entry.answer,
                    "context": entry.context,
                    "session_id": entry.session_id,
                    "created_at": entry.created_at.to_rfc3339(),
                }),
                score: overlap as f64,
            }
        })
        .collect())
}

/// Mirrors Python `_search_trace` (`recall.py:211-286`).
///
/// Tokenizes `origin_function`, `status`, `memory_query`, `memory_context`,
/// `session_feedback`, `error_message` plus JSON-serialized `method_params`
/// and `method_return_value`. Ranks by token-set intersection.
pub async fn search_trace(
    query_text: &str,
    session_id: Option<&str>,
    user_id: Option<&str>,
    top_k: usize,
    sm: Option<&SessionManager>,
) -> Result<Vec<RecallItem>, SearchError> {
    let (Some(sid), Some(sm)) = (session_id, sm) else {
        return Ok(vec![]);
    };
    // Python recall.py:227-228: caller_user_id falsy -> empty. Rust requires
    // a user_id string for `get_agent_trace_session`, so empty/None -> empty.
    let Some(uid) = user_id else {
        return Ok(vec![]);
    };
    if uid.is_empty() {
        return Ok(vec![]);
    }

    let query_tokens = tokenize(query_text);
    if query_tokens.is_empty() {
        return Ok(vec![]);
    }

    let entries = sm.get_agent_trace_session(uid, Some(sid), None).await?;
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let mut scored: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .map(|(idx, e)| {
            // Python recall.py:252-271 -- six string fields plus two
            // JSON-serialized fields.
            let mut parts: Vec<String> = vec![
                e.origin_function.clone(),
                e.status.clone(),
                e.memory_query.clone(),
                e.memory_context.clone(),
                e.session_feedback.clone(),
                e.error_message.clone(),
            ];
            // method_params is non-Option in Rust (default {}). Python
            // skips when `is None`; we always include the JSON serialization
            // since `{}` -> "{}" which contributes no tokens >=2 chars.
            match serde_json::to_string(&e.method_params) {
                Ok(s) => parts.push(s),
                Err(_) => parts.push(format!("{:?}", e.method_params)),
            }
            if let Some(ref mrv) = e.method_return_value {
                match serde_json::to_string(mrv) {
                    Ok(s) => parts.push(s),
                    Err(_) => parts.push(format!("{mrv:?}")),
                }
            }

            let joined = parts.join(" ");
            let entry_tokens = tokenize(&joined);
            let overlap = query_tokens.intersection(&entry_tokens).count();
            (idx, overlap)
        })
        .filter(|(_, overlap)| *overlap > 0)
        .collect();

    scored.sort_by_key(|s| std::cmp::Reverse(s.1));
    scored.truncate(top_k);

    Ok(scored
        .into_iter()
        .map(|(idx, overlap)| {
            let e = &entries[idx];
            RecallItem {
                source: RecallSource::Trace,
                content: serde_json::json!({
                    "trace_id": e.trace_id,
                    "origin_function": e.origin_function,
                    "status": e.status,
                    "memory_query": e.memory_query,
                    "memory_context": e.memory_context,
                    "method_params": e.method_params,
                    "method_return_value": e.method_return_value,
                    "error_message": e.error_message,
                    "session_feedback": e.session_feedback,
                }),
                score: overlap as f64,
            }
        })
        .collect())
}

/// Mirrors Python `_fetch_session_context` (`recall.py:384-424`): the
/// session's active context block for `context_profile`, as one item.
///
/// Read-only (`stamp_served` false, so no `last_served_at` is written) and
/// fail-open: a store error renders nothing rather than failing the recall,
/// as Python's builder does. Empty when the session id or the user is missing
/// or empty (`recall.py:398-400`, `:712-714`), when no session manager is
/// wired (`recall.py:405-406`), or when no entry of that profile renders
/// (`recall.py:417-418`). Otherwise one item whose content is
/// `{"content": <block>, "context_profile": <profile>}`
/// (`ResponseSessionContextEntry`, `RecallResponse.py:17-20`); the wire
/// `source` is added from [`RecallItem::source`].
pub async fn search_session_context(
    query_text: &str,
    session_id: Option<&str>,
    user_id: Option<&str>,
    context_profile: &str,
    sm: Option<&SessionManager>,
) -> Result<Vec<RecallItem>, SearchError> {
    let (Some(sid), Some(uid), Some(sm)) = (session_id, user_id, sm) else {
        return Ok(vec![]);
    };
    if sid.is_empty() || uid.is_empty() {
        return Ok(vec![]);
    }
    let (block, _served) = build_active_context_block_for_profile_safe(
        sm,
        sid,
        Some(uid),
        query_text,
        context_profile,
        false,
        &[] as &[&str],
    )
    .await;
    if block.is_empty() {
        return Ok(vec![]);
    }
    Ok(vec![RecallItem {
        source: RecallSource::SessionContext,
        content: serde_json::json!({
            "content": block,
            "context_profile": context_profile,
        }),
        score: 1.0,
    }])
}

/// A recall's graph search, decided once before anything runs: who asks, and
/// how to search. [`plan_graph`] makes it; the warm-up guard
/// ([`GraphPlan::warmup_note`]) and every [`run_graph`] of the recall share it.
#[derive(Debug, Clone, Copy)]
pub struct GraphPlan {
    owner: Option<Uuid>,
    decision: RouteDecision,
    /// The rule that picked the type, set only when the router chose it, so
    /// a retry never second-guesses an explicit type.
    routed_rule: Option<&'static str>,
    /// What a routed search that fails or finds nothing is retried as.
    fallback: RouteDecision,
}

/// Plans a recall's graph search: its owner, the caller's `user_id` as a UUID,
/// and how to search, recorded on `span`. An explicit `query_type` is used as given;
/// otherwise `auto_route` picks the type from the query text, or the
/// deployment's default does.
#[allow(clippy::too_many_arguments)]
pub fn plan_graph(
    query_text: &str,
    query_type: Option<SearchType>,
    auto_route: bool,
    datasets: Option<&[String]>,
    dataset_ids: Option<&[Uuid]>,
    user_id: Option<&str>,
    search_orchestrator: &SearchOrchestrator,
    span: &tracing::Span,
) -> Result<GraphPlan, SearchError> {
    let owner = graph_owner(user_id, datasets, dataset_ids)?;
    let constraints = search_orchestrator.routing_constraints();
    let fallback = default_route(&constraints);
    let (decision, routed_rule) = match (query_type, auto_route) {
        (Some(qt), _) => (pinned_route(qt, &constraints), None),
        (None, true) => {
            let decision = route_query(query_text, &constraints);
            (decision, Some(decision.rule))
        }
        (None, false) => (fallback, None),
    };
    span.record(
        COGNEE_SEARCH_TYPE,
        format!("{:?}", decision.search_type).as_str(),
    );
    if let Some(rule) = routed_rule {
        span.record(COGNEE_RECALL_ROUTE_RULE, rule);
    }
    Ok(GraphPlan {
        owner,
        decision,
        routed_rule,
        fallback,
    })
}

/// Lifted from the original recall body — runs the graph search via the
/// orchestrator. Mirrors Python's inline `_run_graph` closure
/// (`recall.py:455-493`), searching as `plan` decided. Returns `(items,
/// search_type_used, auto_routed, raw_response)`.
///
/// `dataset_ids` takes precedence over `datasets`: when it is non-empty the
/// names are never resolved (Python `recall.py:655-657`,
/// `search_dataset_ids = dataset_ids or None`). An empty list is the same as
/// `None`.
#[allow(clippy::too_many_arguments)]
pub async fn run_graph(
    plan: &GraphPlan,
    query_text: &str,
    datasets: Option<Vec<String>>,
    dataset_ids: Option<Vec<Uuid>>,
    top_k: usize,
    session_id: Option<&str>,
    search_orchestrator: &SearchOrchestrator,
    span: &tracing::Span,
    options: Option<&RecallOptions>,
) -> Result<(Vec<RecallItem>, SearchType, bool, SearchResponse), SearchError> {
    let GraphPlan {
        owner: owner_id,
        decision,
        routed_rule,
        ..
    } = *plan;
    let search_type = decision.search_type;
    let auto_routed = routed_rule.is_some();

    // Under per-dataset storage an unfiltered graph search covers the datasets
    // the caller may read, which needs a caller. With none (or one that is not
    // a UUID) there is nothing the caller can be shown to read, so the graph
    // source contributes nothing — the same answer as a caller who may read no
    // dataset (Python: zero per-dataset tasks, `[]`). It is not an error:
    // recall's other sources (session, trace) and its auto-mode
    // fall-through still run. Python has no such case, since `recall()`
    // resolves a missing user to the default user first.
    if search_orchestrator.scopes_unfiltered_search()
        && owner_id.is_none()
        && !has_dataset_filter(datasets.as_deref(), dataset_ids.as_deref())
    {
        debug!("recall: per-dataset storage and an unknown caller; no graph results");
        let response = empty_graph_response(search_type);
        return Ok((Vec::new(), search_type, auto_routed, response));
    }

    let caller_only_context = options.and_then(|o| o.only_context);
    let request = SearchRequest {
        query_text: query_text.to_string(),
        search_type,
        top_k: Some(top_k),
        datasets,
        dataset_ids,
        system_prompt: options.and_then(|o| o.system_prompt.clone()),
        system_prompt_path: options.and_then(|o| o.system_prompt_path.clone()),
        only_context: search_only_context(caller_only_context, decision),
        use_combined_context: None,
        session_id: session_id.map(|s| s.to_string()),
        node_type: None,
        node_name: options.and_then(|o| o.node_name.clone()),
        wide_search_top_k: options.and_then(|o| o.wide_search_top_k),
        triplet_distance_penalty: options.and_then(|o| o.triplet_distance_penalty),
        user_id: owner_id,
        tenant_id: options.and_then(|o| o.tenant_id),
        verbose: None,
        feedback_influence: options.and_then(|o| o.feedback_influence),
        retriever_specific_config: options.and_then(|o| o.retriever_specific_config.clone()),
        response_schema: options.and_then(|o| o.response_schema.clone()),
        custom_search_type: None,
        auto_feedback_detection: options.and_then(|o| o.auto_feedback_detection),
        node_name_filter_operator: options.and_then(|o| o.node_name_filter_operator.clone()),
        neighborhood_depth: options.and_then(|o| o.neighborhood_depth),
        neighborhood_seed_top_k: options.and_then(|o| o.neighborhood_seed_top_k),
        include_references: options.is_some_and(|o| o.include_references),
    };

    let (response, search_type) = plan
        .search_with_fallback(
            request,
            caller_only_context,
            |request| async move { search_orchestrator.search(&request).await },
            |response: &SearchResponse| is_empty(&response.result),
        )
        .await?;
    span.record(COGNEE_SEARCH_TYPE, format!("{search_type:?}").as_str());

    let items: Vec<RecallItem> = match &response.result {
        crate::SearchOutput::Items(search_items) => search_items
            .iter()
            .enumerate()
            .map(|(i, item)| RecallItem {
                source: RecallSource::Graph,
                content: serde_json::to_value(item)
                    .unwrap_or_else(|_| serde_json::Value::String(format!("{item:?}"))),
                score: 1.0 - (i as f64 * 0.01),
            })
            .collect(),
        crate::SearchOutput::Text(text) => vec![RecallItem {
            source: RecallSource::Graph,
            content: serde_json::Value::String(text.clone()),
            score: 1.0,
        }],
        crate::SearchOutput::Texts(texts) => texts
            .iter()
            .enumerate()
            .map(|(i, t)| RecallItem {
                source: RecallSource::Graph,
                content: serde_json::Value::String(t.clone()),
                score: 1.0 - (i as f64 * 0.01),
            })
            .collect(),
        // The answer itself, not its tagged `{"kind", "data"}` form.
        crate::SearchOutput::Structured(answer) => vec![RecallItem {
            source: RecallSource::Graph,
            content: answer.clone(),
            score: 1.0,
        }],
        other => vec![RecallItem {
            source: RecallSource::Graph,
            content: serde_json::to_value(other)
                .unwrap_or_else(|_| serde_json::Value::String(format!("{other:?}"))),
            score: 1.0,
        }],
    };

    Ok((items, search_type, auto_routed, response))
}

/// Whether a recall names any dataset: an empty list names none.
pub fn has_dataset_filter(datasets: Option<&[String]>, dataset_ids: Option<&[Uuid]>) -> bool {
    datasets.is_some_and(|d| !d.is_empty()) || dataset_ids.is_some_and(|d| !d.is_empty())
}

/// The graph search's owner: the caller's `user_id` as a UUID. The
/// orchestrator resolves dataset names owner-scoped and authorizes explicit
/// ids against their owner. Recall's `user_id` is a string because the
/// session helpers key on strings. A non-UUID id with a dataset filter is the
/// caller's mistake and an error; without a filter there is no owner scope to
/// apply, and no owner.
fn graph_owner(
    user_id: Option<&str>,
    datasets: Option<&[String]>,
    dataset_ids: Option<&[Uuid]>,
) -> Result<Option<Uuid>, SearchError> {
    let Some(raw) = user_id else {
        return Ok(None);
    };
    match Uuid::parse_str(raw) {
        Ok(id) => Ok(Some(id)),
        Err(error) if has_dataset_filter(datasets, dataset_ids) => {
            Err(SearchError::InvalidInput(format!(
                "dataset filter requires a UUID user_id to identify the owner, got '{raw}': {error}"
            )))
        }
        Err(_) => Ok(None),
    }
}

/// Routed types whose empty result is retried as [`ROUTER_FALLBACK_TYPE`]: a
/// lexical or rules search that finds nothing says nothing about the question.
const RETRY_ON_EMPTY: [SearchType; 2] = [SearchType::ChunksLexical, SearchType::CodingRules];

fn is_empty(output: &crate::SearchOutput) -> bool {
    match output {
        crate::SearchOutput::Items(items) => items.is_empty(),
        crate::SearchOutput::Rules(rules) => rules.is_empty(),
        _ => false,
    }
}

/// The caller's `only_context`, forced on when no LLM can write the answer.
fn search_only_context(caller: Option<bool>, decision: RouteDecision) -> Option<bool> {
    if decision.only_context {
        Some(true)
    } else {
        caller
    }
}

fn switch_to(
    request: &mut SearchRequest,
    decision: RouteDecision,
    caller_only_context: Option<bool>,
) {
    request.search_type = decision.search_type;
    request.only_context = search_only_context(caller_only_context, decision);
}

/// The note recall returns instead of searching a graph that is not built:
/// the last build's failure, or that memory is still warming up.
fn warmup_marker(probe: &WarmupProbe, threshold: u64) -> RecallItem {
    let (status, text) = if probe.state == ReadinessState::BuildFailed {
        let failure = probe.error_message.as_deref().unwrap_or("unknown error");
        let text = format!(
            "Memory build failed: the last ingestion for the requested datasets ended in an \
             error ({failure}). Fix the cause and re-run remember() or cognify()."
        );
        ("build_failed", text)
    } else {
        let text = "Memory is still warming up: no knowledge graph data exists yet for the \
                    requested datasets."
            .to_string();
        ("memory_warming_up", text)
    };
    let content = serde_json::json!({
        "status": status,
        "text": text,
        "datapoint_count": probe.datapoint_count,
        "threshold": threshold,
        "error_class": null,
        "error_message": probe.error_message,
    });
    RecallItem {
        source: RecallSource::System,
        content,
        score: 1.0,
    }
}

impl GraphPlan {
    /// The type the search runs as planned, before any fallback retry.
    pub fn search_type(&self) -> SearchType {
        self.decision.search_type
    }

    /// Whether the router chose the type.
    pub fn auto_routed(&self) -> bool {
        self.routed_rule.is_some()
    }

    /// The `only_context` a search on this plan sends: the caller's, forced
    /// on when no LLM can write the answer.
    pub fn only_context(&self, caller: Option<bool>) -> Option<bool> {
        search_only_context(caller, self.decision)
    }

    /// What a search that does not run reports: no items, over no dataset.
    pub fn empty_response(&self) -> SearchResponse {
        empty_graph_response(self.search_type())
    }

    /// Runs `search` on `request` with the plan's type and `only_context`
    /// (from `caller_only_context`) set on it, and retries
    /// as the fallback when the router's pick fails: a type the backend
    /// rejects, or a lexical or rules search that `found_nothing` judges
    /// empty. An explicit type is never retried. Returns the result and the
    /// type that answered.
    pub async fn search_with_fallback<T, F, Fut>(
        &self,
        mut request: SearchRequest,
        caller_only_context: Option<bool>,
        mut search: F,
        found_nothing: impl Fn(&T) -> bool,
    ) -> Result<(T, SearchType), SearchError>
    where
        F: FnMut(SearchRequest) -> Fut,
        Fut: std::future::Future<Output = Result<T, SearchError>>,
    {
        switch_to(&mut request, self.decision, caller_only_context);
        // The router picked something other than the fallback, so the
        // fallback is still untried and may replace this result.
        let routed_guess =
            self.routed_rule.is_some() && self.decision.search_type != ROUTER_FALLBACK_TYPE;
        let mut result = match search(request.clone()).await {
            Err(SearchError::UnsupportedSearchType(rejected)) if routed_guess => {
                info!(
                    ?rejected,
                    "recall: routed search type is not supported; retrying as the fallback"
                );
                switch_to(&mut request, self.fallback, caller_only_context);
                search(request.clone()).await?
            }
            result => result?,
        };
        if routed_guess && RETRY_ON_EMPTY.contains(&request.search_type) && found_nothing(&result) {
            info!(search_type = ?request.search_type, "recall: routed search found nothing; retrying as the fallback");
            switch_to(&mut request, self.fallback, caller_only_context);
            result = search(request.clone()).await?;
        }
        Ok((result, request.search_type))
    }

    /// Recall's warm-up guard: a note on the graph's state when none of the
    /// recall's datasets has a built graph, or `None` to search. It probes
    /// them all at once, so a caller that searches them one by one asks
    /// before the first. They are checked and resolved as the search would,
    /// so one the caller cannot read fails here.
    ///
    /// On a note the caller does not search. It answers the note when the
    /// graph is the recall's one source, nothing next to others, and reports
    /// [`Self::search_type`], [`Self::auto_routed`] and
    /// [`Self::empty_response`].
    ///
    /// `None` without a probe: with the short-circuit off, for a caller
    /// asking for `only_context` (who expects context, not a note), or
    /// without an owner to scope by. Python resolves a missing user to the
    /// default user and probes; this layer has no default user, so a cold
    /// graph is searched instead. The CLI, the bindings and the HTTP server
    /// always pass a caller.
    pub async fn warmup_note(
        &self,
        search_orchestrator: &SearchOrchestrator,
        datasets: Option<&[String]>,
        dataset_ids: Option<&[Uuid]>,
        span: &tracing::Span,
        options: Option<&RecallOptions>,
    ) -> Result<Option<RecallItem>, SearchError> {
        let Some(warmup) = search_orchestrator.graph_warmup() else {
            return Ok(None);
        };
        if !warmup.config().shortcircuit || options.and_then(|o| o.only_context).unwrap_or(false) {
            return Ok(None);
        }
        let Some(requester) = self.owner else {
            return Ok(None);
        };
        let tenant = options.and_then(|o| o.tenant_id);
        let probe = search_orchestrator
            .assess_recall_readiness(requester, tenant, datasets, dataset_ids)
            .await?;
        if probe.is_warm() {
            return Ok(None);
        }
        info!(
            state = probe.state.as_str(),
            "recall: the graph is not built; skipping the search"
        );
        span.record(COGNEE_RECALL_WARMUP_SHORTCIRCUIT, true);
        span.record(COGNEE_RECALL_WARMUP_STATE, probe.state.as_str());
        Ok(Some(warmup_marker(&probe, warmup.config().threshold)))
    }
}

/// The response of a graph search that did not run: no items, over no
/// dataset.
fn empty_graph_response(search_type: SearchType) -> SearchResponse {
    crate::orchestration::prepare_search_result(
        search_type,
        crate::SearchOutput::Items(Vec::new()),
        None,
        Some(Vec::new()),
        false,
        false,
        false,
    )
}

/// Tokenize text into lowercase words of length >= 2.
fn tokenize(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
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

    /// Plans the graph search, then runs it.
    #[allow(clippy::too_many_arguments)]
    async fn plan_and_run(
        query_text: &str,
        query_type: Option<SearchType>,
        datasets: Option<Vec<String>>,
        dataset_ids: Option<Vec<Uuid>>,
        top_k: usize,
        auto_route: bool,
        session_id: Option<&str>,
        user_id: Option<&str>,
        search_orchestrator: &SearchOrchestrator,
        span: &tracing::Span,
        options: Option<&RecallOptions>,
    ) -> Result<(Vec<RecallItem>, SearchType, bool, SearchResponse), SearchError> {
        let plan = plan_graph(
            query_text,
            query_type,
            auto_route,
            datasets.as_deref(),
            dataset_ids.as_deref(),
            user_id,
            search_orchestrator,
            span,
        )?;
        run_graph(
            &plan,
            query_text,
            datasets,
            dataset_ids,
            top_k,
            session_id,
            search_orchestrator,
            span,
            options,
        )
        .await
    }

    /// A plan in which the router picked `search_type`.
    fn routed_plan(search_type: SearchType) -> GraphPlan {
        let decision = |search_type, rule| RouteDecision {
            search_type,
            rule,
            only_context: false,
        };
        GraphPlan {
            owner: None,
            decision: decision(search_type, "test_rule"),
            routed_rule: Some("test_rule"),
            fallback: decision(ROUTER_FALLBACK_TYPE, "default"),
        }
    }

    /// A request with only a type and `only_context` set.
    fn bare_request(search_type: SearchType, only_context: Option<bool>) -> SearchRequest {
        let mut request: SearchRequest =
            serde_json::from_str(r#"{"query_text": "q"}"#).expect("a query is a whole request");
        request.search_type = search_type;
        request.only_context = only_context;
        request
    }

    #[tokio::test]
    async fn search_with_fallback_searches_as_planned_and_retries_an_empty_pick() {
        let plan = routed_plan(SearchType::CodingRules);
        let mut seen = Vec::new();
        let (found, answered) = plan
            .search_with_fallback(
                bare_request(SearchType::Chunks, Some(true)),
                Some(false),
                |request: SearchRequest| {
                    seen.push((request.search_type, request.only_context));
                    let found = request.search_type == ROUTER_FALLBACK_TYPE;
                    async move { Ok::<bool, SearchError>(found) }
                },
                |found: &bool| !found,
            )
            .await
            .expect("search");
        assert!(found);
        assert_eq!(answered, ROUTER_FALLBACK_TYPE);
        assert_eq!(
            seen,
            [
                (SearchType::CodingRules, Some(false)),
                (ROUTER_FALLBACK_TYPE, Some(false)),
            ]
        );
    }

    #[tokio::test]
    async fn search_with_fallback_keeps_the_forced_only_context_on_retry() {
        let mut plan = routed_plan(SearchType::CodingRules);
        plan.decision.only_context = true;
        plan.fallback.only_context = true;
        let mut seen = Vec::new();
        plan.search_with_fallback(
            bare_request(SearchType::Chunks, Some(false)),
            Some(false),
            |request: SearchRequest| {
                seen.push(request.only_context);
                async move { Ok::<bool, SearchError>(false) }
            },
            |found: &bool| !found,
        )
        .await
        .expect("search");
        assert_eq!(seen, [Some(true), Some(true)]);
    }

    #[tokio::test]
    async fn search_with_fallback_retries_a_rejected_pick_as_planned() {
        let plan = routed_plan(SearchType::CodingRules);
        let mut seen = Vec::new();
        let (_, answered) = plan
            .search_with_fallback(
                bare_request(SearchType::Chunks, None),
                Some(true),
                |request: SearchRequest| {
                    seen.push((request.search_type, request.only_context));
                    let result = match request.search_type {
                        SearchType::CodingRules => {
                            Err(SearchError::UnsupportedSearchType(SearchType::CodingRules))
                        }
                        _ => Ok(true),
                    };
                    async move { result }
                },
                |found: &bool| !found,
            )
            .await
            .expect("the fallback answers");
        assert_eq!(answered, ROUTER_FALLBACK_TYPE);
        assert_eq!(
            seen,
            [
                (SearchType::CodingRules, Some(true)),
                (ROUTER_FALLBACK_TYPE, Some(true)),
            ]
        );
    }

    #[test]
    fn tokenize_splits_and_lowercases() {
        let tokens = tokenize("Hello, World! How are you?");
        assert!(tokens.contains("hello"));
        assert!(tokens.contains("world"));
        assert!(tokens.contains("how"));
        assert!(tokens.contains("are"));
        assert!(tokens.contains("you"));
        // Single-char tokens should be excluded.
        assert!(!tokens.contains("a"));
    }

    #[test]
    fn tokenize_empty_string() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn recall_source_serializes_correctly() {
        let s = serde_json::to_string(&RecallSource::Session).expect("serialize");
        assert_eq!(s, "\"session\"");
        let g = serde_json::to_string(&RecallSource::Graph).expect("serialize");
        assert_eq!(g, "\"graph\"");
    }

    #[test]
    fn recall_source_trace_serializes_correctly() {
        let t = serde_json::to_string(&RecallSource::Trace).expect("serialize");
        assert_eq!(t, "\"trace\"");
    }

    #[test]
    fn test_normalize_scope_none_returns_auto() {
        let out = normalize_scope(None).expect("normalize");
        assert_eq!(out, vec![RecallScope::Auto]);
    }

    #[test]
    fn test_normalize_scope_string_passes_through() {
        for (s, expected) in [
            ("graph", RecallScope::Graph),
            ("session", RecallScope::Session),
            ("trace", RecallScope::Trace),
            ("auto", RecallScope::Auto),
        ] {
            let out = normalize_scope(Some(ScopeInput::from(s))).expect("normalize");
            assert_eq!(out, vec![expected], "scope={s}");
        }
    }

    #[test]
    fn test_normalize_scope_list_dedupes() {
        let out = normalize_scope(Some(ScopeInput::Many(vec![
            "session".to_string(),
            "graph".to_string(),
            "session".to_string(),
            "trace".to_string(),
            "graph".to_string(),
        ])))
        .expect("normalize");
        // Order preserved, duplicates dropped.
        assert_eq!(
            out,
            vec![RecallScope::Session, RecallScope::Graph, RecallScope::Trace,]
        );
    }

    #[test]
    fn test_normalize_scope_all_expands() {
        let all = vec![
            RecallScope::Graph,
            RecallScope::Session,
            RecallScope::Trace,
            RecallScope::SessionContext,
        ];
        let out = normalize_scope(Some(ScopeInput::from("all"))).expect("normalize");
        assert_eq!(out, all);
        // `"all"` mixed in with other values still expands to the canonical list.
        let out2 = normalize_scope(Some(ScopeInput::Many(vec![
            "session".to_string(),
            "all".to_string(),
        ])))
        .expect("normalize");
        assert_eq!(out2, all);
    }

    /// Port of `test_normalize_scope_accepts_session_context`'s alias
    /// asserts (Python `3401da38a`): `graph_context` now means `graph`, and
    /// `all` no longer carries it.
    #[test]
    fn test_normalize_scope_maps_deprecated_graph_context_to_graph() {
        let out = normalize_scope(Some(ScopeInput::from("graph_context"))).expect("normalize");
        assert_eq!(out, vec![RecallScope::Graph]);

        let out = normalize_scope(Some(ScopeInput::Many(vec![
            "graph_context".to_string(),
            "session_context".to_string(),
        ])))
        .expect("normalize");
        assert_eq!(out, vec![RecallScope::Graph, RecallScope::SessionContext]);

        // The alias and its replacement collapse into one source.
        let out = normalize_scope(Some(ScopeInput::Many(vec![
            "graph".to_string(),
            "graph_context".to_string(),
        ])))
        .expect("normalize");
        assert_eq!(out, vec![RecallScope::Graph]);

        // `"all"` with the alias still expands to the canonical list.
        let out = normalize_scope(Some(ScopeInput::Many(vec![
            "graph_context".to_string(),
            "all".to_string(),
        ])))
        .expect("normalize");
        assert_eq!(out, RecallScope::ALL.to_vec());
    }

    #[test]
    fn recall_scope_deserializes_deprecated_graph_context_as_graph() {
        let parsed: RecallScope = serde_json::from_str("\"graph_context\"").expect("deserialize");
        assert_eq!(parsed, RecallScope::Graph);
        assert_eq!(
            serde_json::to_string(&parsed).expect("serialize"),
            "\"graph\""
        );
    }

    #[test]
    fn test_normalize_scope_unknown_returns_error() {
        let err = normalize_scope(Some(ScopeInput::from("nonsense"))).expect_err("should error");
        match err {
            SearchError::InvalidInput(_) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn test_normalize_scope_error_message_matches_python() {
        let err = normalize_scope(Some(ScopeInput::from("foo"))).expect_err("should error");
        let msg = match err {
            SearchError::InvalidInput(m) => m,
            other => panic!("expected InvalidInput, got {other:?}"),
        };
        // Python: f'Unknown recall scope(s): {unknown}. Valid values: {sorted(_VALID_SCOPES)}'
        // -> `Unknown recall scope(s): ['foo']. Valid values: ['all', 'auto', 'code', 'graph', 'graph_context', 'session', 'session_context', 'session_first', 'tools', 'trace']`
        let expected = "Unknown recall scope(s): ['foo']. Valid values: ['all', 'auto', 'code', 'graph', 'graph_context', 'session', 'session_context', 'session_first', 'tools', 'trace']";
        assert_eq!(msg, expected);
    }

    #[test]
    fn unknown_scope_names_render_as_python_repr() {
        let err = normalize_scope(Some(ScopeInput::Many(vec![
            "graph".into(),
            "it's".into(),
            "a\"b'c".into(),
            "back\\slash\n".into(),
        ])))
        .expect_err("unknown names");
        let msg = err.to_string();
        // Python: repr(["it's", 'a"b\'c', 'back\\slash\n'])
        assert!(
            msg.contains(r#"Unknown recall scope(s): ["it's", 'a"b\'c', 'back\\slash\n']."#),
            "{msg}"
        );
    }

    #[test]
    fn recall_scope_all_constant_matches_canonical_order() {
        // Python `entries.py:242`.
        assert_eq!(
            RecallScope::ALL,
            &[
                RecallScope::Graph,
                RecallScope::Session,
                RecallScope::Trace,
                RecallScope::SessionContext,
            ]
        );
    }

    #[test]
    fn recall_scope_serde_round_trip() {
        for (s, expected) in [
            ("\"auto\"", RecallScope::Auto),
            ("\"graph\"", RecallScope::Graph),
            ("\"session\"", RecallScope::Session),
            ("\"session_first\"", RecallScope::SessionFirst),
            ("\"trace\"", RecallScope::Trace),
            ("\"session_context\"", RecallScope::SessionContext),
            ("\"tools\"", RecallScope::Tools),
            ("\"code\"", RecallScope::Code),
        ] {
            let parsed: RecallScope = serde_json::from_str(s).expect("deserialize");
            assert_eq!(parsed, expected);
            assert_eq!(serde_json::to_string(&expected).expect("serialize"), s);
        }
    }

    #[test]
    fn recall_scope_as_wire_matches_serde() {
        assert_eq!(RecallScope::Auto.as_wire(), "auto");
        assert_eq!(RecallScope::Graph.as_wire(), "graph");
        assert_eq!(RecallScope::Session.as_wire(), "session");
        assert_eq!(RecallScope::Trace.as_wire(), "trace");
        for scope in [
            RecallScope::SessionFirst,
            RecallScope::SessionContext,
            RecallScope::Tools,
            RecallScope::Code,
        ] {
            let serialized = serde_json::to_string(&scope).expect("serialize");
            assert_eq!(serialized, format!("\"{}\"", scope.as_wire()));
        }
    }

    /// Every name in Python's `_VALID_SCOPES` (`entries.py:193-204`)
    /// normalizes; each concrete one to its own scope.
    #[test]
    fn normalize_scope_accepts_every_python_scope_name() {
        for (wire, expected) in [
            ("auto", RecallScope::Auto),
            ("graph", RecallScope::Graph),
            ("session", RecallScope::Session),
            ("session_first", RecallScope::SessionFirst),
            ("trace", RecallScope::Trace),
            ("session_context", RecallScope::SessionContext),
            ("tools", RecallScope::Tools),
            ("code", RecallScope::Code),
        ] {
            let out = normalize_scope(Some(ScopeInput::from(wire))).expect("valid scope");
            assert_eq!(out, vec![expected], "{wire}");
        }
    }

    #[test]
    fn as_source_maps_lanes_and_leaves_sentinels_unmapped() {
        assert_eq!(RecallScope::Graph.as_source(), Some(RecallSource::Graph));
        assert_eq!(
            RecallScope::SessionContext.as_source(),
            Some(RecallSource::SessionContext)
        );
        for scope in [
            RecallScope::Auto,
            RecallScope::SessionFirst,
            RecallScope::Tools,
            RecallScope::Code,
        ] {
            assert_eq!(scope.as_source(), None, "{scope:?}");
        }
        assert_eq!(RecallSource::SessionContext.as_str(), "session_context");
    }

    /// Python `entries.py:238-249`: `all` keeps a co-supplied `tools` and
    /// `session_first` (in that order) and drops `code`.
    #[test]
    fn normalize_scope_all_carries_tools_and_session_first_but_not_code() {
        let many = |names: &[&str]| {
            normalize_scope(Some(ScopeInput::Many(
                names.iter().map(|s| (*s).to_string()).collect(),
            )))
            .expect("normalize")
        };
        let base = RecallScope::ALL.to_vec();

        let mut with_first = base.clone();
        with_first.push(RecallScope::SessionFirst);
        assert_eq!(many(&["session_first", "all"]), with_first);

        let mut with_both = base.clone();
        with_both.extend([RecallScope::Tools, RecallScope::SessionFirst]);
        assert_eq!(many(&["session_first", "all", "tools"]), with_both);

        assert_eq!(many(&["all", "code"]), base);
    }

    fn resolve(
        scope: &[RecallScope],
        session_id: Option<&str>,
        has_dataset_filter: bool,
        query_type: Option<SearchType>,
    ) -> ResolvedSources {
        resolve_sources(scope.to_vec(), session_id, has_dataset_filter, query_type)
            .expect("supported scope")
    }

    #[test]
    fn resolve_sources_auto_branches() {
        use RecallScope::{Auto, Graph, Session};
        let fallthrough = |sources: Vec<RecallScope>, auto_fallthrough| ResolvedSources {
            sources,
            auto_fallthrough,
        };
        // Session, no dataset filter, no query type: session first.
        assert_eq!(
            resolve(&[Auto], Some("s"), false, None),
            fallthrough(vec![Session, Graph], true)
        );
        // Session with a dataset filter: both contribute.
        assert_eq!(
            resolve(&[Auto], Some("s"), true, None),
            fallthrough(vec![Session, Graph], false)
        );
        // Session with a query type, with or without a dataset filter: graph
        // only (`elif session_id and query_type is None`).
        assert_eq!(
            resolve(&[Auto], Some("s"), false, Some(SearchType::Chunks)),
            fallthrough(vec![Graph], false)
        );
        assert_eq!(
            resolve(&[Auto], Some("s"), true, Some(SearchType::Chunks)),
            fallthrough(vec![Graph], false)
        );
        // An empty session id is no session id (Python truthiness).
        assert_eq!(
            resolve(&[Auto], Some(""), false, None),
            fallthrough(vec![Graph], false)
        );
        // No session: graph only.
        assert_eq!(
            resolve(&[Auto], None, false, None),
            fallthrough(vec![Graph], false)
        );
        // An explicit empty list runs nothing (Python `normalize_scope([])`
        // -> `[]`), even with a session id; a repeated `Auto` is auto.
        assert_eq!(resolve(&[], None, false, None), fallthrough(vec![], false));
        assert_eq!(
            resolve(&[], Some("s"), false, None),
            fallthrough(vec![], false)
        );
        assert_eq!(
            resolve(&[Auto, Auto], Some("s"), false, None),
            fallthrough(vec![Session, Graph], true)
        );
    }

    /// Python `recall.py:586-594`.
    #[test]
    fn resolve_sources_session_first_reorders_and_dedupes() {
        use RecallScope::{Graph, Session, SessionContext, SessionFirst, Trace};
        let expected = |sources: Vec<RecallScope>| ResolvedSources {
            sources,
            auto_fallthrough: true,
        };
        assert_eq!(
            resolve(&[SessionFirst], None, false, None),
            expected(vec![Session, Graph])
        );
        // Explicit session/graph next to it collapse into the leading pair,
        // and the rest keeps its order.
        assert_eq!(
            resolve(
                &[Trace, Graph, SessionFirst, Session],
                Some("s"),
                true,
                None
            ),
            expected(vec![Session, Graph, Trace])
        );
        // `all` + `session_first`.
        let mut all_first = RecallScope::ALL.to_vec();
        all_first.push(SessionFirst);
        assert_eq!(
            resolve(&all_first, Some("s"), false, Some(SearchType::Chunks)),
            expected(vec![Session, Graph, Trace, SessionContext])
        );
        // The query type and dataset filter do not matter for an explicit
        // scope, and without `session_first` nothing short-circuits.
        assert_eq!(
            resolve(&[Session, Graph], Some("s"), false, None),
            ResolvedSources {
                sources: vec![Session, Graph],
                auto_fallthrough: false,
            }
        );
        // A repeated explicit source runs once.
        assert_eq!(
            resolve(&[Trace, Graph, Trace], None, false, None).sources,
            vec![Trace, Graph]
        );
    }

    /// D1 (SDK-1040): `tools` and `code` are valid names refused before any
    /// lane runs, naming the first one.
    #[test]
    fn resolve_sources_refuses_tools_and_code() {
        for (scope, name) in [
            (vec![RecallScope::Tools], "tools"),
            (vec![RecallScope::Code], "code"),
            (
                vec![RecallScope::Graph, RecallScope::Code, RecallScope::Tools],
                "code",
            ),
            (vec![RecallScope::SessionFirst, RecallScope::Tools], "tools"),
        ] {
            let err = resolve_sources(scope, Some("s"), false, None).expect_err("refused");
            match err {
                SearchError::NotSupported(message) => assert_eq!(
                    message,
                    format!(
                        "Recall scope '{name}' is not supported yet in the Rust SDK \
                         (tracked in SDK-1537)."
                    )
                ),
                other => panic!("expected NotSupported, got {other:?}"),
            }
        }
        // The message is the error's whole display.
        let err =
            resolve_sources(vec![RecallScope::Tools], None, false, None).expect_err("refused");
        assert_eq!(
            err.to_string(),
            "Recall scope 'tools' is not supported yet in the Rust SDK (tracked in SDK-1537)."
        );
    }

    #[test]
    fn tools_trigger_parses_python_values() {
        use std::str::FromStr;
        assert_eq!(ToolsTrigger::default(), ToolsTrigger::Always);
        assert_eq!(
            ToolsTrigger::from_str("always").expect("valid"),
            ToolsTrigger::Always
        );
        assert_eq!(
            ToolsTrigger::from_str("on_empty").expect("valid"),
            ToolsTrigger::OnEmpty
        );
        for trigger in [ToolsTrigger::Always, ToolsTrigger::OnEmpty] {
            assert_eq!(
                ToolsTrigger::from_str(trigger.as_str()).expect("round trip"),
                trigger
            );
        }
    }

    #[test]
    fn tools_trigger_error_matches_python() {
        use std::str::FromStr;
        for bad in ["sometimes", "", "Always", "on-empty"] {
            match ToolsTrigger::from_str(bad) {
                Err(SearchError::InvalidInput(message)) => assert_eq!(
                    message,
                    format!("Invalid tools_trigger '{bad}'. Valid values: 'always', 'on_empty'.")
                ),
                other => panic!("expected InvalidInput for {bad:?}, got {other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // run_graph threads the caller identity into the dataset-name filter
    // -----------------------------------------------------------------------

    use crate::orchestration::SearchTypeRegistry;
    use crate::retrievers::SearchRetriever;
    use crate::types::{SearchContext, SearchOutput, SearchParams};
    use async_trait::async_trait;
    use cognee_database::ops as db_ops;
    use cognee_database::{AclDb, IngestDb, connect, initialize};
    use cognee_models::Dataset;
    use cognee_session::SessionContext;
    use std::sync::Arc;

    /// Records the `SearchParams` it was invoked with so a test can assert on
    /// what the orchestrator handed down after dataset-name resolution.
    struct RecordingRetriever {
        seen: std::sync::Mutex<Option<SearchParams>>,
    }

    #[async_trait]
    impl SearchRetriever for RecordingRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Chunks
        }

        fn dataset_scoping(&self) -> crate::retrievers::DatasetScoping {
            // Fixture items carry `dataset_id`.
            crate::retrievers::DatasetScoping::Membership
        }

        async fn get_context(
            &self,
            _query: &str,
            params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            *self.seen.lock().expect("test mutex") = Some(params.clone());
            Ok(vec![])
        }

        async fn get_completion(
            &self,
            _query: &str,
            _context: Option<SearchContext>,
            _session: &SessionContext,
            params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            *self.seen.lock().expect("test mutex") = Some(params.clone());
            if params.response_schema.is_some() {
                return Ok(SearchOutput::Structured(
                    serde_json::json!({ "answer": "structured" }),
                ));
            }
            Ok(SearchOutput::Text("graph-stub".to_string()))
        }
    }

    async fn orchestrator_with_seeded_dataset(
        name: &str,
        owner: Uuid,
    ) -> (SearchOrchestrator, Arc<RecordingRetriever>, Uuid) {
        let db = Arc::new(connect("sqlite::memory:").await.expect("in-memory sqlite"));
        initialize(&db).await.expect("schema init");
        let dataset = db_ops::datasets::create_dataset(
            &db,
            Dataset::new(name.to_string(), owner, None, Uuid::new_v4()),
        )
        .await
        .expect("seed dataset");

        let retriever = Arc::new(RecordingRetriever {
            seen: std::sync::Mutex::new(None),
        });
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::clone(&retriever) as Arc<dyn SearchRetriever>);
        let orchestrator =
            SearchOrchestrator::new(registry).with_dataset_resolver(db as Arc<dyn IngestDb>);
        (orchestrator, retriever, dataset.id)
    }

    /// Regression: `recall -d <name>` used to fail with "dataset name filter
    /// requires SearchRequest.user_id" because `run_graph` built the request
    /// with `user_id: None` even though every caller had the owner in hand.
    /// The owner must reach the orchestrator so the name resolves to the
    /// caller's dataset id.
    #[tokio::test]
    async fn run_graph_forwards_owner_so_dataset_names_resolve() {
        let owner = Uuid::new_v4();
        let (orchestrator, retriever, dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let owner_str = owner.to_string();
        let span = tracing::Span::none();

        let (_items, used, auto, _response) = plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            Some(vec!["notes".to_string()]),
            None,
            5,
            false,
            None,
            Some(owner_str.as_str()),
            &orchestrator,
            &span,
            None,
        )
        .await
        .expect("dataset name must resolve for its owner");

        assert_eq!(used, SearchType::Chunks);
        assert!(!auto);
        let seen = retriever
            .seen
            .lock()
            .expect("test mutex")
            .clone()
            .expect("retriever must have been invoked");
        assert_eq!(
            seen.dataset_ids.as_deref(),
            Some([dataset_id].as_slice()),
            "resolved dataset id must reach the retriever"
        );
    }

    /// Name resolution stays owner-scoped: another user's dataset of the same
    /// name is not visible, so the filter surfaces `DatasetNotFound` instead
    /// of silently widening.
    #[tokio::test]
    async fn run_graph_dataset_name_is_owner_scoped() {
        let owner = Uuid::new_v4();
        let stranger = Uuid::new_v4().to_string();
        let (orchestrator, _retriever, _dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let span = tracing::Span::none();

        let err = plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            Some(vec!["notes".to_string()]),
            None,
            5,
            false,
            None,
            Some(stranger.as_str()),
            &orchestrator,
            &span,
            None,
        )
        .await
        .expect_err("a stranger must not resolve another owner's dataset");
        assert!(
            matches!(err, SearchError::DatasetNotFound(_)),
            "expected DatasetNotFound, got {err:?}"
        );
    }

    /// A caller id that is not a UUID cannot identify an owner. That is fine
    /// when no name filter is requested (the session helpers accept arbitrary
    /// strings), but with one it must be reported as the caller's mistake
    /// rather than resurfacing as the misleading "requires user_id" error.
    #[tokio::test]
    async fn run_graph_rejects_non_uuid_user_only_when_names_are_filtered() {
        let owner = Uuid::new_v4();
        let (orchestrator, _retriever, _dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let span = tracing::Span::none();

        let err = plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            Some(vec!["notes".to_string()]),
            None,
            5,
            false,
            None,
            Some("user-1"),
            &orchestrator,
            &span,
            None,
        )
        .await
        .expect_err("non-UUID id with a name filter must be rejected");
        match err {
            SearchError::InvalidInput(msg) => {
                assert!(msg.contains("user-1"), "message must name the value: {msg}")
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }

        // Without a name filter the same id is tolerated, as before the fix.
        plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            None,
            None,
            5,
            false,
            None,
            Some("user-1"),
            &orchestrator,
            &span,
            None,
        )
        .await
        .expect("no name filter -> non-UUID id is simply not forwarded");
    }

    /// `POST /v1/recall` deserialized `dataset_ids` and then dropped it on
    /// the floor: `run_graph` had no parameter for it and hard-coded
    /// `dataset_ids: None`, so an id filter returned unfiltered results with
    /// a 200. The id must reach the retriever.
    #[tokio::test]
    async fn run_graph_forwards_dataset_ids_to_the_retriever() {
        let owner = Uuid::new_v4();
        let (orchestrator, retriever, dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let owner_str = owner.to_string();
        let span = tracing::Span::none();

        plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            None,
            Some(vec![dataset_id]),
            5,
            false,
            None,
            Some(owner_str.as_str()),
            &orchestrator,
            &span,
            None,
        )
        .await
        .expect("an owned dataset id must be accepted");

        let seen = retriever
            .seen
            .lock()
            .expect("test mutex")
            .clone()
            .expect("retriever must have been invoked");
        assert_eq!(
            seen.dataset_ids.as_deref(),
            Some([dataset_id].as_slice()),
            "the explicit dataset id must reach the retriever"
        );
    }

    #[tokio::test]
    async fn run_graph_forwards_retriever_specific_config() {
        let owner = Uuid::new_v4();
        let (orchestrator, retriever, dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let owner_str = owner.to_string();
        let span = tracing::Span::none();
        let options = RecallOptions {
            retriever_specific_config: Some(std::collections::HashMap::from([
                ("include_global_context_index".into(), true.into()),
                ("global_context_index_top_k".into(), 2.into()),
            ])),
            ..Default::default()
        };

        plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            None,
            Some(vec![dataset_id]),
            5,
            false,
            None,
            Some(owner_str.as_str()),
            &orchestrator,
            &span,
            Some(&options),
        )
        .await
        .expect("retriever config must be accepted");

        let seen = retriever
            .seen
            .lock()
            .expect("test mutex")
            .clone()
            .expect("retriever must have been invoked");
        assert_eq!(seen.include_global_context_index, Some(true));
        assert_eq!(seen.global_context_index_top_k, Some(2));
    }

    #[tokio::test]
    async fn run_graph_forwards_response_schema() {
        let owner = Uuid::new_v4();
        let (orchestrator, retriever, dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let owner_str = owner.to_string();
        let span = tracing::Span::none();
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } }
        });
        let options = RecallOptions {
            response_schema: Some(schema.clone()),
            ..Default::default()
        };

        let (items, ..) = plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            None,
            Some(vec![dataset_id]),
            5,
            false,
            None,
            Some(owner_str.as_str()),
            &orchestrator,
            &span,
            Some(&options),
        )
        .await
        .expect("a valid schema must be accepted");

        let seen = retriever
            .seen
            .lock()
            .expect("test mutex")
            .clone()
            .expect("retriever must have been invoked");
        assert_eq!(seen.response_schema, Some(schema));
        // The structured answer is the item's content, not its tagged form.
        let contents: Vec<_> = items.iter().map(|item| item.content.clone()).collect();
        assert_eq!(
            contents,
            vec![serde_json::json!({ "answer": "structured" })]
        );
    }

    /// Ids are authorized against the caller: another user's dataset id is
    /// refused with `PermissionDenied` (Python `PermissionDeniedError`),
    /// never silently searched.
    #[tokio::test]
    async fn run_graph_dataset_ids_are_owner_checked() {
        let owner = Uuid::new_v4();
        let stranger = Uuid::new_v4().to_string();
        let (orchestrator, retriever, dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let span = tracing::Span::none();

        let err = plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            None,
            Some(vec![dataset_id]),
            5,
            false,
            None,
            Some(stranger.as_str()),
            &orchestrator,
            &span,
            None,
        )
        .await
        .expect_err("a stranger must not read another owner's dataset by id");
        assert!(
            matches!(err, SearchError::PermissionDenied(_)),
            "expected PermissionDenied, got {err:?}"
        );
        assert!(
            retriever.seen.lock().expect("test mutex").is_none(),
            "retriever must not run for a denied id"
        );
    }

    /// SDK-687, recall side: `/v1/recall` reaches the orchestrator through
    /// `run_graph`, so under per-dataset storage an unfiltered recall must be
    /// scoped to the caller's readable datasets like an unfiltered search, and
    /// an unreadable dataset must never reach the retriever's scope.
    #[tokio::test]
    async fn run_graph_unfiltered_with_acl_is_scoped_to_readable_datasets() {
        let owner = Uuid::new_v4();
        let (orchestrator, retriever, dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let acl = Arc::new(cognee_test_utils::MockAclDb::new());
        acl.grant_permission(owner, dataset_id, "read")
            .await
            .expect("grant");
        // A dataset someone else may read, which the caller may not.
        acl.grant_permission(Uuid::new_v4(), Uuid::new_v4(), "read")
            .await
            .expect("grant");
        let orchestrator = orchestrator
            .with_acl_db(acl as Arc<dyn AclDb>)
            .with_per_dataset_storage(true);
        let owner_str = owner.to_string();
        let span = tracing::Span::none();

        plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            None,
            None,
            5,
            false,
            None,
            Some(owner_str.as_str()),
            &orchestrator,
            &span,
            None,
        )
        .await
        .expect("an unfiltered recall with a readable dataset must succeed");

        let seen = retriever
            .seen
            .lock()
            .expect("test mutex")
            .clone()
            .expect("retriever must have been invoked");
        assert_eq!(
            seen.dataset_ids.as_deref(),
            Some([dataset_id].as_slice()),
            "the scope must be exactly the caller's readable set"
        );
    }

    /// Under per-dataset storage, an unfiltered recall by a caller who may read
    /// nothing returns no graph items and never runs the retriever. An unknown
    /// caller (no id, or one that is not a UUID) gets the same empty graph
    /// result, not an error, so recall's other sources and its auto-mode
    /// fall-through keep working.
    #[tokio::test]
    async fn run_graph_unfiltered_with_acl_and_nothing_readable_is_empty() {
        let owner = Uuid::new_v4();
        let (orchestrator, retriever, _dataset_id) =
            orchestrator_with_seeded_dataset("notes", owner).await;
        let orchestrator = orchestrator
            .with_acl_db(Arc::new(cognee_test_utils::MockAclDb::new()) as Arc<dyn AclDb>)
            .with_per_dataset_storage(true);
        let owner_str = owner.to_string();
        let span = tracing::Span::none();

        let (items, _used, _auto, _response) = plan_and_run(
            "anything",
            Some(SearchType::Chunks),
            None,
            None,
            5,
            false,
            None,
            Some(owner_str.as_str()),
            &orchestrator,
            &span,
            None,
        )
        .await
        .expect("nothing readable is an empty result, not an error");
        assert!(items.is_empty(), "got {items:?}");
        assert!(
            retriever.seen.lock().expect("test mutex").is_none(),
            "retriever must not run when the caller may read no dataset"
        );

        for caller in [None, Some("user-1")] {
            let (items, _used, _auto, _response) = plan_and_run(
                "anything",
                Some(SearchType::Chunks),
                None,
                None,
                5,
                false,
                None,
                caller,
                &orchestrator,
                &span,
                None,
            )
            .await
            .unwrap_or_else(|e| panic!("unknown caller {caller:?}: {e:?}"));
            assert!(items.is_empty(), "caller {caller:?}: {items:?}");
        }
        assert!(retriever.seen.lock().expect("test mutex").is_none());
    }

    // -----------------------------------------------------------------------
    // search_session_context (Python `_fetch_session_context`)
    // -----------------------------------------------------------------------

    fn context_manager(dir: &tempfile::TempDir) -> SessionManager {
        SessionManager::new(Arc::new(cognee_session::FsSessionStore::new(dir.path())))
    }

    async fn seed_context_entry(
        manager: &SessionManager,
        profile: &str,
        section: &str,
        content: &str,
        id: &str,
    ) {
        let entry = cognee_session::SessionContextEntry {
            id: id.to_string(),
            section: section.to_string(),
            context_profile: profile.to_string(),
            content: content.to_string(),
            normalized_content: cognee_session::normalize_content(content),
            confidence: 0.9,
            created_at: "2026-06-10T10:00:01".to_string(),
            source_feedback_ids: Vec::new(),
            source_trace_ids: Vec::new(),
            helpful_count: 0,
            harmful_count: 0,
            priority: 0,
            last_served_at: None,
            embedding: None,
            kind: cognee_session::CONTEXT_ENTRY_KIND.to_string(),
        };
        manager
            .create_session_context_entry(Some("s"), Some("u"), entry.to_row().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn session_context_is_empty_without_a_session_user_or_manager() {
        let dir = tempfile::tempdir().unwrap();
        let manager = context_manager(&dir);
        seed_context_entry(&manager, "qa", "rules", "be concise", "q1").await;
        let sm = Some(&manager);
        for (session_id, user_id, sm) in [
            (None, Some("u"), sm),
            (Some(""), Some("u"), sm),
            (Some("s"), None, sm),
            (Some("s"), Some(""), sm),
            (Some("s"), Some("u"), None),
        ] {
            let items = search_session_context("q", session_id, user_id, "qa", sm)
                .await
                .unwrap();
            assert!(items.is_empty(), "{session_id:?} {user_id:?}: {items:?}");
        }
    }

    #[tokio::test]
    async fn session_context_is_empty_when_nothing_renders() {
        let dir = tempfile::tempdir().unwrap();
        let manager = context_manager(&dir);
        let none = search_session_context("q", Some("s"), Some("u"), "qa", Some(&manager))
            .await
            .unwrap();
        assert!(none.is_empty());
        // Entries of another profile render nothing for this one.
        seed_context_entry(&manager, "agent", "tool_rules", "use uv run", "a1").await;
        let none = search_session_context("q", Some("s"), Some("u"), "qa", Some(&manager))
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn session_context_returns_one_item_with_the_block_and_profile() {
        let dir = tempfile::tempdir().unwrap();
        let manager = context_manager(&dir);
        seed_context_entry(&manager, "qa", "rules", "be concise", "q1").await;
        seed_context_entry(&manager, "agent", "tool_rules", "use uv run", "a1").await;

        let items = search_session_context("q", Some("s"), Some("u"), "agent", Some(&manager))
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.source, RecallSource::SessionContext);
        assert_eq!(item.score, 1.0);
        assert_eq!(item.content["context_profile"], "agent");
        let block = item.content["content"].as_str().expect("block is a string");
        assert!(block.starts_with("## Active session guidance\n"), "{block}");
        assert!(
            block.contains("### Tool rules\n- [10:00:01] use uv run"),
            "{block}"
        );
        assert!(!block.contains("be concise"), "{block}");
        assert_eq!(
            item.content.as_object().map(|o| o.len()),
            Some(2),
            "only content and context_profile: {}",
            item.content
        );
        // Read-only: no `last_served_at` stamp.
        for row in manager
            .get_session_context_entries(Some("s"), Some("u"))
            .await
            .unwrap()
        {
            assert!(row["last_served_at"].is_null(), "{row:?}");
        }
    }

    #[test]
    fn context_profile_defaults_to_qa() {
        assert_eq!(RecallOptions::context_profile_or_default(None), "qa");
        let options = RecallOptions::default();
        assert_eq!(
            RecallOptions::context_profile_or_default(Some(&options)),
            "qa"
        );
        let options = RecallOptions {
            context_profile: Some("agent".to_string()),
            ..Default::default()
        };
        assert_eq!(
            RecallOptions::context_profile_or_default(Some(&options)),
            "agent"
        );
    }
}
