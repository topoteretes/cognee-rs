//! Vector database abstraction for Cognee-Rust.
//!
//! Provides vector storage and similarity search for embeddings.

pub mod error;
pub mod models;
pub mod qdrant_adapter;
pub mod vector_db_trait;

#[cfg(feature = "testing")]
pub mod mock_vector_db;

pub use error::{VectorDBError, VectorDBResult};
pub use models::{CollectionConfig, DistanceMetric, SearchResult, VectorPoint};
pub use qdrant_adapter::QdrantAdapter;
pub use vector_db_trait::VectorDB;

#[cfg(feature = "testing")]
pub use mock_vector_db::MockVectorDB;
