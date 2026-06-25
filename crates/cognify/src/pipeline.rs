//! Cognify pipeline result types.
//!
//! The actual pipeline orchestration lives in [`crate::tasks`]. This module
//! defines the output types shared across the pipeline.

use std::collections::HashMap;

use cognee_models::{Document, DocumentChunk, EdgeType, Embedding};
use uuid::Uuid;

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

    /// Documents needed by the post-pipeline
    /// [`crate::tasks::extract_dlt_fk_edges`] teardown step. Populated by the
    /// final task in [`crate::tasks::build_cognify_pipeline`]; empty in the
    /// temporal branch (which does not run DLT FK extraction). The matching
    /// chunk list reuses the existing [`Self::chunks`] field.
    ///
    /// Not serialised — internal teardown carrier, not part of the public
    /// result shape.
    pub documents_for_dlt: Vec<Document>,

    /// `true` when this result was synthesised by the
    /// `check_pipeline_run_qualification` short-circuit (latest
    /// `pipeline_runs` row was `COMPLETED`). All other fields are empty.
    ///
    /// CLI prints "already complete" when set; HTTP-server returns
    /// `200 OK` with `status = "PipelineRunAlreadyCompleted"`. See doc 08-08
    /// §4.3 and locked decision 13.
    pub already_completed: bool,

    /// The `pipeline_run_id` of the prior completed run that triggered the
    /// short-circuit. `None` on normal (non-short-circuit) results.
    pub prior_pipeline_run_id: Option<Uuid>,
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
            documents_for_dlt: vec![],
            already_completed: false,
            prior_pipeline_run_id: None,
        }
    }

    /// Create a short-circuit "already completed" result tagged with the
    /// prior `pipeline_run_id`. All payload vectors are empty — callers that
    /// need the prior run's outputs should query the graph / vector store
    /// directly (matches Python parity). See doc 08-08 §4.3.
    pub fn already_completed(pipeline_run_id: Uuid) -> Self {
        Self {
            already_completed: true,
            prior_pipeline_run_id: Some(pipeline_run_id),
            ..Self::empty()
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
        let key = format!("{data_type}_{field_name}");
        *self.field_counts.entry(key).or_insert(0) += count;
    }

    /// Get count for a specific `{type}_{field}` collection, or 0 if absent.
    pub fn get(&self, data_type: &str, field_name: &str) -> usize {
        let key = format!("{data_type}_{field_name}");
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
