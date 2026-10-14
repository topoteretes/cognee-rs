//! [`ComponentRegistry`] — the pluggable provider → factory map shared by the
//! `ComponentManager` (cognee) and the HTTP server's standalone wiring.

use std::collections::HashMap;
use std::sync::Arc;

use cognee_cognify::graph_backend::ChunkGraphExtractor;
use cognee_config_core::{ConfigSchema, SchemaError, SectionSchema};
use cognee_database::RelationalDb;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{Llm, Transcriber};
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_session::SessionStore;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;

#[cfg(any(feature = "sqlite", feature = "postgres"))]
use crate::builtins::database::SeaOrmRelationalFactory;
use crate::builtins::embedding::DefaultEmbeddingFactory;
use crate::builtins::llm::{self, OpenAiCompatibleLlmFactory};
use crate::context::BackendBuildContext;
use crate::error::ComponentError;
use crate::sections::{OntologyConfig, RelationalConfig, SessionConfig, StorageConfig};
use crate::traits::{
    EmbeddingFactory, GraphDbFactory, GraphExtractorFactory, LlmFactory, OntologyResolverFactory,
    RelationalDbFactory, SessionStoreFactory, StorageFactory, VectorDbFactory,
};

/// Maps lowercase provider ids to adapter factories, per component kind.
///
/// Construct with [`with_builtins`](Self::with_builtins) to get the OSS
/// provider set, then `register_*` external adapters before handing the
/// registry to a `ComponentManager` or the HTTP server wiring. `build_*` is the
/// single construction path both callers share.
///
/// Every kind is keyed by lowercase provider id. For embedding,
/// [`build_embedding`](Self::build_embedding) selects the factory from
/// `ctx.embedding.provider` (empty means `onnx`); `mock` is handled by the
/// registry itself and needs no factory.
pub struct ComponentRegistry {
    vector: HashMap<String, Arc<dyn VectorDbFactory>>,
    graph: HashMap<String, Arc<dyn GraphDbFactory>>,
    llm: HashMap<String, Arc<dyn LlmFactory>>,
    embedding: HashMap<String, Arc<dyn EmbeddingFactory>>,
    storage: HashMap<String, Arc<dyn StorageFactory>>,
    relational: HashMap<String, Arc<dyn RelationalDbFactory>>,
    session_store: HashMap<String, Arc<dyn SessionStoreFactory>>,
    ontology: HashMap<String, Arc<dyn OntologyResolverFactory>>,
    graph_extractor: HashMap<String, Arc<dyn GraphExtractorFactory>>,
    /// Config sections registered directly (not through a factory).
    config_sections: Vec<&'static SectionSchema>,
}

impl ComponentRegistry {
    /// An empty registry: no factory of any kind, embedding included, so the
    /// only embedding provider it can build is `mock`. Register embedding
    /// factories with [`register_embedding`](Self::register_embedding) (for
    /// example `DefaultEmbeddingFactory::new("openai")`). Prefer
    /// [`with_builtins`](Self::with_builtins) unless you are assembling a
    /// bespoke provider set from scratch.
    pub fn empty() -> Self {
        Self {
            vector: HashMap::new(),
            graph: HashMap::new(),
            llm: HashMap::new(),
            embedding: HashMap::new(),
            storage: HashMap::new(),
            relational: HashMap::new(),
            session_store: HashMap::new(),
            ontology: HashMap::new(),
            graph_extractor: HashMap::new(),
            config_sections: Vec::new(),
        }
    }

