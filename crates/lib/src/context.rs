//! Pipeline context trait for shared component access.

use std::sync::Arc;

use async_trait::async_trait;

use cognee_database::DatabaseTrait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;

use crate::error::ComponentError;

/// Trait providing access to shared pipeline components.
///
/// Implementations lazily initialize and cache expensive components
/// (e.g., embedding models, database connections) so they can be
/// reused across multiple pipeline invocations.
#[async_trait]
pub trait PipelineContext: Send + Sync {
    async fn storage(&self) -> Result<Arc<dyn StorageTrait>, ComponentError>;
    async fn database(&self) -> Result<Arc<dyn DatabaseTrait>, ComponentError>;
    async fn graph_db(&self) -> Result<Arc<dyn GraphDBTrait>, ComponentError>;
    async fn vector_db(&self) -> Result<Arc<dyn VectorDB>, ComponentError>;
    async fn embedding_engine(&self) -> Result<Arc<dyn EmbeddingEngine>, ComponentError>;
    async fn llm(&self) -> Result<Arc<dyn Llm>, ComponentError>;
}
