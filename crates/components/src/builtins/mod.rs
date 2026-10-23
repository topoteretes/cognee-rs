//! Built-in OSS component factories, registered by
//! [`crate::ComponentRegistry::with_builtins`].

pub mod database;
pub mod dataset_handlers;
pub mod embedding;
pub mod graph;
pub mod llm;
pub mod ontology;
#[cfg(any(feature = "pgvector", feature = "pggraph"))]
pub mod pg_dataset_handlers;
pub mod session;
pub mod storage;
pub mod vector;

pub use database::build_database;
pub use embedding::build_embedding_config;
pub use storage::build_storage;
