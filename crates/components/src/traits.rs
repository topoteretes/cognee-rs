//! Adapter factory traits — the extension surface.
//!
//! Each trait maps a lowercase provider id to a constructor over a
//! [`BackendBuildContext`]. OSS registers built-in factories in
//! [`crate::ComponentRegistry::with_builtins`]; external adapter crates
//! implement these traits and register their factories at their own binary entry points.
//!
//! Adding a backend must never require a change to this repository — see
//! `docs/guides/external-providers.md` for what that commits this crate to.
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

use cognee_cognify::graph_backend::ChunkGraphExtractor;
use cognee_config_core::SectionSchema;
use cognee_database::RelationalDb;
use cognee_ontology::OntologyResolver;
use cognee_session::SessionStore;
use cognee_storage::StorageTrait;

use crate::context::BackendBuildContext;
use crate::error::ComponentError;

/// Builds a vector database backend for one provider id.
#[async_trait]
pub trait VectorDbFactory: Send + Sync {
    /// Lowercase provider id this factory serves (e.g. `"lancedb"`, `"qdrant"`).
    fn provider(&self) -> &str;
    /// The configuration sections this factory reads, so registering the
    /// factory registers its settings (see `cognee_config_core::section`).
    /// A factory that is not registered contributes no settings.
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
    /// Construct the backend from the resolved context.
    async fn build(&self, ctx: &BackendBuildContext) -> Result<Arc<dyn VectorDB>, ComponentError>;
}

/// Builds a knowledge-graph database backend for one provider id.
#[async_trait]
pub trait GraphDbFactory: Send + Sync {
    /// Lowercase provider id this factory serves (e.g. `"ladybug"`).
    fn provider(&self) -> &str;
    /// The configuration sections this factory reads, so registering the
    /// factory registers its settings (see `cognee_config_core::section`).
    /// A factory that is not registered contributes no settings.
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
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
    /// The configuration sections this factory reads, so registering the
    /// factory registers its settings (see `cognee_config_core::section`).
    /// A factory that is not registered contributes no settings.
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
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

/// Builds the embedding engine for one provider id.
///
/// The registry selects the factory by `embedding.provider`. An empty provider
/// defaults to `onnx`; a provider with no registered factory is an error, so a
/// typo or a compiled-out backend fails loudly instead of embedding with the
/// wrong model. `mock` is handled by the registry itself.
#[async_trait]
pub trait EmbeddingFactory: Send + Sync {
    /// Lowercase provider id this factory serves (e.g. `"openai"`, `"onnx"`).
    fn provider(&self) -> &str;
    /// The configuration sections this factory reads, so registering the
    /// factory registers its settings (see `cognee_config_core::section`).
    /// A factory that is not registered contributes no settings.
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
    /// Construct the embedding engine from the resolved context.
    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn EmbeddingEngine>, ComponentError>;
}

/// Builds the relational database for one backend id (`relational.provider`).
#[async_trait]
pub trait RelationalDbFactory: Send + Sync {
    /// Lowercase backend id this factory serves (e.g. `"sqlite"`).
    fn provider(&self) -> &str;
    /// The configuration sections this factory reads.
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
    /// Connect to the database at `ctx.relational_db_url` (resolved from the
    /// `relational` section) and bring its schema up to date.
    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn RelationalDb>, ComponentError>;
}

/// Builds the session (Q&A history) store for one store id (`session.store`).
#[async_trait]
pub trait SessionStoreFactory: Send + Sync {
    /// Lowercase store id this factory serves (e.g. `"relational"`).
    fn provider(&self) -> &str;
    /// The configuration sections this factory reads.
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
    /// Construct the store. `database` is the relational database the other
    /// components share, for a store that keeps sessions there.
    async fn build(
        &self,
        ctx: &BackendBuildContext,
        database: Arc<dyn RelationalDb>,
    ) -> Result<Arc<dyn SessionStore>, ComponentError>;
}

/// Builds the file-storage backend for one provider id (`storage.backend`).
#[async_trait]
pub trait StorageFactory: Send + Sync {
    /// Lowercase provider id this factory serves (e.g. `"local"`).
    fn provider(&self) -> &str;
    /// The configuration sections this factory reads (see
    /// [`VectorDbFactory::config_sections`]).
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
    /// Construct and initialize the storage backend.
    async fn build(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn StorageTrait>, ComponentError>;
}

/// Builds an ontology resolver for one resolver id (`ontology.resolver`).
///
/// The registry only asks a factory to build when an ontology file is
/// configured; without one it uses the no-op resolver.
#[async_trait]
pub trait OntologyResolverFactory: Send + Sync {
    /// Lowercase resolver id this factory serves (e.g. `"rdflib"`).
    fn provider(&self) -> &str;
    /// The configuration sections this factory reads.
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
    /// Load the ontology at `file_path` (one path, or a comma-separated list
    /// for multi-file ontologies, as the resolver accepts).
    async fn build(
        &self,
        ctx: &BackendBuildContext,
        file_path: &str,
    ) -> Result<Arc<dyn OntologyResolver>, ComponentError>;
}

/// Builds a graph-extraction backend (an alternative to the built-in LLM
/// extractor, e.g. GLiNER) for one kind.
///
/// Built per cognify run, not cached: a fresh instance is the right place to
/// reset any per-run state. `spec` is the caller's per-call selection: the
/// kind as a JSON string, or an object `{"kind": …, …tuning}` whose extra
/// fields override the factory's configured defaults.
#[async_trait]
pub trait GraphExtractorFactory: Send + Sync {
    /// Lowercase kind this factory serves (e.g. `"gliner"`). `"llm"` is
    /// reserved for the built-in extractor.
    fn provider(&self) -> &str;
    /// The configuration sections this factory reads (process-wide defaults
    /// such as a model directory or endpoint).
    fn config_sections(&self) -> Vec<&'static SectionSchema> {
        Vec::new()
    }
    /// Construct the extractor for one run.
    async fn build(
        &self,
        ctx: &BackendBuildContext,
        spec: &serde_json::Value,
    ) -> Result<Arc<dyn ChunkGraphExtractor>, ComponentError>;
}
