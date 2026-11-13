//! Per-call LLM and embedding engines: the Rust counterpart of Python's
//! `llm_config` / `embedding_config` `ContextVar`s
//! (`cognee/context_global_variables.py:36-37`, set per call at `:198-201`),
//! which `recall()` and `search()` take as arguments
//! (`cognee/api/v1/recall/recall.py:467-468`, `search/search.py:71-72`) and
//! every engine getter reads.
//!
//! Rust retrievers capture their engines when [`SearchBuilder`] builds them,
//! so a per-call engine cannot be passed down. Instead the builder hands every
//! retriever, and the orchestrator, a [`RoutedLlm`] / [`RoutedEmbedding`]: a
//! wrapper that forwards each call to the engine of the current
//! [`EngineOverride`] when one is set for that engine, and to the deployment's
//! engine otherwise. [`with_engine_override`] attaches an override to a future.
//!
//! The override is a task-local, like
//! [`cognee_models::dataset_scope`]: it follows the future wherever it runs,
//! but a `tokio::spawn` *inside* it starts a new task without it. Wrap the
//! spawned future in [`propagate`] when it calls an LLM or embedding engine.
//!
//! [`SearchBuilder`]: crate::SearchBuilder

use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::{EmbeddingEngine, EmbeddingError, EmbeddingResult};
use cognee_llm::llm_trait::StructuredOutputValidator;
use cognee_llm::{DeltaSink, GenerationOptions, GenerationResponse, Llm, LlmResult, Message};
use serde_json::Value;

tokio::task_local! {
    static ENGINE_OVERRIDE: EngineOverride;
}

/// The engines one call runs with instead of the deployment's. A `None`
/// engine is not overridden: the deployment's (or an enclosing override's)
/// serves it.
///
/// Built from a per-call `llm_config` / `embedding_config` by
/// `cognee::ComponentManager::build_engines_with_overrides`, or by hand.
#[derive(Clone, Default)]
pub struct EngineOverride {
    /// The LLM every completion, routing and session-turn call of the call uses.
    pub llm: Option<Arc<dyn Llm>>,
    /// The embedding engine every query embedding of the call uses. It must
    /// produce vectors as wide as the vector store's: see
    /// [`EngineOverride::check_embedding_dimension`].
    pub embedding: Option<Arc<dyn EmbeddingEngine>>,
}

// Hand-written: the engines are not `Debug`, and a key must never reach a log
// line. Only what identifies the engines is shown.
impl std::fmt::Debug for EngineOverride {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineOverride")
            .field("llm", &self.llm.as_ref().map(|llm| llm.model().to_string()))
            .field(
                "embedding_dimension",
                &self.embedding.as_ref().map(|e| e.dimension()),
            )
            .finish()
    }
}

/// A per-call embedding engine whose vectors are not as wide as the vector
/// store's: searching with it would fail inside the store or compare
/// unrelated vectors. Python refuses the same mismatch with
/// `EmbeddingDimensionMismatchError` (`ensure_embedding_model_matches.py:57`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "the per-call embedding_config produces {override_dimensions}-dimensional vectors, but \
     the vector store was built with a {configured_dimensions}-dimensional embedding model; \
     use a model with {configured_dimensions} dimensions or rebuild the dataset with the new one"
)]
pub struct EmbeddingDimensionMismatch {
    /// The width of the deployment's embedding model, which built the store.
    pub configured_dimensions: usize,
    /// The width of the per-call engine.
    pub override_dimensions: usize,
}

impl EngineOverride {
    /// Whether this overrides nothing.
    pub fn is_empty(&self) -> bool {
        self.llm.is_none() && self.embedding.is_none()
    }

    /// Refuse an embedding engine whose width is not `configured`, the width
    /// of the deployment's engine (which built the vector store). Without an
    /// embedding override there is nothing to check.
    pub fn check_embedding_dimension(
        &self,
        configured: usize,
    ) -> Result<(), EmbeddingDimensionMismatch> {
        match &self.embedding {
            Some(engine) if engine.dimension() != configured => Err(EmbeddingDimensionMismatch {
                configured_dimensions: configured,
                override_dimensions: engine.dimension(),
            }),
            _ => Ok(()),
        }
    }

