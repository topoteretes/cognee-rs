//! Graph integration module.
//!
//! This module provides functionality to integrate and deduplicate knowledge graphs
//! from multiple text chunks. It mirrors the Python cognee architecture:
//! - Graph expansion (LLM layer → Storage layer conversion)
//! - Deduplication (in-memory and database-backed)
//! - Node/Edge pair types for storage

pub mod db_deduplication;
pub mod deduplication;
pub mod expansion;
pub mod types;

pub use db_deduplication::retrieve_existing_edges;
pub use deduplication::{DeduplicationResult, deduplicate_nodes_and_edges};
pub use expansion::{
    EdgeResolutionStats, expand_with_nodes_and_edges, expand_with_nodes_and_edges_with_stats,
};
pub use types::{ArtifactProducers, GraphEdgePair, GraphNodePair};
