//! SeaORM entity for `provenance_edge_evidence`: which chunk of which
//! document supports a graph edge.
//!
//! Port of Python's `ProvenanceEdgeEvidence`
//! (`cognee/modules/provenance/edge_evidence/models.py`, table created by
//! alembic revision `f3a7b9c1d2e4`). One row links a graph edge's
//! deterministic `edge_object_id` to the `DocumentChunk`, `Data` row and
//! pipeline run that produced it. The table is append-only and has no foreign
//! keys, so graph-native deletion never depends on it; its rows are swept by
//! [`crate::ops::edge_evidence::delete_edge_evidence`].

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "provenance_edge_evidence")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub tenant_id: Option<String>,
    pub user_id: String,
    pub dataset_id: String,
    pub data_id: String,
    pub pipeline_run_id: Option<String>,
    pub chunk_id: String,
    pub chunk_index: Option<i32>,
    /// The graph's deterministic `edge_object_id`.
    pub edge_id: String,
    pub source_node_id: String,
    pub destination_node_id: String,
    #[sea_orm(column_type = "Text")]
    pub relationship_name: String,
    /// `extracted` (the chunk's LLM extraction produced the edge) or
    /// `structural` (a chunk-rooted edge such as `contains`).
    pub evidence_kind: String,
    pub source_task: Option<String>,
    pub confidence: Option<f64>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