    /// `self` over `outer`: an engine `self` does not override is `outer`'s,
    /// as a Python context sets only the config variables it was given.
    fn over(self, outer: Option<EngineOverride>) -> EngineOverride {
        let outer = outer.unwrap_or_default();
        EngineOverride {
            llm: self.llm.or(outer.llm),
            embedding: self.embedding.or(outer.embedding),
        }
    }
}

/// Run `future` with `engines` as the call's engines. Nested inside another
/// override, an engine `engines` leaves unset stays the outer one.
pub async fn with_engine_override<F: Future>(engines: EngineOverride, future: F) -> F::Output {
    let effective = engines.over(current());
    ENGINE_OVERRIDE.scope(effective, future).await
}

/// `future` under `engines` when there are any, plain otherwise: for a call
/// whose override is optional.
pub async fn maybe_with_engine_override<F: Future>(
    engines: Option<EngineOverride>,
    future: F,
) -> F::Output {
    match engines.filter(|e| !e.is_empty()) {
        Some(engines) => with_engine_override(engines, future).await,
        None => future.await,
    }
}

/// The current override, `None` outside every one.
pub fn current() -> Option<EngineOverride> {
    ENGINE_OVERRIDE.try_with(Clone::clone).ok()
}

/// The current per-call LLM, if the call overrides it.
pub fn current_llm() -> Option<Arc<dyn Llm>> {
    ENGINE_OVERRIDE.try_with(|o| o.llm.clone()).ok().flatten()
}

/// The current per-call embedding engine, if the call overrides it.
pub fn current_embedding() -> Option<Arc<dyn EmbeddingEngine>> {
    ENGINE_OVERRIDE
        .try_with(|o| o.embedding.clone())
        .ok()
        .flatten()
}

/// `future` under the caller's override, for a future that will run in
/// another task (`tokio::spawn`), which does not inherit it. Outside an
/// override the future runs plain.
pub fn propagate<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let engines = current();
    maybe_with_engine_override(engines, future)
}

/// An LLM that forwards every call to the call's override
/// ([`current_llm`]) when there is one, else to the deployment's LLM.
///
/// [`Llm::model`] borrows from the engine, which a task-local cannot lend, so
/// it always names the deployment's model; nothing on the search path reads it.
pub struct RoutedLlm {
    base: Arc<dyn Llm>,
}

impl RoutedLlm {
    /// Route over `base`, the deployment's LLM.
    pub fn new(base: Arc<dyn Llm>) -> Self {
        Self { base }
    }

    fn effective(&self) -> Arc<dyn Llm> {
        current_llm().unwrap_or_else(|| Arc::clone(&self.base))
    }
}

#[async_trait]
impl Llm for RoutedLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        self.effective().generate(messages, options).await
    }

    async fn create_structured_output_raw(
        &self,
        text_input: &str,
        system_prompt: &str,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        self.effective()
            .create_structured_output_raw(text_input, system_prompt, json_schema, options)
            .await
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        self.effective()
            .create_structured_output_with_messages_raw(messages, json_schema, options)
            .await
    }

    async fn create_structured_output_with_messages_raw_validated(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: StructuredOutputValidator<'_>,
    ) -> LlmResult<Value> {
        self.effective()
            .create_structured_output_with_messages_raw_validated(
                messages,
                json_schema,
                options,
                validator,
            )
            .await
    }

    fn model(&self) -> &str {
        self.base.model()
    }

    fn is_configured(&self) -> bool {
        self.effective().is_configured()
    }

    fn supports_streaming(&self) -> bool {
        self.effective().supports_streaming()
    }

    fn streaming_enabled(&self) -> bool {
        self.effective().streaming_enabled()
    }

    async fn generate_streaming(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
        sink: &dyn DeltaSink,
    ) -> LlmResult<GenerationResponse> {
        self.effective()
            .generate_streaming(messages, options, sink)
            .await
    }

    fn supports_function_calling(&self) -> bool {
        self.effective().supports_function_calling()
    }

    fn max_context_length(&self) -> u32 {
        self.effective().max_context_length()
    }

    fn max_completion_tokens(&self) -> u32 {
        self.effective().max_completion_tokens()
    }

    async fn transcribe_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        self.effective()
            .transcribe_image(image_bytes, mime_type, options)
            .await
    }

    fn supports_vision(&self) -> bool {
        self.effective().supports_vision()
    }
}

