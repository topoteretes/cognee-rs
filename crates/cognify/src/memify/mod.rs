//! Memify pipeline -- graph enrichment via triplet embedding.
//!
//! Reads an existing knowledge graph and creates searchable vector
//! embeddings for triplets (subject-relationship-object), enabling
//! `SearchType::TripletCompletion` queries.
//!
//! # Usage
//!
//! ```ignore
//! use cognee_cognify::memify::{memify, MemifyConfig};
//!
//! let result = memify(
//!     &*graph_db, &*vector_db, &*embedding_engine,
//!     Some(dataset_id), Some(owner_id), None,
//!     &MemifyConfig::default(),
//! ).await?;
//! ```

pub mod config;
pub mod error;
pub mod extract_triplets;
pub mod index_triplets;
pub mod pipeline;

pub use config::MemifyConfig;
pub use error::MemifyError;
pub use index_triplets::IndexResult;
pub use pipeline::{MemifyResult, memify};