    /// Registry pre-populated with the OSS built-in factories that the enabled
    /// cargo features make available.
    pub fn with_builtins() -> Self {
        let mut reg = Self::empty();

        // ── vector ────────────────────────────────────────────────────────
        // brute-force is always available. Its spelling variants (brute_force,
        // bruteforce) are canonicalized at lookup time (see
        // `resolve_vector_key`), so it registers under the single canonical key
        // — this keeps a `register_vector` override consistent across all
        // spellings. Registering it unconditionally is also what
        // ANDROID_LANCEDB_FALLBACK degrades to; keep it that way.
        reg.register_vector(Arc::new(crate::builtins::vector::BruteForceFactory));
        // lancedb is behind its own feature (the Arrow/lance native stack is a
        // large build, and not every consumer uses it). When enabled it registers
        // on every target — the Android fallback lives inside its build(), which
        // keeps the provider id target-invariant. NOTE: `lancedb` is also the
        // default `vector_db_provider`, so a build that drops the feature must set
        // VECTOR_DB_PROVIDER explicitly; unsupported_msg() hints at that. Android
        // is the exception: there `resolve_vector_key` degrades the id rather
        // than erroring, because no rebuild can satisfy it on that target.
        #[cfg(feature = "lancedb")]
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
        // Native Anthropic Messages API adapter (not OpenAI-compatible).
        reg.register_llm(Arc::new(llm::AnthropicLlmFactory));
        // Azure OpenAI: OpenAI-compatible wire, but api-key auth + api-version.
        reg.register_llm(Arc::new(llm::AzureLlmFactory));
        // AWS Bedrock Converse. Feature-gated because the AWS stack (aws-config
        // + aws-sigv4) is not free; this registration is the point at which
        // `LLM_PROVIDER=bedrock` works end to end.
        #[cfg(feature = "bedrock")]
        reg.register_llm(Arc::new(llm::BedrockLlmFactory));

        // ── embedding ─────────────────────────────────────────────────────
        // One factory per provider id, each registering its provider's config
        // section. `mock` is handled by `build_embedding` itself.
        #[cfg(feature = "onnx")]
        {
            reg.register_embedding(Arc::new(DefaultEmbeddingFactory::new("onnx")));
            reg.register_embedding(Arc::new(DefaultEmbeddingFactory::new("fastembed")));
        }
        for id in ["openai", "openai_compatible", "ollama"] {
            reg.register_embedding(Arc::new(DefaultEmbeddingFactory::new(id)));
        }
        #[cfg(feature = "bedrock")]
        reg.register_embedding(Arc::new(DefaultEmbeddingFactory::new("bedrock")));

        // ── relational ────────────────────────────────────────────────────
        // SeaORM, under each id its compiled drivers can serve.
        #[cfg(feature = "sqlite")]
        reg.register_relational(Arc::new(SeaOrmRelationalFactory::new("sqlite")));
        #[cfg(feature = "postgres")]
        {
            reg.register_relational(Arc::new(SeaOrmRelationalFactory::new("postgres")));
            reg.register_relational(Arc::new(SeaOrmRelationalFactory::new("postgresql")));
        }

        // ── session store ─────────────────────────────────────────────────
        // `seaorm` is the HTTP server's historical id for the same store.
        for id in ["relational", "seaorm"] {
            reg.register_session_store(Arc::new(
                crate::builtins::session::RelationalSessionFactory::new(id),
            ));
        }
        #[cfg(feature = "session-fs")]
        reg.register_session_store(Arc::new(crate::builtins::session::FsSessionFactory));
        #[cfg(feature = "session-redis")]
        reg.register_session_store(Arc::new(crate::builtins::session::RedisSessionFactory));

        // ── storage / ontology ────────────────────────────────────────────
        reg.register_storage(Arc::new(crate::builtins::storage::LocalStorageFactory));
        reg.register_ontology_resolver(Arc::new(crate::builtins::ontology::RdfLibOntologyFactory));

        // ── graph extraction ──────────────────────────────────────────────
        // None built in: `llm` is the cognify pipeline's own extractor, and
        // alternatives (GLiNER, …) come from external crates.

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

    /// Register (or override) an embedding factory under `f.provider()`.
    pub fn register_embedding(&mut self, f: Arc<dyn EmbeddingFactory>) {
        self.embedding.insert(f.provider().to_lowercase(), f);
    }

    /// Register (or override) a relational database factory under
    /// `f.provider()`.
    pub fn register_relational(&mut self, f: Arc<dyn RelationalDbFactory>) {
        self.relational.insert(f.provider().to_lowercase(), f);
    }

    /// Register (or override) a session-store factory under `f.provider()`.
    pub fn register_session_store(&mut self, f: Arc<dyn SessionStoreFactory>) {
        self.session_store.insert(f.provider().to_lowercase(), f);
    }

    /// Register (or override) a storage factory under `f.provider()`.
    pub fn register_storage(&mut self, f: Arc<dyn StorageFactory>) {
        self.storage.insert(f.provider().to_lowercase(), f);
    }

    /// Register (or override) an ontology resolver factory under
    /// `f.provider()`.
    pub fn register_ontology_resolver(&mut self, f: Arc<dyn OntologyResolverFactory>) {
        self.ontology.insert(f.provider().to_lowercase(), f);
    }

    /// Register (or override) a graph-extraction backend under
    /// `f.provider()`. The kind `llm` is reserved for the built-in extractor.
    ///
    /// # Errors
    /// [`ComponentError::Config`] for the reserved kind `llm`.
    pub fn register_graph_extractor(
        &mut self,
        f: Arc<dyn GraphExtractorFactory>,
    ) -> Result<(), ComponentError> {
        let kind = f.provider().to_lowercase();
        if kind == LLM_GRAPH_EXTRACTOR {
            return Err(ComponentError::Config(format!(
                "graph extractor kind `{LLM_GRAPH_EXTRACTOR}` is reserved for the built-in extractor"
            )));
        }
        self.graph_extractor.insert(kind, f);
        Ok(())
    }

    /// Former single-slot setter; now the same as
    /// [`register_embedding`](Self::register_embedding).
    #[deprecated(note = "embedding factories are per provider now; use register_embedding")]
    pub fn set_embedding(&mut self, f: Arc<dyn EmbeddingFactory>) {
        self.register_embedding(f);
    }

    /// Register a configuration section that is not owned by a factory (for
    /// example a server module's settings), so its keys join this registry's
    /// [`config_schema`](Self::config_schema).
    pub fn register_config_section(&mut self, section: &'static SectionSchema) {
        self.config_sections.push(section);
    }

    /// The kind sections (settings shared by every adapter of a kind, such as
    /// `llm.model`), the config sections of every registered factory, and
    /// those registered with
    /// [`register_config_section`](Self::register_config_section).
    ///
    /// Factories are visited in a stable order (kind, then provider id), so
    /// the schema does not depend on hash-map iteration order. Fails when two
    /// sections claim the same key, alias or primary env var.
    pub fn config_schema(&self) -> Result<ConfigSchema, SchemaError> {
        fn sorted<V>(map: &HashMap<String, V>) -> Vec<&V> {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            entries.into_iter().map(|(_, v)| v).collect()
        }
        // Kind sections first: the settings every adapter of a kind shares,
        // registered by the kind itself.
        let mut sections: Vec<&'static SectionSchema> = vec![
            <crate::sections::LlmConfig as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::EmbeddingConfig as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::VectorConfig as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::GraphConfig as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::RelationalConfig as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::SessionConfig as cognee_config_core::ConfigSection>::schema(),
            // Engine sections: settings of the pipelines rather than of a
            // pluggable component.
            <crate::sections::CognifySettings as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::SearchSettings as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::HybridSearchSettings as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::StorageConfig as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::OntologyConfig as cognee_config_core::ConfigSection>::schema(),
            <crate::sections::GraphExtractionConfig as cognee_config_core::ConfigSection>::schema(),
        ];
        for f in sorted(&self.vector) {
            sections.extend(f.config_sections());
        }
        for f in sorted(&self.graph) {
            sections.extend(f.config_sections());
        }
        for f in sorted(&self.llm) {
            sections.extend(f.config_sections());
        }
        for f in sorted(&self.embedding) {
            sections.extend(f.config_sections());
        }
        for f in sorted(&self.relational) {
            sections.extend(f.config_sections());
        }
        for f in sorted(&self.session_store) {
            sections.extend(f.config_sections());
        }
        for f in sorted(&self.storage) {
            sections.extend(f.config_sections());
        }
        for f in sorted(&self.ontology) {
            sections.extend(f.config_sections());
        }
        for f in sorted(&self.graph_extractor) {
            sections.extend(f.config_sections());
        }
        sections.extend(self.config_sections.iter().copied());

        let mut schema = ConfigSchema::new();
        for section in sections {
            schema.add(section)?;
        }
        Ok(schema)
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

    /// The graph provider to use when nothing selected one, derived from what
    /// this registry actually holds.
    ///
    /// Resolution order:
    /// 1. the first id in [`GRAPH_PROVIDER_PREFERENCE`] that is registered —
    ///    `ladybug` leads, so every build that has it keeps the historical
    ///    default bit-for-bit;
    /// 2. otherwise the single registered provider, when there is exactly one;
    /// 3. otherwise an error. Two shapes, because they need different actions:
    ///    nothing registered at all is a build-configuration problem, while two
    ///    or more unpreferred backends is a choice only the operator can make.
    ///
    /// The preference list carries one id per backend. The built-ins register
    /// two spellings each (`ladybug`/`kuzu`, `postgres`/`postgresql`), so rule 2
    /// on its own would call a plain `pggraph` build ambiguous.
    ///
    /// Why derive at all: the default used to be the literal `"ladybug"`, so an
    /// image compiled with `pggraph` and without `ladybug` aborted on every
    /// container start — `Unsupported graph_database_provider 'ladybug'.
    /// Registered providers: [postgres, postgresql]` — unless the deployment
    /// happened to set `GRAPH_DATABASE_PROVIDER` by hand.
    pub fn default_graph_provider(&self) -> Result<String, ComponentError> {
        if let Some(preferred) = GRAPH_PROVIDER_PREFERENCE
            .iter()
            .find(|id| self.graph.contains_key(**id))
        {
            return Ok((*preferred).to_string());
        }
        let registered = self.graph_providers();
        match registered.as_slice() {
            [only] => Ok(only.clone()),
            // Nothing to choose between: telling the operator to pick one of an
            // empty list is not actionable. This is a build-configuration
            // problem, so name the lever that fixes it.
            [] => Err(ComponentError::Config(
                "No graph backend is registered, so there is no default \
                 graph_database_provider to derive. Rebuild with the `ladybug` or \
                 `pggraph` crate feature, or register a factory at the binary \
                 entry point via ComponentRegistry::register_graph."
                    .to_string(),
            )),
            _ => Err(ComponentError::Config(format!(
                "Cannot derive a default graph_database_provider: the registered \
                 providers [{}] are two or more backends and include neither \
                 `ladybug` nor `postgres`, so there is no preference to apply. Set \
                 GRAPH_DATABASE_PROVIDER (or `graph_database_provider` in config) \
                 to one of them explicitly.",
                registered.join(", ")
            ))),
        }
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
        let key = self.resolve_vector_key(&ctx.vector_provider, ANDROID_LANCEDB_FALLBACK)?;
        // `resolve_vector_key` only returns keys it found in `self.vector`, so
        // this lookup cannot miss -- but re-using the same error keeps that an
        // invariant rather than a panic.
        let factory = self.vector.get(&key).ok_or_else(|| {
            ComponentError::Config(unsupported_msg(
                "vector_db_provider",
                &ctx.vector_provider,
                &self.vector_providers(),
            ))
        })?;
        factory.build(ctx).await
    }

    /// Resolve a vector-provider id to a key that is actually registered.
    ///
    /// Canonicalizes spelling variants, then applies the Android LanceDB
    /// fallback when `android_lancedb_fallback` is set and nothing is
    /// registered under `lancedb`. The flag is a parameter rather than a direct
    /// `cfg!` read so the behaviour is testable on the host.
    fn resolve_vector_key(
        &self,
        provider: &str,
        android_lancedb_fallback: bool,
    ) -> Result<String, ComponentError> {
        let key = canonical_vector_provider(provider);
        if self.vector.contains_key(&key) {
            return Ok(key);
        }
        // Only degrade the one id that is unbuildable on this platform by
        // construction, and only after a real lookup miss -- so an external
        // `lancedb` adapter registered via `register_vector` still wins, and
        // every other unregistered provider keeps the loud error.
        if android_lancedb_fallback
            && key == "lancedb"
            && self.vector.contains_key(ANDROID_LANCEDB_FALLBACK_KEY)
        {
            tracing::warn!(
                "vector_db_provider='{provider}' has no registered factory on this Android \
                 build (the `lancedb` feature is off because the Arrow + lance native stack \
                 does not cross-compile); falling back to in-memory brute-force -- the same \
                 backend a lancedb-enabled Android build resolves to. Set \
                 vector_db_provider='pgvector' for durable storage."
            );
            return Ok(ANDROID_LANCEDB_FALLBACK_KEY.to_string());
        }
        Err(ComponentError::Config(unsupported_msg(
            "vector_db_provider",
            provider,
            &self.vector_providers(),
        )))
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

    /// Build the embedding engine selected by `ctx.embedding.provider`.
    ///
    /// The id is trimmed and lowercased, matching
    /// [`register_embedding`](Self::register_embedding). A mock request
    /// (`ctx.embedding.mock` or provider `mock`) builds the mock engine without
    /// a factory; an empty id means `onnx`. An unregistered id is an error:
    /// an unrecognized one is reported as a typo, a recognized one as a
    /// provider this build does not include.
    ///
    /// The process-wide embedding pacer is installed here, before dispatch, so
    /// every provider gets it, including ones added with `register_embedding`.
    pub async fn build_embedding(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
        crate::builtins::embedding::install_embedding_pacer(ctx);
        // `mock` short-circuits provider selection, like the LLM mock.
        let provider = ctx.embedding.provider.trim().to_lowercase();
        if ctx.embedding.mock || provider == "mock" {
            return crate::builtins::embedding::build_mock_embedding(ctx).await;
        }
        // Empty means the default provider.
        let key = if provider.is_empty() {
            "onnx".to_string()
        } else {
            provider
        };
        if let Some(factory) = self.embedding.get(&key) {
            return factory.build(ctx).await;
        }
        // Not registered: an unrecognized id is a typo (never a silent ONNX
        // fallback); a recognized one is a provider compiled out of this build.
        crate::builtins::embedding::validate_provider_id(&key, false)?;
        Err(ComponentError::EmbeddingEngine(format!(
            "embedding provider '{key}' is not available in this build. Registered: {}",
            self.embedding_providers().join(", ")
        )))
    }

    /// Provider ids with a registered embedding factory (sorted).
    pub fn embedding_providers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.embedding.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Build the relational database selected by `relational.provider`, or,
    /// when that is empty, by the scheme of the resolved URL.
    pub async fn build_database(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn RelationalDb>, ComponentError> {
        let provider = ctx.section::<RelationalConfig>().provider;
        let mut key = provider.trim().to_lowercase();
        if key.is_empty() {
            key = ctx
                .relational_db_url
                .split(':')
                .next()
                .unwrap_or_default()
                .to_lowercase();
        }
        let factory = self.relational.get(&key).ok_or_else(|| {
            ComponentError::Config(unsupported_msg(
                "db_provider",
                &key,
                &self.relational_providers(),
            ))
        })?;
        factory.build(ctx).await
    }

    /// Backend ids with a registered relational factory (sorted).
    pub fn relational_providers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.relational.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Build the session store selected by `session.store` over the shared
    /// relational `database`.
    pub async fn build_session_store(
        &self,
        ctx: &BackendBuildContext,
        database: Arc<dyn RelationalDb>,
    ) -> Result<Arc<dyn SessionStore>, ComponentError> {
        let store = ctx.section::<SessionConfig>().store;
        let key = store.trim().to_lowercase();
        let key = if key.is_empty() {
            "relational".to_string()
        } else {
            key
        };
        let factory = self.session_store.get(&key).ok_or_else(|| {
            ComponentError::Config(unsupported_msg(
                "session_store",
                &key,
                &self.session_store_providers(),
            ))
        })?;
        factory.build(ctx, database).await
    }

    /// Store ids with a registered session-store factory (sorted).
    pub fn session_store_providers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.session_store.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Build the file storage selected by `storage.backend`.
    pub async fn build_storage(
        &self,
        ctx: &BackendBuildContext,
    ) -> Result<Arc<dyn StorageTrait>, ComponentError> {
        let backend = ctx.section::<StorageConfig>().backend;
        let key = backend.trim().to_lowercase();
        let key = if key.is_empty() {
            "local".to_string()
        } else {
            key
        };
        let factory = self.storage.get(&key).ok_or_else(|| {
            ComponentError::Config(unsupported_msg(
                "storage_backend",
                &backend,
                &self.storage_providers(),
            ))
        })?;
        factory.build(ctx).await
    }

    /// Provider ids with a registered storage factory (sorted).
    pub fn storage_providers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.storage.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Build the ontology resolver selected by `ontology.resolver`, loading
    /// `file_path` (or `ontology.file_path` when `None`). An empty path means
    /// no ontology: the no-op resolver, whatever the resolver id.
    pub async fn build_ontology_resolver(
        &self,
        ctx: &BackendBuildContext,
        file_path: Option<&str>,
    ) -> Result<Arc<dyn OntologyResolver>, ComponentError> {
        let cfg = ctx.section::<OntologyConfig>();
        let path = file_path.unwrap_or(&cfg.file_path).trim();
        if path.is_empty() {
            return Ok(Arc::new(NoOpOntologyResolver::new()));
        }
        let key = cfg.resolver.trim().to_lowercase();
        let key = if key.is_empty() {
            "rdflib".to_string()
        } else {
            key
        };
        let factory = self.ontology.get(&key).ok_or_else(|| {
            ComponentError::Config(unsupported_msg(
                "ontology_resolver",
                &cfg.resolver,
                &self.ontology_resolvers(),
            ))
        })?;
        factory.build(ctx, path).await
    }

    /// Resolver ids with a registered ontology factory (sorted).
    pub fn ontology_resolvers(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.ontology.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Whether `kind` names a registered graph-extraction backend (`llm`, the
    /// built-in extractor, is not one).
    pub fn has_graph_extractor(&self, kind: &str) -> bool {
        self.graph_extractor.contains_key(&kind.to_lowercase())
    }

    /// Build the graph-extraction backend registered under `kind` for one
    /// cognify run; `spec` is the caller's selection (see
    /// [`GraphExtractorFactory::build`]). `None` when no backend is
    /// registered under `kind`.
    pub async fn build_graph_extractor(
        &self,
        ctx: &BackendBuildContext,
        kind: &str,
        spec: &serde_json::Value,
    ) -> Option<Result<Arc<dyn ChunkGraphExtractor>, ComponentError>> {
        let factory = self.graph_extractor.get(&kind.to_lowercase())?;
        Some(factory.build(ctx, spec).await)
    }

    /// Kinds with a registered graph-extraction backend (sorted).
    pub fn graph_extractor_kinds(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.graph_extractor.keys().cloned().collect();
        ids.sort();
        ids
    }
}

/// The graph-extraction kind of cognify's own LLM extractor
/// (`graph_extraction.backend`'s default).
pub const LLM_GRAPH_EXTRACTOR: &str = "llm";

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// Whether an unregistered `lancedb` provider degrades to brute-force instead
/// of erroring.
///
/// True only on Android builds compiled without the `lancedb` feature — the one
/// first-party profile (`cognee/android-default`) that drops it, because the
/// Arrow + lance native stack does not cross-compile. A lancedb-*enabled*
/// Android build already resolves `lancedb` to brute-force inside
/// `LanceDbFactory::build`, so this keeps the provider id target-invariant
/// across both, which is what `Settings` values persisted before the feature
/// gate landed (and `VECTOR_DB_PROVIDER=lancedb`) depend on.
///
/// Every other slim consumer keeps the loud unsupported-provider error: on a
/// desktop build the missing feature is a build-configuration mistake the
/// operator can actually fix by rebuilding.
const ANDROID_LANCEDB_FALLBACK: bool = cfg!(all(target_os = "android", not(feature = "lancedb")));

/// The provider the Android LanceDB fallback degrades to. Registered
/// unconditionally by [`ComponentRegistry::with_builtins`].
const ANDROID_LANCEDB_FALLBACK_KEY: &str = "brute-force";

/// Canonical graph-backend ids, most preferred first, used by
/// [`ComponentRegistry::default_graph_provider`] when a build registers more
/// than one backend.
///
/// One entry per backend: alias spellings (`kuzu` for ladybug, `postgresql` for
/// pggraph) are deliberately absent, because a backend registered under two ids
/// must not read as two candidate backends. `ladybug` leads so that every build
/// that has it resolves exactly as it did before the default was derived.
const GRAPH_PROVIDER_PREFERENCE: [&str; 2] = ["ladybug", "postgres"];

/// What [`default_graph_provider`] falls back to when the built-in registry
/// cannot derive anything.
///
/// With only built-ins that means a build with no graph feature at all, since
/// every feature-driven combination resolves through the preference list.
/// Reporting the historical literal there keeps the diagnosis the operator
/// eventually gets from [`ComponentRegistry::build_graph`] — "Unsupported
/// graph_database_provider 'ladybug' … Rebuild with the `ladybug` crate
/// feature" — which names the actual problem better than a derivation error
/// about an empty registry would.
const GRAPH_PROVIDER_FALLBACK: &str = "ladybug";

/// The graph provider a build should default to, derived once from the
/// factories its compiled features register.
///
/// This is the value config defaults (`Settings::graph_database_provider`,
/// `HttpServerConfig::graph_provider`) use instead of a hardcoded string, so a
/// feature set that drops `ladybug` no longer defaults to a backend it cannot
/// build. See [`ComponentRegistry::default_graph_provider`] for the rule.
///
/// Memoised: building a registry is cheap but not free, and config defaults are
/// constructed far more than once per process.
pub fn default_graph_provider() -> &'static str {
    static DERIVED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DERIVED
        .get_or_init(|| {
            ComponentRegistry::with_builtins()
                .default_graph_provider()
                .unwrap_or_else(|_| GRAPH_PROVIDER_FALLBACK.to_string())
        })
        .as_str()
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
            "lancedb" => " Rebuild with the `lancedb` crate feature to enable it.",
            "pgvector" => " Rebuild with the `pgvector` crate feature to enable it.",
            "mock" => " Rebuild with the `testing` crate feature to enable it.",
            _ => "",
        },
        "db_provider" => match p.as_str() {
            "sqlite" => " Rebuild with the `sqlite` crate feature to enable it.",
            "postgres" | "postgresql" => " Rebuild with the `postgres` crate feature to enable it.",
            _ => "",
        },
        // A prebuilt cognee-cli or binding cannot be rebuilt by its user, and
        // may inherit a COGNEE_SESSION_STORE meant for the HTTP server, so the
        // hint also names the way out that needs no rebuild.
        "session_store" => match p.as_str() {
            "fs" => {
                " Rebuild with the `session-fs` crate feature to enable it, or set \
                 session.store (COGNEE_SESSION_STORE) to `relational` for this process."
            }
            "redis" => {
                " Rebuild with the `session-redis` crate feature to enable it, or set \
                 session.store (COGNEE_SESSION_STORE) to `relational` for this process."
            }
            _ => "",
        },
        "llm_provider" => match p.as_str() {
            // The only feature-gated built-in LLM provider: without `bedrock`
            // the factory is not registered at all, so LLM_PROVIDER=bedrock
            // lands here and must read as a build-configuration problem rather
            // than an unknown provider.
            "bedrock" => " Rebuild with the `bedrock` crate feature to enable it.",
            _ => "",
        },
        _ => "",
    };
    format!(
        "Unsupported {field} '{provider}'. Registered providers: [{}].{hint} \
         External adapters must be registered via \
         ComponentRegistry::register_* at the binary entry point.",
        supported.join(", ")
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
pub(crate) mod tests {
    use super::*;

    // A build that drops `lancedb` on Android must not hard-error on a provider
    // id it can never satisfy. `Settings::default()` names brute-force there,
    // but that only covers defaults: the CLI persists the whole `Settings` to
    // config.json (so any `config set` writes back whatever provider was in
    // effect), and `VECTOR_DB_PROVIDER` sets it too. Both bypass the default and
    // arrive here as an explicit "lancedb", which is how an Android install that
    // predates the feature gate breaks on upgrade. Resolution -- not the default
    // -- is the layer that has to absorb it.
    //
    // The `android_lancedb_fallback` flag is passed explicitly so both sides are
    // reachable from a host test; production callers pass
    // `ANDROID_LANCEDB_FALLBACK`.
    #[test]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test module: a failed resolution should fail the test loudly"
    )]
    fn android_lancedb_falls_back_to_brute_force_when_unregistered() {
        let mut reg = ComponentRegistry::empty();
        reg.register_vector(Arc::new(crate::builtins::vector::BruteForceFactory));
        assert!(!reg.vector_providers().iter().any(|p| p == "lancedb"));

        // On Android-without-lancedb the explicit id degrades instead of failing.
        assert_eq!(
            reg.resolve_vector_key("lancedb", true).unwrap(),
            "brute-force"
        );
        // Case and the historical spellings reach the same place.
        assert_eq!(
            reg.resolve_vector_key("LanceDB", true).unwrap(),
            "brute-force"
        );

        // Everywhere else the same input keeps the loud, actionable error.
        let err = reg
            .resolve_vector_key("lancedb", false)
            .expect_err("without the Android fallback this must stay an error");
        assert!(matches!(err, ComponentError::Config(_)));
        assert!(err.to_string().contains("`lancedb` crate feature"));

        // The fallback is scoped to `lancedb` alone: it must not swallow other
        // unregistered providers, on Android or anywhere else.
        assert!(reg.resolve_vector_key("pgvector", true).is_err());
        assert!(reg.resolve_vector_key("nonsense", true).is_err());
    }

    // The fallback fires only on a real lookup miss, so an external `lancedb`
    // adapter registered at the binary entry point still wins on Android --
    // `register_vector` stays the documented override.
    #[test]
    #[allow(
        clippy::unwrap_used,
        reason = "test module: a failed resolution should fail the test loudly"
    )]
    fn registered_lancedb_wins_over_the_android_fallback() {
        struct StubLanceDb;
        #[async_trait::async_trait]
        impl VectorDbFactory for StubLanceDb {
            fn provider(&self) -> &str {
                "lancedb"
            }
            async fn build(
                &self,
                _ctx: &BackendBuildContext,
            ) -> Result<Arc<dyn VectorDB>, ComponentError> {
                unreachable!("resolution-only test")
            }
        }

        let mut reg = ComponentRegistry::empty();
        reg.register_vector(Arc::new(crate::builtins::vector::BruteForceFactory));
        reg.register_vector(Arc::new(StubLanceDb));

        assert_eq!(reg.resolve_vector_key("lancedb", true).unwrap(), "lancedb");
    }

