//! [`ComponentRegistry`] — the pluggable provider → factory map shared by the
//! `ComponentManager` (cognee) and the HTTP server's standalone wiring.

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
        // construction, and only after a real lookup miss -- so a closed
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
         Closed adapters (e.g. qdrant, litert) must be registered via \
         ComponentRegistry::register_* at the binary entry point.",
        supported.join(", ")
    )
}

#[cfg(test)]
mod tests {
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

    // The fallback fires only on a real lookup miss, so a closed `lancedb`
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
        ctx.llm.aws.bearer_token = Some("test-token".to_string());
        ctx.llm.aws.region = Some("us-east-1".to_string());

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
        ctx.llm.aws.bearer_token = Some("test-token".to_string());
        ctx.llm.aws.region = Some("Not A Region".to_string());

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
                rate_limit_enabled: false,
                rate_limit_requests: 60,
                rate_limit_interval: 60,
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
                aws: crate::context::AwsInputs::default(),
            },
            llm: crate::context::LlmInputs {
                provider: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                api_key: "sk-test".to_string(),
                endpoint: String::new(),
                anthropic_base_url: None,
                max_retries: 3,
                min_retry_seconds: 0,
                max_parallel_requests: cognee_llm::in_flight::DEFAULT_MAX_IN_FLIGHT as u32,
                rate_limit_enabled: false,
                rate_limit_requests: 60,
                rate_limit_interval: 60,
                auto_rate_limit: true,
                max_completion_tokens: cognee_llm::OpenAIAdapter::DEFAULT_MAX_COMPLETION_TOKENS,
                llm_args: serde_json::Map::new(),
                api_version: String::new(),
                reasoning_override: None,
                mock: false,
                cassette: String::new(),
                record_path: String::new(),
                aws: crate::context::AwsInputs::default(),
            },
        }
    }
}
