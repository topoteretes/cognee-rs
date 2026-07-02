//! Built-in OSS component factories, registered by
//! [`crate::ComponentRegistry::with_builtins`].

pub mod database;
pub mod embedding;
pub mod graph;
pub mod llm;
pub mod storage;
pub mod vector;

pub use database::build_database;
pub use embedding::build_embedding_config;
pub use storage::build_storage;
