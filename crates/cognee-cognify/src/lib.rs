pub mod error;
pub mod fact_extraction;
pub mod graph_integration;
pub mod pipeline;

pub use error::CognifyError;
pub use fact_extraction::{Edge, FactExtractor, KnowledgeGraph, Node};
pub use graph_integration::{
    DeduplicationResult, GraphEdgePair, GraphNodePair, deduplicate_nodes_and_edges,
    expand_with_nodes_and_edges,
};
pub use pipeline::{CognifyPipeline, CognifyResult};
