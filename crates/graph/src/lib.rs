//! Graph database abstraction layer for Cognee.
//!
//! This crate provides a trait-based interface for graph database operations,
//! enabling pluggable graph database backends for knowledge graph storage.
//!
//! # Architecture
//!
//! - `GraphDBTrait` - Async trait defining graph database operations
//! - `LadybugAdapter` - Implementation using Ladybug (lbug) embedded graph database
//! - `GraphNode` / `GraphEdge` - Type aliases for node and edge data
//!
//! # Example
//!
//! ```ignore
//! use cognee_graph::{GraphDBTrait, LadybugAdapter};
//! use cognee_models::Entity;
//!
//! let db = LadybugAdapter::new("./graph_data").await?;
//! db.initialize().await?;
//!
//! let entity = Entity::new("Alice", EntityType::new("Person", None), Some("user-1"));
//! db.add_node(&entity).await?;
//! ```

mod dataset_routed;
#[cfg(any(feature = "ladybug", feature = "postgres"))]
mod dedup;
mod error;
mod formatted;
mod temporal;
mod traits;
mod types;

#[cfg(feature = "ladybug")]
mod ladybug;

#[cfg(feature = "postgres")]
mod pg_graph_adapter;

#[cfg(any(test, feature = "testing"))]
pub mod mock;

pub use dataset_routed::{DatasetGraphResolver, DatasetRoutedGraphDB, NO_DATASET_SCOPE};
pub use error::{GraphDBError, GraphDBResult};
pub use formatted::get_formatted_graph_data;
pub use temporal::{
    LEGACY_TIMESTAMP_SPAN_MS, TemporalAnchors, WindowTimestamp, default_temporal_anchors,
    default_timestamps_in_range, timestamp_period,
};
pub use traits::{
    EdgeKey, GraphDBTrait, GraphDBTraitExt, NODE_LABEL_KEYS, NeighborhoodScope, NodeTruthState,
    apply_neighborhood_scope, extract_truth_alignment, extract_truth_epoch, node_label_contains,
};
pub use types::{EdgeData, GraphEdge, GraphNode, NodeData, parse_audit_timestamp};

#[cfg(feature = "ladybug")]
pub use ladybug::{DEFAULT_MAX_DB_SIZE, LadybugAdapter, LadybugConfig};

#[cfg(feature = "postgres")]
pub use pg_graph_adapter::PgGraphAdapter;

#[cfg(any(test, feature = "testing"))]
pub use mock::MockGraphDB;
