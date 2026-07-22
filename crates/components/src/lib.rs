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
//! External adapter crates (e.g. the closed `cognee-vector-qdrant` /
//! `cognee-llm-litert`) implement [`VectorDbFactory`] / [`GraphDbFactory`] /
//! [`LlmFactory`] / [`EmbeddingFactory`] and register them via explicit
//! dependency injection:
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
mod traits;

pub use builtins::{build_database, build_embedding_config, build_storage};
pub use context::{BackendBuildContext, EmbeddingInputs, LlmInputs};
pub use error::ComponentError;
pub use registry::ComponentRegistry;
pub use traits::{EmbeddingFactory, GraphDbFactory, LlmFactory, VectorDbFactory};

// Re-export the default embedding factory so external callers can compose it.
pub use builtins::embedding::DefaultEmbeddingFactory;

/// Compile-time assertion that the registry is `Send + Sync`, so a
/// `ComponentManager` holding one stays `Send + Sync` (required by the
/// `Arc<HandleState>` bindings layer).
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ComponentRegistry>();
};