    // The fallback must stay off on every non-Android build, so a desktop
    // consumer that drops the feature still gets told to rebuild.
    #[test]
    #[cfg(not(target_os = "android"))]
    fn android_fallback_constant_is_off_on_non_android_targets() {
        assert!(!ANDROID_LANCEDB_FALLBACK);
    }

    /// A graph factory that only answers to `provider`, for composing registries
    /// whose provider set does not depend on this crate's enabled features.
    struct StubGraph(&'static str);
    #[async_trait::async_trait]
    impl GraphDbFactory for StubGraph {
        fn provider(&self) -> &str {
            self.0
        }
        async fn build(
            &self,
            _ctx: &BackendBuildContext,
        ) -> Result<Arc<dyn GraphDBTrait>, ComponentError> {
            unreachable!("default-resolution test")
        }
    }

    fn graph_registry(providers: &[&'static str]) -> ComponentRegistry {
        let mut reg = ComponentRegistry::empty();
        for p in providers {
            reg.register_graph(Arc::new(StubGraph(p)));
        }
        reg
    }

    // The default must follow what is registered, not a literal. This is the
    // case that used to abort every `pggraph`-only container at boot: the
    // registry knows postgres/postgresql, the default said "ladybug".
    #[test]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test module: a failed derivation should fail the test loudly"
    )]
    fn default_graph_provider_follows_the_registered_set() {
        // pggraph-only: both spellings of one backend still resolve to one
        // answer, via the preference list rather than the single-entry rule.
        assert_eq!(
            graph_registry(&["postgres", "postgresql"])
                .default_graph_provider()
                .unwrap(),
            "postgres"
        );

        // ladybug wins wherever it is registered, alone or alongside others, so
        // every build that has it keeps the pre-derivation default.
        assert_eq!(
            graph_registry(&["ladybug", "kuzu"])
                .default_graph_provider()
                .unwrap(),
            "ladybug"
        );
        assert_eq!(
            graph_registry(&["ladybug", "kuzu", "postgres", "postgresql", "mock"])
                .default_graph_provider()
                .unwrap(),
            "ladybug"
        );

        // A single registered provider is unambiguous even when it is not in
        // the preference list — a `testing`-only build, or one external adapter.
        assert_eq!(
            graph_registry(&["mock"]).default_graph_provider().unwrap(),
            "mock"
        );

        // pggraph + testing: the preference list decides, so the extra `mock`
        // registration does not make a deployable build ambiguous.
        assert_eq!(
            graph_registry(&["postgres", "postgresql", "mock"])
                .default_graph_provider()
                .unwrap(),
            "postgres"
        );

        // The two error shapes need different actions from the reader, so they
        // are different messages. Nothing registered is a build-configuration
        // problem -- pointing at an empty list to choose from would not be
        // actionable -- while two unpreferred backends is the operator's call.
        let err = graph_registry(&[])
            .default_graph_provider()
            .expect_err("an empty graph registry has no default to derive");
        assert!(matches!(err, ComponentError::Config(_)));
        assert!(
            err.to_string().contains("crate feature"),
            "the empty case must name the build lever, not an empty choice: {err}"
        );
        assert!(!err.to_string().contains("GRAPH_DATABASE_PROVIDER"));
        let err = graph_registry(&["mock", "neo4j"])
            .default_graph_provider()
            .expect_err("two unpreferred backends must not resolve silently");
        assert!(err.to_string().contains("mock, neo4j"));
        assert!(err.to_string().contains("GRAPH_DATABASE_PROVIDER"));
    }

