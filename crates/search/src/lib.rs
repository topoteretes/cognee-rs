//! Unified search orchestration across 17 retrieval strategies (GraphCompletion, RagCompletion, Chunks, and more).

/// Graph retrieval strategies and helpers.
pub mod graph_retrieval;
/// Graph-readiness probe for recall's warm-up short-circuit.
pub mod graph_warmup;
/// Observability hooks for search pipeline tracing.
pub mod observability;
/// Search orchestration and pipeline assembly.
pub mod orchestration;
/// Query router that maps natural-language queries to `SearchType`s.
pub mod query_router;
/// Normalization of retriever output into recall's typed result shape.
pub mod recall_normalize;
/// `RecallScope` and related types for scoping search results.
pub mod recall_scope;
/// A caller's `response_schema` for structured completions.
pub mod response_schema;
/// Individual retriever implementations for each `SearchType`.
pub mod retrievers;
#[cfg(test)]
mod test_support;
/// Core search types: `SearchType`, `SearchParams`, `SearchResult`, etc.
pub mod types;
/// Per-user `prefers` weights and preference text: the store `improve()`
/// writes and the read the retrievers personalize with.
pub mod user_preferences;
/// Shared search utilities (completion helpers, etc.).
pub mod utils;

#[allow(deprecated)]
pub use cognee_session::SeaOrmSessionStore;
pub use cognee_session::{RelationalSessionStore, SessionContext, SessionManager, SessionStore};
pub use orchestration::{
    DatasetSearchResult, SearchBuilder, SearchOrchestrator, SearchTuning, SearchTypeRegistry,
    SessionSearchMode, merge_dataset_results,
};
pub use query_router::{
    ROUTER_FALLBACK_TYPE, RouteDecision, RoutingConstraints, constrain, default_route,
    pinned_route, route_query,
};
pub use recall_normalize::{
    NormalizedResult, RecallResultKind, build_graph_entry, build_structured_entry,
    entry_dataset_ids, entry_evidence, item_dataset_id, only_context_system_prompt,
};
pub use recall_scope::{
    RecallItem, RecallScope, RecallSource, ResolvedSources, ScopeInput, ToolsTrigger,
    normalize_scope, resolve_sources,
};
pub use retrievers::{
    ChunksRetriever, CodingRulesRetriever, CompletionRetriever, CypherSearchRetriever,
    DatasetScoping, FeedbackRetriever, FeelingLuckyRetriever,
    GraphCompletionContextExtensionRetriever, GraphCompletionCotRetriever,
    GraphCompletionRetriever, GraphSummaryCompletionRetriever, HybridBudget, HybridRetriever,
    LexicalRetriever, LexicalScorer, NaturalLanguageRetriever, SearchRetriever, SearchRetrieverRef,
    SummariesRetriever, TemporalDiagnostics, TemporalFallbackReason, TemporalHybridRetriever,
    TemporalRetrieval,
};
pub use types::{
    EvidenceKind, EvidenceReference, EvidenceRole, Rule, SearchContext, SearchError, SearchGraph,
    SearchGraphEdge, SearchGraphNode, SearchItem, SearchOutput, SearchParams, SearchRequest,
    SearchResponse, SearchType,
};
pub use user_preferences::{ActivePreferences, PreferenceConfig, load_active_preferences};
