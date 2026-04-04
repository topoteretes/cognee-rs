//! Cognify pipeline result types.
//!
//! The actual pipeline orchestration lives in [`crate::tasks`]. This module
//! defines the output types shared across the pipeline.

use cognee_models::{DocumentChunk, Embedding};

use crate::graph_integration::{GraphEdgePair, GraphNodePair};
use crate::summarization::TextSummary;

/// Result of the cognify pipeline.
#[derive(Debug, Clone)]
pub struct CognifyResult {
    /// Text chunks extracted from documents
    pub chunks: Vec<DocumentChunk>,

    /// Entities (nodes) with their types, deduplicated
    pub entities: Vec<GraphNodePair>,

    /// Edges (relationships) between entities, deduplicated
    pub edges: Vec<GraphEdgePair>,

    /// Text summaries generated from chunks
    pub summaries: Vec<TextSummary>,

    /// Embeddings for chunks, entities, and summaries
    pub embeddings: Vec<Embedding>,

    /// Statistics about indexed fields
    pub indexed_fields: IndexedFieldsStats,
}

impl CognifyResult {
    /// Create an empty result (no data to process).
    pub fn empty() -> Self {
        Self {
            chunks: vec![],
            entities: vec![],
            edges: vec![],
            summaries: vec![],
            embeddings: vec![],
            indexed_fields: IndexedFieldsStats::default(),
        }
    }
}

/// Statistics about indexed fields.
///
/// Tracks how many data points were indexed for each field type.
/// Useful for verifying indexing completeness and debugging.
#[derive(Debug, Clone, Default)]
pub struct IndexedFieldsStats {
    /// Number of DocumentChunk.text fields indexed
    pub chunk_text_count: usize,

    /// Number of Entity.name fields indexed
    pub entity_name_count: usize,

    /// Number of Entity.description fields indexed
    pub entity_description_count: usize,

    /// Number of TextSummary.text fields indexed
    pub summary_text_count: usize,

    /// Number of triplets indexed
    pub triplet_count: usize,
}