    // The value the config defaults actually take: derived from `with_builtins`,
    // so it must name a provider this build can construct.
    #[test]
    fn derived_default_graph_provider_is_buildable_by_this_feature_set() {
        let derived = default_graph_provider();

        #[cfg(feature = "ladybug")]
        assert_eq!(
            derived, "ladybug",
            "a build with `ladybug` must keep the historical default"
        );
        #[cfg(all(not(feature = "ladybug"), feature = "pggraph"))]
        assert_eq!(
            derived, "postgres",
            "a `pggraph`-only build must default to a provider it registers"
        );

        // Whatever the feature set, the default has to be registered -- except
        // when there is no graph feature at all, where the fallback is what
        // produces the "rebuild with the `ladybug` crate feature" diagnosis.
        #[cfg(any(feature = "ladybug", feature = "pggraph", feature = "testing"))]
        {
            let registered = ComponentRegistry::with_builtins().graph_providers();
            assert!(
                registered.iter().any(|p| p == derived),
                "derived default '{derived}' is not registered; have {registered:?}"
            );
        }
        #[cfg(not(any(feature = "ladybug", feature = "pggraph", feature = "testing")))]
        assert_eq!(derived, GRAPH_PROVIDER_FALLBACK);
    }

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
        // `lancedb` is both feature-gated and the default vector provider, so this
        // is the hint an operator of a build without it actually sees.
        assert!(v("lancedb").contains("`lancedb` crate feature"));
        assert!(v("mock").contains("`testing` crate feature"));
        // `bedrock` is the only feature-gated built-in LLM provider, so a build
        // without the feature must point at it rather than read as an unknown
        // provider.
        let l = |p: &str| unsupported_msg("llm_provider", p, &[]);
        assert!(l("bedrock").contains("`bedrock` crate feature"));
        assert!(!l("openai").contains("crate feature"));
        // No cross-kind hint: a graph provider in a vector error (and vice-versa)
        // gets no feature hint at all.
        assert!(!v("postgres").contains("crate feature"));
        assert!(!g("pgvector").contains("crate feature"));
        assert!(!v("bedrock").contains("crate feature"));
    }

    // Drift-guard: `with_builtins()` must register the documented provider set
    // for the enabled feature-set. Run with `--features testing` (and pgvector/
    // pggraph) to cover the gated providers. This test locks in the coverage so
    // a provider cannot silently vanish from one caller as it did before the
    // registry unified the two construction paths.
    #[test]
    fn builtins_register_documented_providers() {
        let reg = ComponentRegistry::with_builtins();

        // Vector: brute-force (canonical) is always present; lancedb follows its
        // feature, like pgvector and mock below.
        assert!(
            reg.vector_providers().iter().any(|p| p == "brute-force"),
            "vector provider 'brute-force' must be registered; have {:?}",
            reg.vector_providers()
        );
        #[cfg(feature = "lancedb")]
        assert!(
            reg.vector_providers().iter().any(|p| p == "lancedb"),
            "the `lancedb` feature must register the `lancedb` vector provider; have {:?}",
            reg.vector_providers()
        );
        #[cfg(not(feature = "lancedb"))]
        assert!(
            !reg.vector_providers().iter().any(|p| p == "lancedb"),
            "without the `lancedb` feature the provider must NOT be registered; have {:?}",
            reg.vector_providers()
        );
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
        // Bedrock follows its feature, like lancedb above: on means the factory
        // is registered and `LLM_PROVIDER=bedrock` resolves; off means the
        // provider must be absent so the operator gets the unsupported-provider
        // message instead of a build without the AWS stack.
        #[cfg(feature = "bedrock")]
        assert!(
            reg.llm_providers().iter().any(|p| p == "bedrock"),
            "the `bedrock` feature must register the `bedrock` llm provider; have {:?}",
            reg.llm_providers()
        );
        #[cfg(not(feature = "bedrock"))]
        assert!(
            !reg.llm_providers().iter().any(|p| p == "bedrock"),
            "without the `bedrock` feature the provider must NOT be registered; have {:?}",
            reg.llm_providers()
        );
    }

    // `LLM_PROVIDER=bedrock` must resolve through the *registry lookup*, not
    // just exist as a type: `build_llm` keys on the lowercased provider id, so
    // this is the assertion that ties the factory's `provider()` string to the
    // env value operators actually set.
    #[cfg(feature = "bedrock")]
    #[test]
    fn with_builtins_registers_bedrock_llm_provider() {
        let reg = ComponentRegistry::with_builtins();
        assert!(
            reg.llm_providers().iter().any(|p| p == "bedrock"),
            "with_builtins() must register the `bedrock` llm provider; have {:?}",
            reg.llm_providers()
        );
        assert_eq!(crate::builtins::llm::BEDROCK_PROVIDER, "bedrock");
    }

    // Parity guard (plan §1.1 / §4 R5): Bedrock is absent from Python's
    // `_API_KEY_REQUIRED_PROVIDERS` and listed in `_NO_API_KEY_PROVIDERS`, so an
    // empty `LLM_API_KEY` is the normal IAM configuration and must NOT be
    // rejected the way the Anthropic factory rejects it.
    //
    // Kept strictly offline: the bearer token short-circuits `resolve_auth`
    // before any credential lookup (`aws/credentials.rs` — that early return is
    // documented as behaviour, not an optimisation), and the explicit region
    // short-circuits the ambient region chain, so nothing here touches IMDS,
    // SSO or the profile files.
    #[cfg(feature = "bedrock")]
    #[tokio::test]
    async fn bedrock_builds_without_an_api_key() {
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        ctx.llm.provider = "bedrock".to_string();
        // A converse-routed id cognee ships; an invoke-routed id fails by design.
        ctx.llm.model = "eu.anthropic.claude-haiku-4-5-20251001-v1:0".to_string();
        ctx.llm.api_key = String::new();
        ctx.sections.insert(
            "aws.bearer_token".to_string(),
            serde_json::json!("test-token"),
        );
        ctx.sections
            .insert("aws.region".to_string(), serde_json::json!("us-east-1"));

        let built = reg.build_llm(&ctx).await;
        let adapter = match built {
            Ok(adapter) => adapter,
            Err(e) => panic!(
                "an empty LLM_API_KEY must fall through to the credential ladder, not error: {e:?}"
            ),
        };
        // `is_ok()` alone would also pass if the factory built some other
        // adapter entirely; the model id is the cheapest proof that the
        // configured Bedrock model is what came back.
        assert_eq!(
            adapter.model(),
            "eu.anthropic.claude-haiku-4-5-20251001-v1:0"
        );

        // §6.4: Bedrock has no Whisper equivalent, so audio degrades to None
        // rather than erroring or wiring an adapter that 404s at runtime.
        match reg.build_transcriber(&ctx).await {
            Ok(None) => {}
            Ok(Some(_)) => panic!("bedrock must not advertise a transcriber (plan §6.4)"),
            Err(e) => {
                panic!("build_transcriber must return Ok(None) for bedrock, not error: {e:?}")
            }
        }
    }

    // The LLM-side twin of `aws_inputs_are_carried_into_the_embedding_config`
    // (`builtins::embedding`): without the `(&ctx.llm.aws).into()` carry-across
    // the factory would hand `BedrockAdapter::new` a defaulted `AwsInputs` and
    // every explicitly supplied credential/region/endpoint would be silently
    // replaced by the ambient environment.
    //
    // A syntactically invalid region is the discriminator: it is rejected at the
    // *first* rung of the §1.3 chain (`aws::region::validate`), before any
    // ambient lookup and before the credential ladder, so the error naming it
    // can only come from `ctx.llm.aws` having reached the adapter. Offline for
    // the same reason.
    #[cfg(feature = "bedrock")]
    #[tokio::test]
    async fn bedrock_carries_the_aws_inputs_into_the_adapter() {
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        ctx.llm.provider = "bedrock".to_string();
        ctx.llm.model = "eu.anthropic.claude-haiku-4-5-20251001-v1:0".to_string();
        ctx.llm.api_key = String::new();
        ctx.sections.insert(
            "aws.bearer_token".to_string(),
            serde_json::json!("test-token"),
        );
        ctx.sections
            .insert("aws.region".to_string(), serde_json::json!("Not A Region"));

        let err = match reg.build_llm(&ctx).await {
            Ok(_) => panic!(
                "the caller-supplied region never reached the adapter: an invalid region must \
                 fail the §1.3 chain instead of falling through to the ambient one"
            ),
            Err(e) => e,
        };
        assert!(
            matches!(&err, ComponentError::Llm(msg) if msg.contains("Not A Region")),
            "expected the region validation error naming the supplied region, got: {err:?}"
        );
    }

    // The companion to `bedrock_builds_without_an_api_key`: proves the factory
    // reaches `BedrockAdapter::new` with an empty key rather than rejecting it
    // first. An invoke-routed id is refused *inside* the adapter constructor
    // before any region/credential resolution (plan §6.7), so the error shape
    // distinguishes the two: `ComponentError::Llm` = the key was passed through;
    // a `ComponentError::Config` naming an API key = an Anthropic-style
    // key-required check crept in. Offline for the same reason — the route check
    // is the first statement in `new`.
    #[cfg(feature = "bedrock")]
    #[tokio::test]
    async fn bedrock_empty_api_key_is_never_a_config_rejection() {
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        ctx.llm.provider = "bedrock".to_string();
        ctx.llm.model = "invoke/amazon.titan-text-express-v1".to_string();
        ctx.llm.api_key = String::new();

        let err = match reg.build_llm(&ctx).await {
            Ok(_) => panic!("an invoke-routed chat model must be rejected by the adapter"),
            Err(e) => e,
        };
        assert!(
            !matches!(&err, ComponentError::Config(msg) if msg.contains("API key")),
            "bedrock must not require an API key (plan §1.1): {err:?}"
        );
        assert!(
            matches!(&err, ComponentError::Llm(msg) if msg.contains("Converse")),
            "expected the adapter's route rejection, got: {err:?}"
        );
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

    /// An embedding factory that records that it was chosen and then fails
    /// with its own id, so a test can tell which factory `build_embedding`
    /// dispatched to without constructing a real engine.
    struct StubEmbedding {
        id: &'static str,
        called: Arc<std::sync::atomic::AtomicBool>,
    }

    impl StubEmbedding {
        fn new(id: &'static str) -> (Arc<Self>, Arc<std::sync::atomic::AtomicBool>) {
            let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
            (
                Arc::new(Self {
                    id,
                    called: Arc::clone(&called),
                }),
                called,
            )
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingFactory for StubEmbedding {
        fn provider(&self) -> &str {
            self.id
        }
        async fn build(
            &self,
            _ctx: &BackendBuildContext,
        ) -> Result<Arc<dyn EmbeddingEngine>, ComponentError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(ComponentError::EmbeddingEngine(format!("stub:{}", self.id)))
        }
    }

    fn embedding_error(result: Result<Arc<dyn EmbeddingEngine>, ComponentError>) -> ComponentError {
        match result {
            Ok(_) => panic!("expected an error, got an engine"),
            Err(e) => e,
        }
    }

    // `mock` needs no factory: an empty registry still builds it, from the
    // provider id in any case or padding, and from the `mock` flag alone.
    #[tokio::test]
    async fn build_embedding_mock_short_circuits_without_a_factory() {
        let mut reg = ComponentRegistry::empty();
        // A factory registered under `mock` must not be consulted either.
        let (stub, called) = StubEmbedding::new("mock");
        reg.register_embedding(stub);
        for provider in ["mock", "MOCK", " mock "] {
            let mut ctx = test_ctx();
            ctx.embedding.provider = provider.to_string();
            ctx.embedding.mock = false;
            if let Err(e) = reg.build_embedding(&ctx).await {
                panic!("provider {provider:?} must build the mock engine: {e:?}");
            }
        }
        let mut ctx = test_ctx();
        ctx.embedding.provider = "openai".to_string();
        ctx.embedding.mock = true;
        if let Err(e) = reg.build_embedding(&ctx).await {
            panic!("the mock flag must override the provider: {e:?}");
        }
        assert!(
            !called.load(std::sync::atomic::Ordering::SeqCst),
            "mock must short-circuit before factory lookup"
        );
    }

    // The lookup key must match what `register_embedding` stores: a factory
    // registered under a mixed-case id is reached from any spelling of it.
    #[tokio::test]
    async fn build_embedding_dispatches_to_a_registered_factory_case_insensitively() {
        let mut reg = ComponentRegistry::empty();
        let (stub, called) = StubEmbedding::new("MyEmbed");
        reg.register_embedding(stub);
        assert_eq!(reg.embedding_providers(), vec!["myembed".to_string()]);

        for provider in ["myembed", "MyEmbed", "  MYEMBED "] {
            called.store(false, std::sync::atomic::Ordering::SeqCst);
            let mut ctx = test_ctx();
            ctx.embedding.provider = provider.to_string();
            let err = embedding_error(reg.build_embedding(&ctx).await);
            assert!(
                matches!(&err, ComponentError::EmbeddingEngine(m) if m == "stub:MyEmbed"),
                "provider {provider:?} must reach the registered factory, got: {err:?}"
            );
            assert!(called.load(std::sync::atomic::Ordering::SeqCst));
        }
    }

    // An empty or blank id means `onnx`, and registering under a built-in id
    // replaces the built-in factory.
    #[tokio::test]
    async fn build_embedding_empty_provider_routes_to_onnx_and_builtins_can_be_overridden() {
        let mut reg = ComponentRegistry::with_builtins();
        let (stub, called) = StubEmbedding::new("onnx");
        reg.register_embedding(stub);

        for provider in ["", "   ", "onnx", "ONNX"] {
            called.store(false, std::sync::atomic::Ordering::SeqCst);
            let mut ctx = test_ctx();
            ctx.embedding.provider = provider.to_string();
            let err = embedding_error(reg.build_embedding(&ctx).await);
            assert!(
                matches!(&err, ComponentError::EmbeddingEngine(m) if m == "stub:onnx"),
                "provider {provider:?} must reach the overriding `onnx` factory, got: {err:?}"
            );
            assert!(called.load(std::sync::atomic::Ordering::SeqCst));
        }
    }

    // A recognized provider with no factory is a build without it, not a
    // typo; an unrecognized one is a typo, never a silent fallback.
    #[tokio::test]
    async fn build_embedding_tells_a_missing_provider_from_a_typo() {
        let reg = ComponentRegistry::empty();

        for provider in ["openai", "", "Bedrock"] {
            let mut ctx = test_ctx();
            ctx.embedding.provider = provider.to_string();
            let err = embedding_error(reg.build_embedding(&ctx).await);
            assert!(
                matches!(&err, ComponentError::EmbeddingEngine(m)
                    if m.contains("not available in this build")),
                "provider {provider:?} is recognized, so it must be reported as not \
                 available, got: {err:?}"
            );
        }

        let mut ctx = test_ctx();
        ctx.embedding.provider = "opnai".to_string();
        let err = embedding_error(reg.build_embedding(&ctx).await);
        let msg = err.to_string();
        assert!(
            msg.contains("unknown embedding provider 'opnai'"),
            "a typo must be reported as unknown: {msg}"
        );
        assert!(!msg.contains("not available in this build"), "{msg}");

        // The same holds with the built-ins registered.
        let reg = ComponentRegistry::with_builtins();
        let err = embedding_error(reg.build_embedding(&ctx).await);
        assert!(
            err.to_string()
                .contains("unknown embedding provider 'opnai'")
        );
    }

    // ── storage selection ────────────────────────────────────────────────

    fn storage_ctx(backend: &str, root: &std::path::Path) -> BackendBuildContext {
        let mut ctx = test_ctx();
        ctx.data_root_directory = root.to_path_buf();
        ctx.sections
            .insert("storage.backend".to_string(), serde_json::json!(backend));
        ctx
    }

    // An empty backend is the default `local`, and the id is matched trimmed
    // and case-insensitively, like every other provider id.
    #[tokio::test]
    #[allow(clippy::expect_used, reason = "test module: fail loudly")]
    async fn build_storage_defaults_to_local_and_matches_case_insensitively() {
        let reg = ComponentRegistry::with_builtins();
        let dir = tempfile::tempdir().expect("tempdir");
        for backend in ["", "   ", "local", " LOCAL "] {
            let storage = reg
                .build_storage(&storage_ctx(backend, dir.path()))
                .await
                .unwrap_or_else(|e| panic!("backend {backend:?} must build local storage: {e}"));
            assert!(
                storage.get_full_path("x").starts_with(dir.path()),
                "backend {backend:?} must be the local storage under data_root_directory"
            );
        }
    }

    // An unknown backend is a config error naming the key and the registered
    // ids — never a silent fallback to local storage.
    #[tokio::test]
    #[allow(clippy::expect_used, reason = "test module: fail loudly")]
    async fn build_storage_errors_on_an_unknown_backend() {
        let reg = ComponentRegistry::with_builtins();
        let dir = tempfile::tempdir().expect("tempdir");
        let err = match reg.build_storage(&storage_ctx("nope", dir.path())).await {
            Ok(_) => panic!("an unknown storage backend must not build"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(matches!(err, ComponentError::Config(_)), "{err:?}");
        assert!(msg.contains("storage_backend"), "{msg}");
        assert!(msg.contains("nope"), "{msg}");
        assert!(
            msg.contains("local"),
            "the registered ids are listed: {msg}"
        );
    }

    // ── ontology resolver selection ──────────────────────────────────────

    fn ontology_fixture() -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ontology/tests/fixtures/owl_terms.ttl")
            .to_string_lossy()
            .into_owned()
    }

    fn ontology_ctx(resolver: &str, file_path: &str) -> BackendBuildContext {
        let mut ctx = test_ctx();
        ctx.sections
            .insert("ontology.resolver".to_string(), serde_json::json!(resolver));
        ctx.sections.insert(
            "ontology.file_path".to_string(),
            serde_json::json!(file_path),
        );
        ctx
    }

    /// Whether `resolver` actually loaded the fixture: the no-op resolver
    /// matches nothing, the RDF one knows the fixture's `Person` class.
    #[allow(clippy::expect_used, reason = "test module: fail loudly")]
    fn knows_person(resolver: &Arc<dyn OntologyResolver>) -> bool {
        resolver
            .find_closest_match("Person", "classes")
            .expect("lookup")
            .is_some()
    }

    // No path means no ontology — the no-op resolver — whatever the resolver
    // id says, because the short-circuit precedes the resolver lookup.
    #[tokio::test]
    #[allow(clippy::expect_used, reason = "test module: fail loudly")]
    async fn build_ontology_resolver_without_a_path_is_the_noop_resolver() {
        let reg = ComponentRegistry::with_builtins();
        for (resolver, path) in [("rdflib", ""), ("bogus", ""), ("rdflib", "   ")] {
            let built = reg
                .build_ontology_resolver(&ontology_ctx(resolver, path), None)
                .await
                .unwrap_or_else(|e| panic!("{resolver:?}/{path:?} must be the no-op: {e}"));
            assert!(!knows_person(&built), "{resolver:?}/{path:?}");
        }
    }

    // A configured path loads through the resolver id, matched trimmed and
    // case-insensitively.
    #[tokio::test]
    #[allow(clippy::expect_used, reason = "test module: fail loudly")]
    async fn build_ontology_resolver_loads_the_configured_path() {
        let reg = ComponentRegistry::with_builtins();
        for resolver in ["rdflib", " RDFLIB ", ""] {
            let built = reg
                .build_ontology_resolver(&ontology_ctx(resolver, &ontology_fixture()), None)
                .await
                .unwrap_or_else(|e| panic!("resolver {resolver:?} must load: {e}"));
            assert!(knows_person(&built), "resolver {resolver:?}");
        }
    }

    // An unknown resolver id with a path is a config error naming the key.
    #[tokio::test]
    async fn build_ontology_resolver_errors_on_an_unknown_resolver() {
        let reg = ComponentRegistry::with_builtins();
        let err = match reg
            .build_ontology_resolver(&ontology_ctx("bogus", &ontology_fixture()), None)
            .await
        {
            Ok(_) => panic!("an unknown resolver id must not build"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(matches!(err, ComponentError::Config(_)), "{err:?}");
        assert!(msg.contains("ontology_resolver"), "{msg}");
        assert!(msg.contains("bogus"), "{msg}");
    }

    // The per-call path (the CLI's `--ontology-file`) overrides
    // `ontology.file_path` both ways: a path loads with nothing configured,
    // and an empty one turns a configured ontology off.
    #[tokio::test]
    #[allow(clippy::expect_used, reason = "test module: fail loudly")]
    async fn build_ontology_resolver_per_call_path_overrides_the_configured_one() {
        let reg = ComponentRegistry::with_builtins();
        let fixture = ontology_fixture();

        let built = reg
            .build_ontology_resolver(&ontology_ctx("rdflib", ""), Some(&fixture))
            .await
            .expect("the per-call path loads");
        assert!(knows_person(&built));

        let built = reg
            .build_ontology_resolver(&ontology_ctx("rdflib", &fixture), Some(""))
            .await
            .expect("an empty per-call path is the no-op");
        assert!(!knows_person(&built));
    }

    // Needs the SQLite driver (and, for the first, the built-in SQLite factory).
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn build_database_selects_by_provider_and_falls_back_to_the_url_scheme() {
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        // Default provider `sqlite` against an in-memory URL.
        reg.build_database(&ctx).await.expect("sqlite builds");

        // Empty provider: the URL scheme picks the factory.
        ctx.sections
            .insert("relational.provider".to_string(), serde_json::json!(""));
        reg.build_database(&ctx)
            .await
            .expect("scheme fallback builds");
    }

    #[tokio::test]
    async fn build_database_errors_on_unregistered_provider() {
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        ctx.sections.insert(
            "relational.provider".to_string(),
            serde_json::json!("oracle"),
        );
        let msg = match reg.build_database(&ctx).await {
            Ok(_) => panic!("oracle is not a built-in relational backend"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("oracle"),
            "message should name the provider: {msg}"
        );
    }

    // Needs the SQLite driver (and, for the first, the built-in SQLite factory).
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_registered_relational_factory_is_selected_and_brings_its_section() {
        cognee_config_core::config_section! {
            /// A custom backend's own setting.
            pub struct CustomRelationalConfig in "relational.custom" {
                /// Pool size.
                pub pool: u32 = 4 => { env: [], alias: [], secret: false, },
            }
        }
        struct Custom;
        #[async_trait::async_trait]
        impl RelationalDbFactory for Custom {
            fn provider(&self) -> &str {
                "custom"
            }
            fn config_sections(&self) -> Vec<&'static SectionSchema> {
                vec![<CustomRelationalConfig as cognee_config_core::ConfigSection>::schema()]
            }
            async fn build(
                &self,
                _ctx: &BackendBuildContext,
            ) -> Result<Arc<dyn RelationalDb>, ComponentError> {
                // Delegate to SQLite in memory: what matters is the selection.
                let db = cognee_database::connect("sqlite::memory:")
                    .await
                    .map_err(|e| ComponentError::Database(e.to_string()))?;
                Ok(Arc::new(db))
            }
        }
        let mut reg = ComponentRegistry::with_builtins();
        reg.register_relational(Arc::new(Custom));
        let schema = reg.config_schema().expect("no collisions");
        assert!(schema.key_def("relational.custom.pool").is_some());
        assert!(schema.key_def("db_provider").is_some());

        let mut ctx = test_ctx();
        ctx.sections.insert(
            "relational.provider".to_string(),
            serde_json::json!("custom"),
        );
        let db = reg.build_database(&ctx).await.expect("custom builds");
        assert_eq!(db.system_label(), "sqlite");
    }

    #[tokio::test]
    async fn session_store_defaults_to_the_relational_database() {
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        let db = reg.build_database(&ctx).await.expect("sqlite builds");
        reg.build_session_store(&ctx, Arc::clone(&db))
            .await
            .expect("relational store builds");

        // The HTTP server's historical id selects the same store.
        ctx.sections
            .insert("session.store".to_string(), serde_json::json!("seaorm"));
        reg.build_session_store(&ctx, db)
            .await
            .expect("seaorm is the relational store");
    }

    #[cfg(not(feature = "session-redis"))]
    #[tokio::test]
    async fn a_compiled_out_session_store_points_at_its_feature() {
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        let db = reg.build_database(&ctx).await.expect("sqlite builds");
        ctx.sections
            .insert("session.store".to_string(), serde_json::json!("redis"));
        let msg = match reg.build_session_store(&ctx, db).await {
            Ok(_) => panic!("redis is behind the session-redis feature"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("session-redis"), "{msg}");
        // The no-rebuild way out (a prebuilt CLI/binding) is named too.
        assert!(msg.contains("to `relational`"), "{msg}");
        // Its settings are not registered either.
        let schema = reg.config_schema().expect("schema");
        assert!(schema.key_def("cache_host").is_none());
    }

    #[cfg(feature = "session-fs")]
    #[tokio::test]
    async fn the_fs_session_store_uses_its_configured_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = ComponentRegistry::with_builtins();
        let mut ctx = test_ctx();
        let db = reg.build_database(&ctx).await.expect("sqlite builds");
        ctx.sections
            .insert("session.store".to_string(), serde_json::json!("fs"));
        ctx.sections.insert(
            "session.fs.directory".to_string(),
            serde_json::json!(dir.path().to_string_lossy()),
        );
        let store = reg.build_session_store(&ctx, db).await.expect("fs builds");
        store
            .create_qa_entry("s1", None, "q", "a", None)
            .await
            .expect("write");
        assert!(std::fs::read_dir(dir.path()).expect("dir").next().is_some());
    }

    pub(crate) fn test_ctx() -> BackendBuildContext {
        BackendBuildContext {
            sections: Default::default(),
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
                rate_limit_enabled: false,
                rate_limit_requests: 60,
                rate_limit_interval: 60,
                mock: false,
                mock_mode: Default::default(),
                max_completion_tokens: 8191,
                max_sequence_length: 512,
            },
            llm: crate::context::LlmInputs {
                api_version: String::new(),
                debug_requests: false,
                streaming: false,
                temperature: None,
                provider: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                api_key: "sk-test".to_string(),
                endpoint: String::new(),
                max_retries: 3,
                network_retries: 2,
                min_retry_seconds: 0,
                max_parallel_requests: cognee_llm::in_flight::DEFAULT_MAX_IN_FLIGHT as u32,
                request_timeout_seconds: cognee_llm::OpenAIAdapter::DEFAULT_REQUEST_TIMEOUT
                    .as_secs() as u32,
                connect_timeout_seconds: cognee_llm::OpenAIAdapter::DEFAULT_CONNECT_TIMEOUT
                    .as_secs() as u32,
                // No aggregate deadline in this fixture: these helpers build a
                // context for component wiring assertions, and a time bound would
                // be an unrelated variable in them.
                request_deadline_seconds: 0,
                rate_limit_enabled: false,
                rate_limit_requests: 60,
                rate_limit_interval: 60,
                auto_rate_limit: true,
                max_completion_tokens: cognee_llm::OpenAIAdapter::DEFAULT_MAX_COMPLETION_TOKENS,
                llm_args: serde_json::Map::new(),
                reasoning_override: None,
                structured_output_mode: cognee_llm::StructuredOutputMode::Auto,
                mock: false,
                cassette: String::new(),
                record_path: String::new(),
            },
        }
    }
}
