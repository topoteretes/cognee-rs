//! Unified public API for Cognee-Rust.
//!
//! This crate provides a single entry point by re-exporting the core operations:
//! - add (`IngestPipeline`)
//! - cognify (`CognifyPipeline` and related types)
//! - search (`SearchBuilder`/`SearchOrchestrator` and related types)

pub mod add {
    pub use cognee_ingestion::{ContentHasher, IngestPipeline};
}

pub mod cognify {
    pub use cognee_chunking::{ChunkingError, CutType, ExtractTextChunksPipeline, WordCounter};
    pub use cognee_cognify::*;
}

pub mod search {
    pub use cognee_search::*;
}

pub mod delete {
    pub use cognee_delete::*;
}

pub mod models {
    pub use cognee_models::*;
}

pub mod storage {
    pub use cognee_storage::*;
}

pub mod database {
    pub use cognee_database::*;
}

pub mod graph {
    pub use cognee_graph::*;
}

pub mod vector {
    pub use cognee_vector::*;
}

pub mod embedding {
    pub use cognee_embedding::*;
}

pub mod llm {
    pub use cognee_llm::*;
}

pub mod ontology {
    pub use cognee_ontology::*;
}

pub mod component_manager;
pub mod config;
pub mod context;
pub mod error;

pub use component_manager::ComponentManager;
pub use config::Settings;
pub use context::PipelineContext;
pub use error::ComponentError;

pub mod prelude {
    pub use crate::add::IngestPipeline;
    pub use crate::cognify::{CognifyConfig, CognifyPipeline};
    pub use crate::database::{DatabaseTrait, SqliteDatabase};
    pub use crate::graph::GraphDBTrait;
    pub use crate::llm::Llm;
    pub use crate::models::{Data, DataInput, Dataset};
    pub use crate::search::{SearchBuilder, SearchOrchestrator, SearchRequest, SearchType};
    pub use crate::storage::{LocalStorage, StorageTrait};
    pub use crate::vector::VectorDB;
    pub use uuid::Uuid;
}

pub use add::{ContentHasher, IngestPipeline};
pub use cognee_cognify::*;
pub use cognee_delete::*;
pub use cognee_search::*;

pub use cognee_database;
pub use cognee_delete;
pub use cognee_embedding;
pub use cognee_graph;
pub use cognee_llm;
pub use cognee_models;
pub use cognee_ontology;
pub use cognee_storage;
pub use cognee_vector;
pub use uuid;
