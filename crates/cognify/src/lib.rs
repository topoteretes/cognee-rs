pub mod config;
pub mod error;
pub mod fact_extraction;
pub mod graph_integration;
pub mod pipeline;
pub mod summarization;
pub mod triplet_creation; // NEW - Phase 3

pub use config::{ChunkStrategy, CognifyConfig, ConfigError};
pub use error::CognifyError;
pub use fact_extraction::{Edge, FactExtractor, KnowledgeGraph, Node};
pub use graph_integration::{
    DeduplicationResult, GraphEdgePair, GraphNodePair, deduplicate_nodes_and_edges,
    expand_with_nodes_and_edges,
};
pub use pipeline::{CognifyPipeline, CognifyResult, IndexedFieldsStats};
pub use summarization::{SummarizedContent, SummaryExtractor, TextSummary};
pub use triplet_creation::create_triplets_from_graph; // NEW - Phase 3
