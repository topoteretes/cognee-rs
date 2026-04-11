//! Vector database abstraction for Cognee-Rust.
//!
//! Provides vector storage and similarity search for embeddings.

pub mod error;
pub mod models;
pub mod vector_db_trait;

#[cfg(feature = "qdrant")]
pub mod qdrant_adapter;

#[cfg(feature = "testing")]
pub mod mock_vector_db;

pub use error::{VectorDBError, VectorDBResult};
pub use models::{CollectionConfig, DistanceMetric, SearchResult, VectorPoint};
pub use vector_db_trait::VectorDB;

#[cfg(feature = "qdrant")]
pub use qdrant_adapter::QdrantAdapter;

#[cfg(feature = "testing")]
pub use mock_vector_db::MockVectorDB;
