//! `cognee-components` — shared construction of pipeline backends.
//!
//! Both the `ComponentManager` (cognee, lazy version-cached) and the HTTP
//! server's standalone wiring (eager) build the same seven backends — storage,
//! relational database, graph DB, vector DB, embedding engine, LLM, and
//! transcriber. This crate holds that construction logic once, behind a
//! [`ComponentRegistry`] that maps provider ids to adapter factories.
//!
//! ## Extension
//!
//! External adapter crates implement [`VectorDbFactory`] / [`GraphDbFactory`]
//! / [`LlmFactory`] / [`EmbeddingFactory`] / [`RelationalDbFactory`] / [`SessionStoreFactory`] /
//! [`StorageFactory`] /
//! [`OntologyResolverFactory`] / [`GraphExtractorFactory`] and register them
//! via explicit dependency injection:
//!
//! ```ignore
//! let mut reg = ComponentRegistry::with_builtins();
//! reg.register_vector(Arc::new(QdrantVectorFactory));
//! reg.register_llm(Arc::new(LiteRtLlmFactory));
//! // hand `reg` to ComponentManager::with_registry / wire_default_backends_with
//! ```
//!
//! ## The construction contract
//!
//! Callers lower their own config type into a [`BackendBuildContext`], doing all
//! provider-specific URL resolution and environment reads at that boundary. The
//! registry and its factories are pure over the context — see
//! [`context`] for details.

mod builtins;
mod context;
mod error;
mod registry;
pub mod sections;
mod traits;

pub use builtins::embedding::{OnnxAssetDefaults, onnx_asset_defaults};
pub use builtins::{build_database, build_embedding_config, build_storage};
pub use context::{
    AwsInputs, BackendBuildContext, EmbeddingInputs, LlmInputs, parse_reasoning_override,
    parse_structured_output_mode,
};
pub use error::ComponentError;
pub use registry::{ComponentRegistry, LLM_GRAPH_EXTRACTOR, default_graph_provider};
pub use sections::{
    AnthropicConfig, AwsConfig, EmbeddingConfig, FsSessionConfig, GraphConfig,
    GraphExtractionConfig, LlmConfig, OllamaEmbeddingConfig, OnnxEmbeddingConfig, OntologyConfig,
    OpenAiCompatibleConfig, OpenAiCompatibleEmbeddingConfig, RedisSessionConfig, RelationalConfig,
    SessionConfig, StorageConfig, VectorConfig,
};
pub use traits::{
    EmbeddingFactory, GraphDbFactory, GraphExtractorFactory, LlmFactory, OntologyResolverFactory,
    RelationalDbFactory, SessionStoreFactory, StorageFactory, VectorDbFactory,
};

// Re-export the default embedding factory so external callers can compose it.
pub use builtins::database::SeaOrmRelationalFactory;
pub use builtins::embedding::DefaultEmbeddingFactory;
pub use builtins::ontology::RdfLibOntologyFactory;
pub use builtins::session::RelationalSessionFactory;
pub use builtins::storage::LocalStorageFactory;

/// Compile-time assertion that the registry is `Send + Sync`, so a
/// `ComponentManager` holding one stays `Send + Sync` (required by the
/// `Arc<HandleState>` bindings layer).
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ComponentRegistry>();
};
