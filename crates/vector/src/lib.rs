//! Vector database abstraction for Cognee-Rust.
//!
//! Provides vector storage and similarity search for embeddings.

/// Error types for vector database operations.
pub mod error;
/// Data models for vector points, search results, and collection configuration.
pub mod models;
/// Vector database trait definition.
pub mod vector_db_trait;

#[cfg(feature = "pgvector")]
pub mod pgvector_adapter;

#[cfg(feature = "testing")]
pub mod mock_vector_db;

pub use error::{VectorDBError, VectorDBResult};
pub use models::{CollectionConfig, DistanceMetric, SearchResult, VectorPoint};
pub use vector_db_trait::VectorDB;

#[cfg(feature = "pgvector")]
pub use pgvector_adapter::PgVectorAdapter;

#[cfg(feature = "testing")]
pub use mock_vector_db::MockVectorDB;
