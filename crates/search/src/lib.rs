//! Unified search orchestration across 16 retrieval strategies (GraphCompletion, RagCompletion, Chunks, and more).

/// Graph retrieval strategies and helpers.
pub mod graph_retrieval;
/// Observability hooks for search pipeline tracing.
pub mod observability;
/// Search orchestration and pipeline assembly.
pub mod orchestration;
/// Query router that maps natural-language queries to `SearchType`s.
pub mod query_router;
/// Query router override-count statistics.
pub mod query_router_stats;
/// `RecallScope` and related types for scoping search results.
pub mod recall_scope;
/// Individual retriever implementations for each `SearchType`.
pub mod retrievers;
#[cfg(test)]
mod test_support;
/// Core search types: `SearchType`, `SearchParams`, `SearchResult`, etc.
pub mod types;
/// Shared search utilities (completion helpers, etc.).
pub mod utils;

#[allow(deprecated)]
pub use cognee_session::SeaOrmSessionStore;
pub use cognee_session::{RelationalSessionStore, SessionContext, SessionManager, SessionStore};
pub use orchestration::{
    DatasetSearchResult, SearchBuilder, SearchOrchestrator, SearchTuning, SearchTypeRegistry,
    merge_dataset_results,
};
pub use query_router::{RouteResult, route_query};
pub use query_router_stats::{clear_override_counts, override_counts_snapshot, record_override};
pub use recall_scope::{RecallItem, RecallScope, RecallSource, ScopeInput, normalize_scope};
pub use retrievers::{
    ChunksRetriever, CodingRulesRetriever, CompletionRetriever, CypherSearchRetriever,
    DatasetScoping, FeedbackRetriever, FeelingLuckyRetriever,
    GraphCompletionContextExtensionRetriever, GraphCompletionCotRetriever,
    GraphCompletionRetriever, GraphSummaryCompletionRetriever, HybridBudget, HybridRetriever,
    LexicalRetriever, LexicalScorer, NaturalLanguageRetriever, SearchRetriever, SearchRetrieverRef,
    SummariesRetriever, TemporalDiagnostics, TemporalFallbackReason, TemporalHybridRetriever,
    TemporalRetrieval, TripletRetriever,
};
pub use types::{
    FeedbackDetectionResult, Rule, SearchContext, SearchError, SearchGraph, SearchGraphEdge,
    SearchGraphNode, SearchItem, SearchOutput, SearchParams, SearchRequest, SearchResponse,
    SearchType,
};