/// An embedding engine that forwards every call to the call's override
/// ([`current_embedding`]) when there is one, else to the deployment's engine.
///
/// An override whose width differs from the deployment engine's is refused
/// on every embedding call ([`EngineOverride::check_embedding_dimension`]),
/// so a hand-built override cannot search the store with vectors of the wrong
/// width either.
pub struct RoutedEmbedding {
    base: Arc<dyn EmbeddingEngine>,
}

impl RoutedEmbedding {
    /// Route over `base`, the deployment's embedding engine.
    pub fn new(base: Arc<dyn EmbeddingEngine>) -> Self {
        Self { base }
    }

    fn effective(&self) -> Arc<dyn EmbeddingEngine> {
        current_embedding().unwrap_or_else(|| Arc::clone(&self.base))
    }

    /// The engine to embed with, refusing an override of the wrong width.
    fn checked(&self) -> EmbeddingResult<Arc<dyn EmbeddingEngine>> {
        let engine = self.effective();
        let configured = self.base.dimension();
        if engine.dimension() != configured {
            return Err(EmbeddingError::ConfigError(
                EmbeddingDimensionMismatch {
                    configured_dimensions: configured,
                    override_dimensions: engine.dimension(),
                }
                .to_string(),
            ));
        }
        Ok(engine)
    }
}

