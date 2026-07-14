//! [`ComponentRegistry`] — the pluggable provider → factory map shared by the
//! `ComponentManager` (cognee-lib) and the HTTP server's standalone wiring.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{Llm, Transcriber};
use cognee_vector::VectorDB;

use crate::builtins::embedding::DefaultEmbeddingFactory;
use crate::builtins::llm::{self, OpenAiCompatibleLlmFactory};
use crate::context::BackendBuildContext;
use crate::error::ComponentError;
use crate::traits::{EmbeddingFactory, GraphDbFactory, LlmFactory, VectorDbFactory};

/// Maps lowercase provider ids to adapter factories, per component kind.
///
/// Construct with [`with_builtins`](Self::with_builtins) to get the OSS
/// provider set, then `register_*` external adapters before handing the
/// registry to a `ComponentManager` or the HTTP server wiring. `build_*` is the
/// single construction path both callers share.
///
/// Vector / graph / llm are keyed by provider id (the real extension points).
/// Embedding holds a single replaceable factory, because provider selection
/// happens inside `EmbeddingConfig::create_engine`.
pub struct ComponentRegistry {
    vector: HashMap<String, Arc<dyn VectorDbFactory>>,
    graph: HashMap<String, Arc<dyn GraphDbFactory>>,
    llm: HashMap<String, Arc<dyn LlmFactory>>,
    embedding: Arc<dyn EmbeddingFactory>,
}

impl ComponentRegistry {
    /// An empty registry with only the default embedding factory installed.
    /// Prefer [`with_builtins`](Self::with_builtins) unless you are assembling
    /// a bespoke provider set from scratch.
    pub fn empty() -> Self {
        Self {
            vector: HashMap::new(),
            graph: HashMap::new(),
            llm: HashMap::new(),
            embedding: Arc::new(DefaultEmbeddingFactory),
        }
    }

    /// Registry pre-populated with the OSS built-in factories that the enabled
    /// cargo features make available.
    pub fn with_builtins() -> Self {
        let mut reg = Self::empty();

        // ── vector ────────────────────────────────────────────────────────
        // brute-force is always available. Its spelling variants (brute_force,
        // bruteforce) are canonicalized at lookup time (see `build_vector`), so
        // it registers under the single canonical key — this keeps a
        // `register_vector` override consistent across all spellings.
        reg.register_vector(Arc::new(crate::builtins::vector::BruteForceFactory));
        // lancedb is registered unconditionally; the Android fallback lives
        // inside its build(), keeping the provider id target-invariant.
        reg.register_vector(Arc::new(crate::builtins::vector::LanceDbFactory));
        #[cfg(feature = "pgvector")]
        reg.register_vector(Arc::new(crate::builtins::vector::PgVectorFactory));
        #[cfg(feature = "testing")]
        reg.register_vector(Arc::new(crate::builtins::vector::MockVectorFactory));

        // ── graph ─────────────────────────────────────────────────────────
        #[cfg(feature = "ladybug")]
        {
            reg.register_graph(Arc::new(crate::builtins::graph::LadybugGraphFactory::new(
                "ladybug",
            )));
            reg.register_graph(Arc::new(crate::builtins::graph::LadybugGraphFactory::new(
                "kuzu",
            )));
        }
        #[cfg(feature = "pggraph")]
        {
            reg.register_graph(Arc::new(crate::builtins::graph::PgGraphFactory::new(
                "postgres",
            )));
            reg.register_graph(Arc::new(crate::builtins::graph::PgGraphFactory::new(
                "postgresql",
            )));
        }
        #[cfg(feature = "testing")]
        reg.register_graph(Arc::new(crate::builtins::graph::MockGraphFactory));

        // ── llm ───────────────────────────────────────────────────────────
        for id in llm::OPENAI_COMPATIBLE_PROVIDERS {
            reg.register_llm(Arc::new(OpenAiCompatibleLlmFactory::new(id)));
        }

        reg
    }

    // ── registration (extension points) ───────────────────────────────────

    /// Register (or override) a vector backend factory under `f.provider()`.
    pub fn register_vector(&mut self, f: Arc<dyn VectorDbFactory>) {
        self.vector.insert(f.provider().to_lowercase(), f);
    }

    /// Register (or override) a graph backend factory under `f.provider()`.
    pub fn register_graph(&mut self, f: Arc<dyn GraphDbFactory>) {
        self.graph.insert(f.provider().to_lowercase(), f);
    }

    /// Register (or override) an LLM factory under `f.provider()`.
    pub fn register_llm(&mut self, f: Arc<dyn LlmFactory>) {
        self.llm.insert(f.provider().to_lowercase(), f);
    }

    /// Replace the embedding factory (provider selection is internal to the
    /// engine, so there is a single slot).
    pub fn set_embedding(&mut self, f: Arc<dyn EmbeddingFactory>) {
        self.embedding = f;
    }

    /// Provider ids with a registered vector factory (sorted). Used by the
    /// drift-guard test and for actionable error messages.
    pub fn vector_providers(&self) -> Vec<String> {
        let mut v: Vec<String> = self.vector.keys().cloned().collect();
        v.sort();
        v
    }

