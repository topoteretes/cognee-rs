//! Adapter factory traits — the extension surface.
//!
//! Each trait maps a lowercase provider id to a constructor over a
//! [`BackendBuildContext`]. OSS registers built-in factories in
//! [`crate::ComponentRegistry::with_builtins`]; external crates (e.g. the
//! closed `cognee-vector-qdrant` / `cognee-llm-litert`) implement these traits
//! and register their factories at their own binary entry points.
//!
//! All traits are `Send + Sync` and use `#[async_trait]` so that
//! `Arc<dyn XFactory>` is dyn-compatible and the holding `ComponentManager`
//! stays `Send + Sync` (required by the `Arc<HandleState>` bindings layer).

use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{Llm, Transcriber};
use cognee_vector::VectorDB;

use crate::context::BackendBuildContext;
use crate::error::ComponentError;

/// Builds a vector database backend for one provider id.
#[async_trait]
pub trait VectorDbFactory: Send + Sync {
    /// Lowercase provider id this factory serves (e.g. `"lancedb"`, `"qdrant"`).
    fn provider(&self) -> &str;
    /// Construct the backend from the resolved context.
    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn VectorDB>, ComponentError>;
}

/// Builds a knowledge-graph database backend for one provider id.
#[async_trait]
pub trait GraphDbFactory: Send + Sync {
    /// Lowercase provider id this factory serves (e.g. `"ladybug"`).
    fn provider(&self) -> &str;
    /// Construct the backend from the resolved context.
    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn GraphDBTrait>, ComponentError>;
}

/// Builds an LLM adapter (and, optionally, a matching transcriber) for one
/// provider id.
///
/// [`build`](Self::build) returns the *raw* provider adapter; cross-cutting
/// mock-override and record-wrapping are applied by
/// [`crate::ComponentRegistry::build_llm`], not here.
/// [`build_transcriber`](Self::build_transcriber) likewise builds a *raw*
/// adapter — it is never mock-overridden or record-wrapped, because only the
/// real OpenAI-compatible adapter implements [`Transcriber`].
#[async_trait]
pub trait LlmFactory: Send + Sync {
    /// Lowercase provider id this factory serves.
    fn provider(&self) -> &str;
    /// Construct the raw LLM adapter from the resolved context.
    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn Llm>, ComponentError>;
    /// Construct a transcriber for this provider, or `Ok(None)` when the
    /// provider does not support audio transcription (graceful no-audio).
    ///
    /// Defaults to `Ok(None)`.
    async fn build_transcriber(
        &self,
        _ctx: &BackendBuildContext,
    ) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        Ok(None)
    }
}

/// Builds the embedding engine.
///
/// Unlike the other kinds, embedding provider selection happens *inside*
/// `EmbeddingConfig::create_engine`, so the registry holds a single replaceable
/// embedding factory rather than a per-provider map. The default factory maps
/// [`crate::EmbeddingInputs`] to a config and preserves the historical
/// unknown-provider → `onnx` fallback.
#[async_trait]
pub trait EmbeddingFactory: Send + Sync {
    /// Construct the embedding engine from the resolved context.
    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn EmbeddingEngine>, ComponentError>;
}
