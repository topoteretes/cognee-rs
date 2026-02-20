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

mod error;
mod ladybug;
mod traits;
mod types;

#[cfg(any(test, feature = "testing"))]
pub mod mock;

pub use error::{GraphDBError, GraphDBResult};
pub use ladybug::LadybugAdapter;
pub use traits::GraphDBTrait;
pub use types::{EdgeData, GraphEdge, GraphNode, NodeData};

#[cfg(any(test, feature = "testing"))]
pub use mock::MockGraphDB;