    /// Provider ids with a registered graph factory (sorted).
    pub fn graph_providers(&self) -> Vec<String> {
        let mut v: Vec<String> = self.graph.keys().cloned().collect();
        v.sort();
        v
    }

    /// Provider ids with a registered LLM factory (sorted).
    pub fn llm_providers(&self) -> Vec<String> {
        let mut v: Vec<String> = self.llm.keys().cloned().collect();
        v.sort();
        v
    }

    // ── construction (shared build path) ───────────────────────────────────

    /// Build the vector backend selected by `ctx.vector_provider`.
    pub async fn build_vector(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn VectorDB>, ComponentError> {
        let key = canonical_vector_provider(&ctx.vector_provider);
        let factory = self.vector.get(&key).ok_or_else(|| {
            ComponentError::Config(unsupported_msg(
                "vector_db_provider",
                &ctx.vector_provider,
                &self.vector_providers(),
            ))
        })?;
        factory.build(ctx).await
    }

    /// Build the graph backend selected by `ctx.graph_provider`.
    pub async fn build_graph(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
        let key = ctx.graph_provider.to_lowercase();
        let factory = self.graph.get(&key).ok_or_else(|| {
            ComponentError::Config(unsupported_msg(
                "graph_database_provider",
                &ctx.graph_provider,
                &self.graph_providers(),
            ))
        })?;
        factory.build(ctx).await
    }

    /// Build the LLM adapter selected by `ctx.llm.provider`.
    ///
    /// A mock request (`ctx.llm.mock` or `provider == "mock"`) replaces the
    /// adapter entirely, before provider lookup. A non-empty
    /// `ctx.llm.record_path` wraps the built real adapter in a recorder. Both
    /// are applied here so every provider — including externally-registered
    /// ones — gets identical mock/record semantics.
    pub async fn build_llm(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn Llm>, ComponentError> {
        if ctx.llm.mock || ctx.llm.provider == "mock" {
            return llm::build_mock_llm(ctx);
        }

        let key = ctx.llm.provider.to_lowercase();
        let factory = self.llm.get(&key).ok_or_else(|| {
            ComponentError::Config(unsupported_msg(
                "llm_provider",
                &ctx.llm.provider,
                &self.llm_providers(),
            ))
        })?;
        let adapter = factory.build(ctx).await?;

        if !ctx.llm.record_path.trim().is_empty() {
            return llm::wrap_recording(adapter, &ctx.llm.record_path);
        }
        Ok(adapter)
    }

    /// Build a transcriber for `ctx.llm.provider`, or `Ok(None)` when the
    /// provider does not support audio transcription. Never mock-overridden or
    /// record-wrapped (only the real adapter implements `Transcriber`).
    pub async fn build_transcriber(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Option<Arc<dyn Transcriber>>, ComponentError> {
        let key = ctx.llm.provider.to_lowercase();
        match self.llm.get(&key) {
            Some(factory) => factory.build_transcriber(ctx).await,
            None => Ok(None),
        }
    }

    /// Build the embedding engine via the (single) embedding factory.
    pub async fn build_embedding(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        self.embedding.build(ctx).await
    }
}

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Canonicalize a vector-provider string, collapsing the historical
/// brute-force spelling variants (`brute_force`, `bruteforce`) onto the single
/// registered key `brute-force`. All other providers pass through lowercased.
fn canonical_vector_provider(provider: &str) -> String {
    match provider.to_lowercase().as_str() {
        "brute-force" | "brute_force" | "bruteforce" => "brute-force".to_string(),
        other => other.to_string(),
    }
}

fn unsupported_msg(field: &str, provider: &str, supported: &[String]) -> String {
    // Feature-gated built-ins are simply absent from the registry when their
    // cargo feature is off; point the operator at the feature rather than
    // letting it read as an unknown-backend problem. The hint is keyed on BOTH
    // the component kind (`field`) and the provider, so a graph feature is never
    // suggested for a vector error (or vice versa), and the ladybug/pggraph
    // built-ins each map to the feature that gates them.
    let p = provider.to_lowercase();
    let hint = match field {
        "graph_database_provider" => match p.as_str() {
            "ladybug" | "kuzu" => " Rebuild with the `ladybug` crate feature to enable it.",
            "postgres" | "postgresql" => " Rebuild with the `pggraph` crate feature to enable it.",
            "mock" => " Rebuild with the `testing` crate feature to enable it.",
            _ => "",
        },
        "vector_db_provider" => match p.as_str() {
            "pgvector" => " Rebuild with the `pgvector` crate feature to enable it.",
            _ => "",
        },
        _ => "",
    };
    format!(
        "Unsupported {field} '{provider}'. Registered providers: [{}].{hint} \
         Closed adapters (e.g. qdrant, litert) must be registered via \
         ComponentRegistry::register_* at the binary entry point.",
        supported.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The unsupported-provider feature hint must be keyed on BOTH the component
    // kind and the provider: a graph feature must never be suggested for a
    // vector error, and the ladybug/pggraph/pgvector built-ins each map to the
    // cargo feature that gates them.
    #[test]
    fn unsupported_msg_hint_is_field_aware() {
        let g = |p: &str| unsupported_msg("graph_database_provider", p, &[]);
        let v = |p: &str| unsupported_msg("vector_db_provider", p, &[]);
        // The message always echoes the provider name, so assert on the *hint*
        // phrase ("`<feature>` crate feature") rather than the provider substring.
        assert!(g("postgres").contains("`pggraph` crate feature"));
        assert!(g("ladybug").contains("`ladybug` crate feature"));
        assert!(v("pgvector").contains("`pgvector` crate feature"));
        // No cross-kind hint: a graph provider in a vector error (and vice-versa)
        // gets no feature hint at all.
        assert!(!v("postgres").contains("crate feature"));
        assert!(!g("pgvector").contains("crate feature"));
    }

    // Drift-guard: `with_builtins()` must register the documented provider set
    // for the enabled feature-set. Run with `--features testing` (and pgvector/
    // pggraph) to cover the gated providers. This test locks in the coverage so
    // a provider cannot silently vanish from one caller as it did before the
    // registry unified the two construction paths.
    #[test]
    fn builtins_register_documented_providers() {
        let reg = ComponentRegistry::with_builtins();

        // Vector: brute-force (canonical) and lancedb are always present.
        for id in ["brute-force", "lancedb"] {
            assert!(
                reg.vector_providers().iter().any(|p| p == id),
                "vector provider '{id}' must be registered; have {:?}",
                reg.vector_providers()
            );
        }
        // The brute-force spelling variants canonicalize to the same key.
        assert_eq!(canonical_vector_provider("brute_force"), "brute-force");
        assert_eq!(canonical_vector_provider("bruteforce"), "brute-force");
        #[cfg(feature = "pgvector")]
        assert!(reg.vector_providers().iter().any(|p| p == "pgvector"));
        #[cfg(feature = "testing")]
        assert!(
            reg.vector_providers().iter().any(|p| p == "mock"),
            "the `testing` feature must register the `mock` vector provider"
        );

        // Graph.
        #[cfg(feature = "ladybug")]
        for id in ["ladybug", "kuzu"] {
            assert!(reg.graph_providers().iter().any(|p| p == id));
        }
        #[cfg(feature = "pggraph")]
        for id in ["postgres", "postgresql"] {
            assert!(reg.graph_providers().iter().any(|p| p == id));
        }
        #[cfg(feature = "testing")]
        assert!(
            reg.graph_providers().iter().any(|p| p == "mock"),
            "the `testing` feature must register the `mock` graph provider"
        );

        // LLM: every OpenAI-compatible provider id.
        for id in crate::builtins::llm::OPENAI_COMPATIBLE_PROVIDERS {
            assert!(
                reg.llm_providers().iter().any(|p| p == id),
                "llm provider '{id}' must be registered; have {:?}",
                reg.llm_providers()
            );
        }
    }

    #[tokio::test]
    async fn build_vector_errors_on_unregistered_provider() {
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        ctx.vector_provider = "qdrant".to_string();
        let msg = match reg.build_vector(&ctx).await {
            Ok(_) => panic!("qdrant must not be registered in OSS builtins"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("qdrant"),
            "message should name the provider: {msg}"
        );
        assert!(
            msg.contains("register_"),
            "message should point at the registration seam: {msg}"
        );
    }

    fn test_ctx() -> BackendBuildContext {
        BackendBuildContext {
            data_root_directory: std::path::PathBuf::from("/tmp/cognee-test-data"),
            system_root_directory: std::path::PathBuf::from("/tmp/cognee-test-system"),
            relational_db_url: "sqlite::memory:".to_string(),
            graph_provider: "ladybug".to_string(),
            graph_file_path: String::new(),
            graph_postgres_url: None,
            vector_provider: "brute-force".to_string(),
            vector_db_url: String::new(),
            vector_postgres_url: None,
            embedding_dimensions: 384,
            embedding: crate::context::EmbeddingInputs {
                provider: "onnx".to_string(),
                model: "bge-small-en-v1.5".to_string(),
                dimensions: 384,
                endpoint: None,
                api_key: None,
                batch_size: 36,
                mock: false,
                mock_deterministic: false,
                api_version: None,
                huggingface_tokenizer: None,
                max_completion_tokens: 8191,
                onnx_model_path: std::path::PathBuf::new(),
                onnx_tokenizer_path: std::path::PathBuf::new(),
                onnx_model_name: "bge-small-en-v1.5".to_string(),
                onnx_dimensions: 384,
                onnx_max_sequence_length: 512,
                onnx_batch_size: 32,
            },
            llm: crate::context::LlmInputs {
                provider: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                api_key: "sk-test".to_string(),
                endpoint: String::new(),
                max_retries: 3,
                llm_args: serde_json::Map::new(),
                mock: false,
                cassette: String::new(),
                record_path: String::new(),
            },
        }
    }
}
