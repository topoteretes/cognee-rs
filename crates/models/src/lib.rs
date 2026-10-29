//! Core data types shared across the cognee-rust crates (Data, Dataset, Document, DocumentChunk, Entity, KnowledgeGraph, and more).

mod backend_overrides;
mod data;
mod data_input;
mod data_point;
mod dataset;
/// The dataset an operation runs in, for per-dataset storage.
pub mod dataset_scope;
mod document;
mod document_chunk;
mod edge_metadata;
mod edge_type;
mod embedding;
mod entity;
mod entity_type;
mod global_context_summary;
pub mod has_datapoint;
mod internal_node;
/// Memory entry types for typed `remember()` dispatch.
pub mod memory;
/// Permission helpers.
pub mod permission;
/// Temporal event and timestamp types for the cognify pipeline.
pub mod temporal_event;
mod timestamp;
mod triplet;
mod user;
mod user_preference;

pub use backend_overrides::{BackendConfig, BackendOverrides};
pub use data::Data;
pub use data_input::DataInput;
pub use data_point::DataPoint;
pub use dataset::Dataset;
pub use document::{Document, classify_documents, doc_type_for_extension};
pub use document_chunk::DocumentChunk;
pub use edge_metadata::EdgeMetadata;
pub use edge_type::EdgeType;
pub use embedding::Embedding;
pub use entity::Entity;
pub use entity_type::EntityType;
pub use global_context_summary::GlobalContextSummary;
pub use has_datapoint::HasDataPoint;
pub use internal_node::{INTERNAL_PROPERTY, is_internal_node, marks_internal};
pub use memory::{FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};
pub use permission::permissions;
pub use temporal_event::*;
pub use timestamp::{
    TimePeriod, Timestamp, TimestampPrecision, month_from_word, normalize_absolute_date,
    parse_time_period, timestamp_overlaps,
};
pub use triplet::Triplet;
pub use user::User;
pub use user_preference::UserPreference;

// `Role` and `Tenant` are not part of this crate; a downstream ACL
// implementation owns them.