#[async_trait]
impl EmbeddingEngine for RoutedEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        self.checked()?.embed(texts).await
    }

    async fn embed_query(&self, query: &str) -> EmbeddingResult<Vec<Vec<f32>>> {
        self.checked()?.embed_query(query).await
    }

    async fn embed_queries(&self, queries: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        self.checked()?.embed_queries(queries).await
    }

    fn dimension(&self) -> usize {
        self.effective().dimension()
    }

    fn batch_size(&self) -> usize {
        self.effective().batch_size()
    }

    fn max_concurrent_batches(&self) -> usize {
        self.effective().max_concurrent_batches()
    }

    fn max_sequence_length(&self) -> usize {
        self.effective().max_sequence_length()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// An LLM that answers with its name and counts its calls.
    struct NamedLlm {
        name: &'static str,
        configured: bool,
        calls: AtomicUsize,
    }

    impl NamedLlm {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                configured: true,
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl Llm for NamedLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(GenerationResponse {
                content: self.name.to_string(),
                model: self.name.to_string(),
                usage: None,
                finish_reason: None,
            })
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            Ok(Value::String(self.name.to_string()))
        }

        fn model(&self) -> &str {
            self.name
        }

        fn is_configured(&self) -> bool {
            self.configured
        }
    }

    /// An embedding engine of a given width whose vectors are filled with
    /// `fill`.
    struct FilledEmbedding {
        width: usize,
        fill: f32,
    }

    #[async_trait]
    impl EmbeddingEngine for FilledEmbedding {
        async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![self.fill; self.width]).collect())
        }
        fn dimension(&self) -> usize {
            self.width
        }
        fn batch_size(&self) -> usize {
            8
        }
        fn max_sequence_length(&self) -> usize {
            128
        }
    }

    fn embedding(width: usize, fill: f32) -> Arc<dyn EmbeddingEngine> {
        Arc::new(FilledEmbedding { width, fill })
    }

    async fn answer(llm: &dyn Llm) -> String {
        llm.generate(Vec::new(), None).await.unwrap().content
    }

    #[tokio::test]
    async fn the_routed_llm_uses_the_override_only_inside_it() {
        let base = NamedLlm::new("base");
        let per_call = NamedLlm::new("per-call");
        let routed = RoutedLlm::new(base.clone());

        assert_eq!(answer(&routed).await, "base");
        let engines = EngineOverride {
            llm: Some(per_call.clone()),
            embedding: None,
        };
        let inside = with_engine_override(engines, answer(&routed)).await;
        assert_eq!(inside, "per-call");
        assert_eq!(answer(&routed).await, "base");
        assert_eq!(base.calls.load(Ordering::SeqCst), 2);
        assert_eq!(per_call.calls.load(Ordering::SeqCst), 1);
        // The deployment's model name, whatever the override.
        assert_eq!(routed.model(), "base");
    }

    #[tokio::test]
    async fn is_configured_follows_the_effective_llm() {
        let routed = RoutedLlm::new(NamedLlm::new("base"));
        let unconfigured: Arc<dyn Llm> = Arc::new(cognee_llm::UnconfiguredLlm::new("openai"));
        assert!(routed.is_configured());
        let engines = EngineOverride {
            llm: Some(unconfigured),
            embedding: None,
        };
        let inside = with_engine_override(engines, async { routed.is_configured() }).await;
        assert!(!inside);
    }

    #[tokio::test]
    async fn the_routed_embedding_uses_the_override_only_inside_it() {
        let routed = RoutedEmbedding::new(embedding(2, 0.0));
        let engines = EngineOverride {
            llm: None,
            embedding: Some(embedding(2, 1.0)),
        };
        let inside = with_engine_override(engines, routed.embed_query("q"))
            .await
            .unwrap();
        assert_eq!(inside, vec![vec![1.0, 1.0]]);
        assert_eq!(routed.embed_query("q").await.unwrap(), vec![vec![0.0, 0.0]]);
    }

    #[tokio::test]
    async fn an_override_of_the_wrong_width_is_refused_before_it_embeds() {
        let routed = RoutedEmbedding::new(embedding(2, 0.0));
        let engines = EngineOverride {
            llm: None,
            embedding: Some(embedding(3, 1.0)),
        };
        assert_eq!(
            engines.check_embedding_dimension(2),
            Err(EmbeddingDimensionMismatch {
                configured_dimensions: 2,
                override_dimensions: 3,
            })
        );
        assert_eq!(engines.check_embedding_dimension(3), Ok(()));
        let err = with_engine_override(engines, routed.embed_query("q"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("3-dimensional"), "{err}");
    }

    #[tokio::test]
    async fn a_nested_override_keeps_the_outer_engines_it_leaves_unset() {
        let outer = EngineOverride {
            llm: Some(NamedLlm::new("outer")),
            embedding: Some(embedding(2, 5.0)),
        };
        let inner = EngineOverride {
            llm: Some(NamedLlm::new("inner")),
            embedding: None,
        };
        let (llm, embedding_fill) = with_engine_override(outer, async {
            with_engine_override(inner, async {
                let llm = current_llm().unwrap().model().to_string();
                let fill = current_embedding().unwrap().embed(&["x"]).await.unwrap()[0][0];
                (llm, fill)
            })
            .await
        })
        .await;
        assert_eq!(llm, "inner");
        assert_eq!(embedding_fill, 5.0);
    }

    #[tokio::test]
    async fn a_spawn_inside_an_override_needs_propagate() {
        let engines = EngineOverride {
            llm: Some(NamedLlm::new("per-call")),
            embedding: None,
        };
        let (bare, propagated) = with_engine_override(engines, async {
            let bare = tokio::spawn(async { current_llm().is_some() })
                .await
                .unwrap();
            let propagated = tokio::spawn(propagate(async {
                current_llm().map(|llm| llm.model().to_string())
            }))
            .await
            .unwrap();
            (bare, propagated)
        })
        .await;
        assert!(!bare);
        assert_eq!(propagated.as_deref(), Some("per-call"));
    }

    #[tokio::test]
    async fn an_empty_override_sets_nothing() {
        let seen = maybe_with_engine_override(Some(EngineOverride::default()), async {
            current().is_some()
        })
        .await;
        assert!(!seen);
    }

    #[test]
    fn debug_shows_no_more_than_the_model_and_the_width() {
        let engines = EngineOverride {
            llm: Some(NamedLlm::new("per-call")),
            embedding: Some(embedding(4, 0.0)),
        };
        assert_eq!(
            format!("{engines:?}"),
            r#"EngineOverride { llm: Some("per-call"), embedding_dimension: Some(4) }"#
        );
    }
}
