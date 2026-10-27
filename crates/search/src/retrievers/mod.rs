mod advanced_graph_retrievers;
mod base_retriever;
mod chunks_retriever;
mod completion_retriever;
mod context_items;
mod cypher_nl_retrievers;
mod decomposition_retriever;
mod graph_completion_retriever;
mod graph_core;
mod graph_report_retriever;
mod hybrid;
mod lexical_retriever;
mod lucky_feedback_rules_retrievers;
mod summaries_retriever;
mod temporal_hybrid;
mod triplet_retriever;

pub use advanced_graph_retrievers::{
    GraphCompletionContextExtensionRetriever, GraphCompletionCotRetriever,
    GraphSummaryCompletionRetriever,
};
#[cfg(test)]
pub(crate) use base_retriever::AppliedTuning;
pub use base_retriever::{DatasetScoping, SearchRetriever, SearchRetrieverRef};
#[allow(
    unused_imports,
    reason = "internal API for the later hybrid-assembly task (P1-07/P1-09)"
)]
pub use chunks_retriever::ChunksRetriever;
pub use completion_retriever::CompletionRetriever;
pub use cypher_nl_retrievers::{CypherSearchRetriever, NaturalLanguageRetriever};
pub use decomposition_retriever::{
    DECOMPOSITION_PAYLOAD_KEY, DecompositionMode, GraphCompletionDecompositionRetriever,
};
pub use graph_completion_retriever::GraphCompletionRetriever;
pub use graph_report_retriever::{
    DEFAULT_GRAPH_REPORT_TOP_N, EMPTY_GRAPH_REPORT, GraphReportRetriever, REPORT_PAYLOAD_KEY,
};
pub(crate) use hybrid::extract_used_ids;
pub use hybrid::{HybridBudget, HybridRetriever};
pub use lexical_retriever::{BM25_DEFAULT_B, BM25_DEFAULT_K1, LexicalRetriever, LexicalScorer};
pub use lucky_feedback_rules_retrievers::{
    CodingRulesRetriever, FeedbackRetriever, FeelingLuckyRetriever,
};
pub use summaries_retriever::SummariesRetriever;
pub use temporal_hybrid::{
    TemporalDiagnostics, TemporalFallbackReason, TemporalHybridRetriever, TemporalRetrieval,
};
pub use triplet_retriever::TripletRetriever;
