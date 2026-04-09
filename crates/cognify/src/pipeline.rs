//! Cognify pipeline result types.
//!
//! The actual pipeline orchestration lives in [`crate::tasks`]. This module
//! defines the output types shared across the pipeline.

use std::collections::HashMap;

use cognee_models::{DocumentChunk, EdgeType, Embedding};

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

    /// Edge types aggregated from relationship names
    pub edge_types: Vec<EdgeType>,

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
            edge_types: vec![],
            embeddings: vec![],
            indexed_fields: IndexedFieldsStats::default(),
        }
    }
}

/// Statistics about indexed fields.
///
/// Tracks how many data points were indexed for each field type.
/// Uses dynamic `{TypeName}_{field_name}` keys matching the Python SDK's
/// `metadata["index_fields"]`-driven approach, plus legacy convenience accessors.
#[derive(Debug, Clone, Default)]
pub struct IndexedFieldsStats {
    /// Dynamic per-collection counts keyed by `"{TypeName}_{field_name}"`.
    ///
    /// E.g. `"DocumentChunk_text" -> 42`, `"Entity_name" -> 7`.
    pub field_counts: HashMap<String, usize>,

    /// Number of triplets indexed (triplets are not standard DataPoints,
    /// so they are tracked separately).
    pub triplet_count: usize,
}

impl IndexedFieldsStats {
    /// Record that `count` items were indexed for `collection`.
    pub fn record(&mut self, data_type: &str, field_name: &str, count: usize) {
        let key = format!("{}_{}", data_type, field_name);
        *self.field_counts.entry(key).or_insert(0) += count;
    }

    /// Get count for a specific `{type}_{field}` collection, or 0 if absent.
    pub fn get(&self, data_type: &str, field_name: &str) -> usize {
        let key = format!("{}_{}", data_type, field_name);
        self.field_counts.get(&key).copied().unwrap_or(0)
    }

    // -- Convenience accessors (backward-compatible with old named fields) --

    /// Number of DocumentChunk.text fields indexed.
    pub fn chunk_text_count(&self) -> usize {
        self.get("DocumentChunk", "text")
    }

    /// Number of Entity.name fields indexed.
    pub fn entity_name_count(&self) -> usize {
        self.get("Entity", "name")
    }

    /// Number of EntityType.name fields indexed.
    pub fn entity_type_name_count(&self) -> usize {
        self.get("EntityType", "name")
    }

    /// Number of TextSummary.text fields indexed.
    pub fn summary_text_count(&self) -> usize {
        self.get("TextSummary", "text")
    }

    /// Number of EdgeType.relationship_name fields indexed.
    pub fn edge_type_count(&self) -> usize {
        self.get("EdgeType", "relationship_name")
    }
}
