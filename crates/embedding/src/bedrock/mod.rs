//! AWS Bedrock embedding engine (feature `bedrock`).
//!
//! [`BedrockEmbeddingEngine`] implements [`EmbeddingEngine`] against Bedrock's
//! **InvokeModel** API — `POST {endpoint}/model/{modelId}/invoke` — which is
//! where `litellm.aembedding` routes Bedrock embeddings. Converse is the chat
//! API only and is never used here (plan §1.5).
//!
//! The shared AWS plumbing (env resolution, the region and endpoint chains, the
//! credential ladder, SigV4 and the transport seam) is **not** duplicated: it
//! lives in [`cognee_llm::adapters::bedrock::aws`], which is homed in
//! `cognee-llm` precisely so both this engine and the chat adapter share one
//! implementation. The wire shapes live beside this file in [`titan`] and
//! [`cohere`].
//!
//! # Three rules that fail silently if forgotten
//!
//! 1. **The request URL keeps the original, un-normalised model id** (plan
//!    §1.4.1). Normalisation picks the request family and nothing else.
//! 2. **`EmbeddingConfig::endpoint` is not the Bedrock api_base.** It defaults
//!    to `https://api.openai.com/v1` and nothing clears it when the provider is
//!    switched, so passing it through blindly would POST Bedrock bodies at
//!    OpenAI — see `bedrock_api_base`.
//! 3. **Only Titan v2 normalises server-side.** The [`EmbeddingEngine`]
//!    contract promises unit vectors, so g1, the multimodal variant and Cohere
//!    are normalised client-side.
//!
//! # Auth
//!
//! cognee passes no AWS credentials on the embedding path (plan §1.5): the
//! bearer `api_key` short-circuits to `Authorization: Bearer …` with no SigV4
//! and no credential lookup, and when it is absent the ambient credential
//! ladder runs. An absent key is a supported configuration, not an error.
//!
//! # Not implemented, on purpose
//!
//! litellm's `async_invoke/*` embedding branch keys on an explicit
//! `async_invoke/` model prefix (`embed/embedding.py:392-394`). cognee ships no
//! model in that family on either SDK, so the row is informational and this
//! engine does not implement it (plan §1.5).

// Both modules carry their own `//!` docs; see the note in `lib.rs` for why
// they are not repeated as outer docs here.
pub mod cohere;
pub mod titan;

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use cognee_llm::adapters::bedrock::aws::transport::{BedrockTransport, ReqwestBedrockTransport};
use cognee_llm::adapters::bedrock::{aws, converse, model_id};
use cognee_llm::error::LlmError;
use futures::stream::{self, StreamExt, TryStreamExt};
use tracing::debug;

use crate::config::EmbeddingConfig;
use crate::engine::EmbeddingEngine;
use crate::error::{EmbeddingError, EmbeddingResult};
use crate::utils::{handle_embedding_response, l2_normalize, sanitize_embedding_inputs};

/// Maximum number of InvokeModel requests in flight from a single `embed` call.
///
/// The Titan families take one text per request (litellm's
/// `_single_func_embeddings` loop), so a 36-text batch is 36 requests; this
/// bounds them against Bedrock's per-account throttle. Mirrors the
/// OpenAI-compatible engine's `MAX_CONCURRENT_BATCHES`.
const MAX_CONCURRENT_REQUESTS: usize = 8;

/// Per-request HTTP timeout, matching the sibling HTTP embedding engines.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Default wall-clock budget for retrying transient failures, matching the
/// OpenAI-compatible and Ollama engines.
const DEFAULT_RETRY_BUDGET: Duration = Duration::from_secs(128);

/// The Bedrock embedding request families cognee can reach.
///
/// Selected from the **normalised** model id (`bedrock/` prefix stripped, ARN
/// unwrapped, suffixes removed) — the un-normalised id is what goes in the URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BedrockEmbeddingFamily {
    /// `amazon.titan-embed-text-v1` — `{"inputText": …}`, no server-side
    /// normalisation.
    TitanTextG1,
    /// `amazon.titan-embed-text-v2:0` — accepts `dimensions` / `normalize`.
    TitanTextV2,
    /// `amazon.titan-embed-image-v1` — the multimodal variant, driven with text
    /// only here.
    TitanMultimodal,
    /// `cohere.embed-*` — the one family that batches.
    Cohere,
}

impl BedrockEmbeddingFamily {
    /// Pick the family for `model`.
    ///
    /// Mirrors litellm's prefix dispatch in `bedrock/embed/embedding.py`. An
    /// id outside the four families is a configuration error rather than a
    /// guess: sending a Titan body to an unknown model would fail at the first
    /// vector write instead of at construction.
    pub fn for_model(model: &str) -> EmbeddingResult<Self> {
        let base = model_id::base_model(model).to_ascii_lowercase();
        if base.contains("titan-embed-image") {
            Ok(Self::TitanMultimodal)
        } else if base.starts_with("amazon.titan-embed-text-v2") {
            Ok(Self::TitanTextV2)
        } else if base.starts_with("amazon.titan-embed") {
            Ok(Self::TitanTextG1)
        } else if base.starts_with("cohere.embed") {
            Ok(Self::Cohere)
        } else {
            Err(EmbeddingError::ConfigError(format!(
                "unsupported Bedrock embedding model '{model}' (normalised: '{base}'). \
                 Supported families: amazon.titan-embed-text-v1, \
                 amazon.titan-embed-text-v2:0, amazon.titan-embed-image-v1, cohere.embed-*."
            )))
        }
    }

    /// Whether Bedrock returns a unit-norm vector for this family, so the
    /// client-side L2 pass can be skipped.
    ///
    /// Only Titan v2 does, and only because we send `normalize: true`. Cohere's
    /// `float` embeddings are **not** documented as unit-norm (checked against
    /// the Cohere embed API reference, which specifies neither a norm nor a
    /// range), so they are normalised here.
    fn normalizes_server_side(self) -> bool {
        matches!(self, Self::TitanTextV2)
    }
}

/// Embedding engine that calls Bedrock's InvokeModel endpoint.
///
/// Built by [`EmbeddingConfig::create_engine`] when the provider is
/// [`crate::EmbeddingProvider::Bedrock`]. See the module docs for the wire spec
/// it implements.
pub struct BedrockEmbeddingEngine {
    /// The model id **exactly as configured** — this is what goes in the
    /// request URL, cross-region prefix and ARN wrapper included.
    model: String,
    /// Family resolved once from the §1.4.1-normalised id.
    family: BedrockEmbeddingFamily,
    /// Resolved runtime endpoint, without a trailing slash.
    endpoint: String,
    /// Resolved AWS region — diagnostics only; the transport holds its own copy
    /// for signing.
    region: String,
    transport: Arc<dyn BedrockTransport>,
    dimensions: usize,
    batch_size: usize,
    max_sequence_length: usize,
    retry_budget: Duration,
}

impl BedrockEmbeddingEngine {
    /// Build an engine from `config`.
    ///
    /// Runs the same chains as
    /// [`cognee_llm::adapters::bedrock::BedrockAdapter::new`]: region → endpoint
    /// → auth. `config.api_key` is the Bedrock API key; when set it
    /// short-circuits to a bearer header with no SigV4 and no credential
    /// lookup, and when unset the ambient ladder runs (plan §1.5).
    ///
    /// # Errors
    ///
    /// * [`EmbeddingError::ConfigError`] — empty or unsupported model id, an
    ///   unresolvable region or credential chain, or a `reqwest` client that
    ///   cannot be built.
    pub async fn new(config: &EmbeddingConfig) -> EmbeddingResult<Self> {
        let model = config.model.trim().to_string();
        if model.is_empty() {
            return Err(EmbeddingError::ConfigError(
                "Bedrock embedding requires EMBEDDING_MODEL to be set (e.g. \
                 amazon.titan-embed-text-v2:0)"
                    .to_string(),
            ));
        }
        let family = BedrockEmbeddingFamily::for_model(&model)?;

        let settings = config.aws.resolve();
        let region = aws::region::resolve_region(&settings, Some(&model))
            .await
            .map_err(embedding_error_from_llm)?;
        let endpoint =
            aws::endpoint::resolve_endpoint(bedrock_api_base(config), &settings, &region);
        let auth = aws::credentials::resolve_auth(config.api_key.as_deref(), &settings, &region)
            .await
            .map_err(embedding_error_from_llm)?;

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| {
                EmbeddingError::ConfigError(format!("Failed to build HTTP client: {e}"))
            })?;
        let transport = Arc::new(ReqwestBedrockTransport::new(client, auth, region.clone()));

        debug!(
            model = model.as_str(),
            family = ?family,
            region = region.as_str(),
            endpoint = endpoint.as_str(),
            dimensions = config.dimensions,
            "built Bedrock embedding engine",
        );

        Ok(Self {
            model,
            family,
            endpoint,
            region,
            transport,
            dimensions: config.dimensions,
            batch_size: config.batch_size,
            max_sequence_length: config.max_completion_tokens,
            retry_budget: DEFAULT_RETRY_BUDGET,
        })
    }

    /// Set the wall-clock budget for retrying transient failures
    /// (rate limits, 5xx, network errors). [`Duration::ZERO`] disables retry.
    pub fn with_retry_budget(mut self, budget: Duration) -> Self {
        self.retry_budget = budget;
        self
    }

    /// The request family selected for the configured model.
    pub fn family(&self) -> BedrockEmbeddingFamily {
        self.family
    }

    /// The resolved AWS region.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// The resolved runtime endpoint.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// The InvokeModel URL: `{endpoint}/model/{modelId}/invoke`.
    ///
    /// `modelId` is the **original** id, percent-encoded as one path segment by
    /// the same helper the Converse path uses — that is what makes `…-v2:0` and
    /// an inference-profile ARN usable here.
    fn invoke_url(&self) -> String {
        format!(
            "{}/model/{}/invoke",
            self.endpoint.trim_end_matches('/'),
            converse::encode_model_id(model_id::wire_model_id(&self.model))
        )
    }

    /// POST `body` once and return the raw response body.
    async fn post_once(&self, body: Vec<u8>) -> EmbeddingResult<Vec<u8>> {
        let response = self
            .transport
            .post_json(&self.invoke_url(), body)
            .await
            .map_err(embedding_error_from_llm)?;

        if !response.status.is_success() {
            return Err(map_http_error(
                response.status.as_u16(),
                &response.body_lossy(),
            ));
        }
        Ok(response.body)
    }

    /// POST with exponential-jitter retry on transient errors, mirroring the
    /// sibling engines: the wait starts at 2 s and doubles up to 128 s, plus a
    /// uniform jitter in `[0, wait)`, for up to [`Self::retry_budget`] total.
    async fn post_with_retry(&self, body: Vec<u8>) -> EmbeddingResult<Vec<u8>> {
        let start = Instant::now();
        let mut wait_secs = 2u64;
        loop {
            match self.post_once(body.clone()).await {
                Ok(response) => return Ok(response),
                Err(e) if is_retryable(&e) && start.elapsed() < self.retry_budget => {
                    let jitter = rand::random::<u64>() % wait_secs;
                    tokio::time::sleep(Duration::from_secs(wait_secs + jitter)).await;
                    wait_secs = (wait_secs * 2).min(128);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Serialise `request`, POST it and hand back the raw response body.
    async fn invoke<T: serde::Serialize>(&self, request: &T) -> EmbeddingResult<Vec<u8>> {
        let body = serde_json::to_vec(request).map_err(|e| {
            EmbeddingError::ApiError(format!("Failed to serialize the Bedrock request: {e}"))
        })?;
        self.post_with_retry(body).await
    }

    /// One Titan request for one text.
    async fn embed_titan_one(&self, text: &str) -> EmbeddingResult<Vec<f32>> {
        let body = match self.family {
            BedrockEmbeddingFamily::TitanTextG1 => {
                self.invoke(&titan::TitanTextRequest::g1(text)).await?
            }
            BedrockEmbeddingFamily::TitanTextV2 => {
                // `normalize: true` is what lets the client-side L2 pass be
                // skipped for this family — the two must move together.
                self.invoke(&titan::TitanTextRequest::v2(
                    text,
                    Some(self.dimensions),
                    Some(true),
                ))
                .await?
            }
            BedrockEmbeddingFamily::TitanMultimodal => {
                self.invoke(&titan::TitanMultimodalRequest::text(text, self.dimensions))
                    .await?
            }
            BedrockEmbeddingFamily::Cohere => {
                return Err(EmbeddingError::ConfigError(
                    "the Cohere family batches; it must not reach the Titan per-text loop"
                        .to_string(),
                ));
            }
        };
        titan::parse_response(&body)
    }

    /// The Titan `_single_func_embeddings` loop: one request per text, bounded
    /// concurrency, results reassembled **in input order**.
    async fn embed_titan(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let requests: Vec<_> = texts
            .iter()
            .enumerate()
            .map(
                |(index, text)| async move { self.embed_titan_one(text).await.map(|v| (index, v)) },
            )
            .collect();

        // `try_collect` over `buffer_unordered` aborts on the first failure —
        // cancelling in-flight retries instead of waiting them out — and the
        // index restores input order afterwards.
        let mut indexed: Vec<(usize, Vec<f32>)> = stream::iter(requests)
            .buffer_unordered(MAX_CONCURRENT_REQUESTS)
            .try_collect()
            .await?;
        indexed.sort_by_key(|(index, _)| *index);
        Ok(indexed.into_iter().map(|(_, vector)| vector).collect())
    }

    /// Cohere batches every text into one request.
    async fn embed_cohere(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let body = self
            .invoke(&cohere::CohereEmbeddingRequest::search_document(texts))
            .await?;
        let vectors = cohere::parse_response(&body)?;
        if vectors.len() != texts.len() {
            return Err(EmbeddingError::ApiError(format!(
                "Bedrock returned {} embeddings for {} texts",
                vectors.len(),
                texts.len()
            )));
        }
        Ok(vectors)
    }
}

#[async_trait]
impl EmbeddingEngine for BedrockEmbeddingEngine {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let sanitized = sanitize_embedding_inputs(texts);
        let sanitized: Vec<&str> = sanitized.iter().map(|c| c.as_ref()).collect();

        let vectors = match self.family {
            BedrockEmbeddingFamily::Cohere => self.embed_cohere(&sanitized).await?,
            _ => self.embed_titan(&sanitized).await?,
        };

        // The trait contract is "every returned embedding is L2-normalised".
        let vectors: Vec<Vec<f32>> = if self.family.normalizes_server_side() {
            vectors
        } else {
            vectors.iter().map(|v| l2_normalize(v)).collect()
        };

        Ok(handle_embedding_response(texts, vectors, self.dimensions))
    }

    fn dimension(&self) -> usize {
        self.dimensions
    }

    /// `EmbeddingConfig::batch_size` verbatim.
    ///
    /// The Titan families issue one request per text regardless, so this is a
    /// caller-facing chunk size rather than a wire batch size — reporting `1`
    /// would make every caller chunk to single texts and lose the bounded
    /// concurrency in `BedrockEmbeddingEngine::embed_titan`. Cohere really
    /// does batch, so the configured value is the honest answer for it.
    fn batch_size(&self) -> usize {
        self.batch_size
    }

    fn max_sequence_length(&self) -> usize {
        self.max_sequence_length
    }
}

/// Hand-written so the resolved auth (held by the transport) can never reach a
/// log line through `{:?}`; only the routing decisions are shown.
impl std::fmt::Debug for BedrockEmbeddingEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BedrockEmbeddingEngine")
            .field("model", &self.model)
            .field("family", &self.family)
            .field("region", &self.region)
            .field("endpoint", &self.endpoint)
            .field("dimensions", &self.dimensions)
            .field("batch_size", &self.batch_size)
            .finish_non_exhaustive()
    }
}

// ─── Endpoint guard ───────────────────────────────────────────────────────────

/// The Bedrock `api_base` to use, or `None` to let
/// `AWS_BEDROCK_RUNTIME_ENDPOINT` / the regional default decide.
///
/// [`EmbeddingConfig::default`] sets `endpoint` to `https://api.openai.com/v1`
/// off Android and **nothing clears it** when the provider is switched to
/// bedrock (`EMBEDDING_ENDPOINT` merely overrides it). Feeding that through as
/// the Bedrock api_base would POST InvokeModel bodies at OpenAI, so any
/// `api.openai.com` endpoint is treated as "not configured for Bedrock" — the
/// same class of trap `LlmInputs::anthropic_base_url` exists to avoid.
fn bedrock_api_base(config: &EmbeddingConfig) -> Option<&str> {
    config
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
        .filter(|endpoint| !is_openai_host(endpoint))
}

/// Whether `endpoint`'s host is OpenAI's.
fn is_openai_host(endpoint: &str) -> bool {
    let authority = endpoint
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = authority.split(['/', ':', '?']).next().unwrap_or(authority);
    host.eq_ignore_ascii_case("api.openai.com")
}

// ─── Error mapping ────────────────────────────────────────────────────────────

/// Whether `error` is worth another attempt. Matches the sibling engines: only
/// [`EmbeddingError::HttpError`] is transient.
fn is_retryable(error: &EmbeddingError) -> bool {
    matches!(error, EmbeddingError::HttpError(_))
}

/// Map a Bedrock HTTP failure onto the embedding taxonomy.
///
/// The exception-name rules — in particular that **Bedrock reports throttling
/// as HTTP 400 `ThrottlingException` as well as 429** — are not re-derived
/// here: they live in
/// [`cognee_llm::adapters::bedrock::converse::map_error`] and stay in one
/// place. Only the projection onto [`EmbeddingError`] is local, and 5xx is
/// decided by status first because the LLM taxonomy folds server errors into
/// its generic `ApiError`, which would otherwise read as terminal.
fn map_http_error(status: u16, body: &str) -> EmbeddingError {
    if status >= 500 {
        return EmbeddingError::HttpError(format!("HTTP {status}: {body}"));
    }
    embedding_error_from_llm(converse::map_error(status, body))
}

/// Project an [`LlmError`] from the shared AWS module onto [`EmbeddingError`].
///
/// Rate limits, timeouts and network failures are transient
/// ([`EmbeddingError::HttpError`], which the retry loop acts on); configuration
/// and credential failures are [`EmbeddingError::ConfigError`]; everything else
/// — other 4xx and unexpected response shapes — is a terminal
/// [`EmbeddingError::ApiError`].
fn embedding_error_from_llm(error: LlmError) -> EmbeddingError {
    match error {
        LlmError::RateLimitExceeded(message)
        | LlmError::Timeout(message)
        | LlmError::NetworkError(message) => EmbeddingError::HttpError(message),
        LlmError::ConfigError(message) | LlmError::FeatureNotSupported(message) => {
            EmbeddingError::ConfigError(message)
        }
        other => EmbeddingError::ApiError(other.to_string()),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::provider::EmbeddingProvider;
    use crate::utils::compute_norm;

    /// A config whose region is pinned and whose api_key is set, so neither the
    /// ambient region chain nor the credential ladder is consulted (both would
    /// reach the filesystem / IMDS on a developer machine).
    fn config_for(server_url: &str, model: &str, dimensions: usize) -> EmbeddingConfig {
        let mut config = EmbeddingConfig {
            provider: EmbeddingProvider::Bedrock,
            model: model.to_string(),
            dimensions,
            endpoint: Some(server_url.to_string()),
            api_key: Some("bedrock-api-key".to_string()),
            batch_size: 36,
            ..EmbeddingConfig::default()
        };
        config.aws.region = Some("us-east-1".to_string());
        config
    }

    async fn engine_for(
        server_url: &str,
        model: &str,
        dimensions: usize,
    ) -> BedrockEmbeddingEngine {
        BedrockEmbeddingEngine::new(&config_for(server_url, model, dimensions))
            .await
            .expect("engine builds with a pinned region and a bearer key")
            // Keep the round-trip tests fast: a transient failure must surface
            // immediately instead of sleeping through the 128 s budget.
            .with_retry_budget(Duration::ZERO)
    }

    // ── Family selection ─────────────────────────────────────────────────────

    #[test]
    fn family_selection_uses_the_normalised_id() {
        for (model, expected) in [
            (
                "amazon.titan-embed-text-v1",
                BedrockEmbeddingFamily::TitanTextG1,
            ),
            (
                "amazon.titan-embed-text-v2:0",
                BedrockEmbeddingFamily::TitanTextV2,
            ),
            (
                "amazon.titan-embed-image-v1",
                BedrockEmbeddingFamily::TitanMultimodal,
            ),
            ("cohere.embed-english-v3", BedrockEmbeddingFamily::Cohere),
            // Normalisation is what makes these resolve at all.
            (
                "bedrock/amazon.titan-embed-text-v2:0",
                BedrockEmbeddingFamily::TitanTextV2,
            ),
            (
                "eu.amazon.titan-embed-text-v1",
                BedrockEmbeddingFamily::TitanTextG1,
            ),
        ] {
            assert_eq!(
                BedrockEmbeddingFamily::for_model(model).expect("known family"),
                expected,
                "model {model}"
            );
        }
    }

    #[test]
    fn an_unknown_family_is_a_config_error() {
        let err = BedrockEmbeddingFamily::for_model("anthropic.claude-3-5-sonnet-20240620-v1:0")
            .expect_err("not an embedding model");
        assert!(matches!(err, EmbeddingError::ConfigError(_)), "{err:?}");
    }

    // ── Endpoint guard ───────────────────────────────────────────────────────

    #[test]
    fn the_openai_default_endpoint_is_never_adopted_as_the_bedrock_api_base() {
        let config = EmbeddingConfig::default();
        assert_eq!(
            config.endpoint.as_deref(),
            Some("https://api.openai.com/v1"),
            "guard premise: the default config really does carry the OpenAI endpoint",
        );
        assert_eq!(
            bedrock_api_base(&config),
            None,
            "the OpenAI default must not become the Bedrock api_base",
        );

        // …and the endpoint chain therefore falls through to the regional default.
        let settings = cognee_llm::adapters::bedrock::aws::env::AwsInputs::default().resolve();
        assert_eq!(
            aws::endpoint::resolve_endpoint(bedrock_api_base(&config), &settings, "us-east-1"),
            "https://bedrock-runtime.us-east-1.amazonaws.com",
        );
    }

    /// The guard has to be *wired into the constructor*, not merely available.
    /// Without this case, replacing `bedrock_api_base(config)` in
    /// [`BedrockEmbeddingEngine::new`] with a bare `config.endpoint` leaves
    /// every other test in this module green while the engine POSTs
    /// InvokeModel bodies at api.openai.com.
    #[tokio::test]
    async fn the_constructor_applies_the_openai_endpoint_guard() {
        let mut config = EmbeddingConfig {
            provider: EmbeddingProvider::Bedrock,
            model: "amazon.titan-embed-text-v2:0".to_string(),
            dimensions: 1024,
            api_key: Some("bedrock-api-key".to_string()),
            ..EmbeddingConfig::default()
        };
        config.aws.region = Some("us-east-1".to_string());
        assert_eq!(
            config.endpoint.as_deref(),
            Some("https://api.openai.com/v1"),
            "guard premise: the default config really does carry the OpenAI endpoint",
        );

        let engine = BedrockEmbeddingEngine::new(&config)
            .await
            .expect("builds with a pinned region and a bearer key");

        assert!(
            !is_openai_host(engine.endpoint()),
            "the OpenAI default leaked into the Bedrock runtime endpoint: {}",
            engine.endpoint(),
        );
        // The exact fallback, unless the developer's own environment overrides
        // the runtime endpoint — the chain in `aws::endpoint` reads it.
        if std::env::var_os("AWS_BEDROCK_RUNTIME_ENDPOINT").is_none() {
            assert_eq!(
                engine.endpoint(),
                "https://bedrock-runtime.us-east-1.amazonaws.com",
            );
            assert_eq!(
                engine.invoke_url(),
                "https://bedrock-runtime.us-east-1.amazonaws.com\
                 /model/amazon.titan-embed-text-v2%3A0/invoke",
            );
        }
    }

    #[test]
    fn an_explicit_non_openai_endpoint_is_passed_through() {
        let config = EmbeddingConfig {
            endpoint: Some("https://vpce-123.bedrock-runtime.us-east-1.vpce.amazonaws.com".into()),
            ..EmbeddingConfig::default()
        };
        assert_eq!(
            bedrock_api_base(&config),
            Some("https://vpce-123.bedrock-runtime.us-east-1.vpce.amazonaws.com"),
        );
    }

    #[test]
    fn a_blank_endpoint_falls_through_to_the_regional_default() {
        let config = EmbeddingConfig {
            endpoint: Some("   ".to_string()),
            ..EmbeddingConfig::default()
        };
        assert_eq!(bedrock_api_base(&config), None);
    }

    #[test]
    fn is_openai_host_only_matches_the_openai_host() {
        assert!(is_openai_host("https://api.openai.com/v1"));
        assert!(is_openai_host("https://API.OpenAI.com"));
        assert!(!is_openai_host(
            "https://bedrock-runtime.us-east-1.amazonaws.com"
        ));
        assert!(!is_openai_host("http://127.0.0.1:1234"));
    }

    // ── Error mapping ────────────────────────────────────────────────────────

    #[test]
    fn a_400_throttling_exception_is_transient() {
        let err = map_http_error(
            400,
            r#"{"__type":"ThrottlingException","message":"slow down"}"#,
        );
        assert!(
            matches!(err, EmbeddingError::HttpError(_)),
            "Bedrock reports throttling as a 400 too — it must stay retryable: {err:?}"
        );
        assert!(is_retryable(&err));
    }

    #[test]
    fn a_plain_400_validation_exception_is_terminal() {
        let err = map_http_error(
            400,
            r#"{"__type":"ValidationException","message":"bad body"}"#,
        );
        assert!(matches!(err, EmbeddingError::ApiError(_)), "{err:?}");
        assert!(!is_retryable(&err));
    }

    #[test]
    fn a_503_is_transient() {
        let err = map_http_error(503, "ServiceUnavailableException");
        assert!(matches!(err, EmbeddingError::HttpError(_)), "{err:?}");
    }

    #[test]
    fn a_403_is_terminal() {
        let err = map_http_error(403, r#"{"__type":"AccessDeniedException"}"#);
        assert!(matches!(err, EmbeddingError::ApiError(_)), "{err:?}");
    }

    #[test]
    fn credential_failures_become_config_errors() {
        assert!(matches!(
            embedding_error_from_llm(LlmError::ConfigError("no credentials".into())),
            EmbeddingError::ConfigError(_)
        ));
    }

    // ── URL construction ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn the_invoke_url_keeps_the_un_normalised_model_id() {
        let engine = engine_for(
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            "eu.amazon.titan-embed-text-v2:0",
            1024,
        )
        .await;
        assert_eq!(
            engine.invoke_url(),
            "https://bedrock-runtime.us-east-1.amazonaws.com\
             /model/eu.amazon.titan-embed-text-v2%3A0/invoke",
            "the cross-region prefix stays and `:` is percent-encoded",
        );
        assert_eq!(engine.family(), BedrockEmbeddingFamily::TitanTextV2);
        assert_eq!(engine.region(), "us-east-1");
    }

    // ── HTTP round-trips ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn titan_g1_sends_one_request_per_text_and_keeps_input_order() {
        let mut server = mockito::Server::new_async().await;
        let alpha = server
            .mock("POST", "/model/eu.amazon.titan-embed-text-v1/invoke")
            .match_header("authorization", "Bearer bedrock-api-key")
            .match_header("x-amz-date", mockito::Matcher::Missing)
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"inputText": "alpha"}),
            ))
            .with_status(200)
            .with_body(r#"{"embedding":[3.0,0.0],"inputTextTokenCount":1}"#)
            .expect(1)
            .create_async()
            .await;
        let beta = server
            .mock("POST", "/model/eu.amazon.titan-embed-text-v1/invoke")
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"inputText": "beta"}),
            ))
            .with_status(200)
            .with_body(r#"{"embedding":[0.0,4.0],"inputTextTokenCount":1}"#)
            .expect(1)
            .create_async()
            .await;

        // The un-normalised id (with its `eu.` prefix) is what the URL carries.
        let engine = engine_for(&server.url(), "eu.amazon.titan-embed-text-v1", 2).await;
        let out = engine.embed(&["alpha", "beta"]).await.expect("embeds");

        assert_eq!(
            out,
            vec![vec![1.0, 0.0], vec![0.0, 1.0]],
            "results follow input order and are L2-normalised client-side",
        );
        alpha.assert_async().await;
        beta.assert_async().await;
    }

    #[tokio::test]
    async fn the_bearer_path_signs_nothing() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/model/amazon.titan-embed-text-v1/invoke")
            .match_header("authorization", "Bearer bedrock-api-key")
            .match_header("content-type", "application/json")
            .match_header("x-amz-date", mockito::Matcher::Missing)
            .match_header("x-amz-security-token", mockito::Matcher::Missing)
            .with_status(200)
            .with_body(r#"{"embedding":[1.0,0.0]}"#)
            .expect(1)
            .create_async()
            .await;

        let engine = engine_for(&server.url(), "amazon.titan-embed-text-v1", 2).await;
        engine.embed(&["alpha"]).await.expect("embeds");

        mock.assert_async().await;
        // A SigV4 `Authorization` would start with AWS4-HMAC-SHA256; the header
        // matcher above pins it to the bearer form, and no signature-only
        // headers may appear alongside it.
    }

    #[tokio::test]
    async fn titan_v2_asks_bedrock_to_normalize_and_skips_the_client_pass() {
        let mut server = mockito::Server::new_async().await;
        // The body assertion is the point: v2 must send `normalize: true`.
        let mock = server
            .mock("POST", "/model/amazon.titan-embed-text-v2%3A0/invoke")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "inputText": "alpha",
                "dimensions": 2,
                "normalize": true,
            })))
            .with_status(200)
            // Deliberately not unit-norm: if the engine also normalised
            // client-side, this would come back as [1.0, 0.0].
            .with_body(r#"{"embedding":[3.0,0.0]}"#)
            .expect(1)
            .create_async()
            .await;

        let engine = engine_for(&server.url(), "amazon.titan-embed-text-v2:0", 2).await;
        let out = engine.embed(&["alpha"]).await.expect("embeds");

        assert_eq!(
            out,
            vec![vec![3.0, 0.0]],
            "v2 normalises server-side, so the response is returned verbatim",
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn titan_multimodal_normalizes_client_side() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/model/amazon.titan-embed-image-v1/invoke")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "inputText": "alpha",
                "embeddingConfig": { "outputEmbeddingLength": 2 },
            })))
            .with_status(200)
            .with_body(r#"{"embedding":[3.0,4.0]}"#)
            .expect(1)
            .create_async()
            .await;

        let engine = engine_for(&server.url(), "amazon.titan-embed-image-v1", 2).await;
        let out = engine.embed(&["alpha"]).await.expect("embeds");

        assert!((compute_norm(&out[0]) - 1.0).abs() < 1e-6, "{:?}", out[0]);
        assert!((out[0][0] - 0.6).abs() < 1e-6);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn cohere_batches_every_text_into_one_request_and_normalizes_client_side() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/model/cohere.embed-english-v3/invoke")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "texts": ["alpha", "beta"],
                "input_type": "search_document",
            })))
            .with_status(200)
            .with_body(r#"{"embeddings":[[3.0,4.0],[0.0,2.0]]}"#)
            .expect(1)
            .create_async()
            .await;

        let engine = engine_for(&server.url(), "cohere.embed-english-v3", 2).await;
        let out = engine.embed(&["alpha", "beta"]).await.expect("embeds");

        assert_eq!(out.len(), 2);
        for vector in &out {
            assert!((compute_norm(vector) - 1.0).abs() < 1e-6, "{vector:?}");
        }
        assert!((out[0][0] - 0.6).abs() < 1e-6);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_400_throttling_body_surfaces_as_a_retryable_http_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/model/amazon.titan-embed-text-v1/invoke")
            .with_status(400)
            .with_body(r#"{"__type":"ThrottlingException","message":"slow down"}"#)
            // The retry budget is zero, so exactly one attempt is made.
            .expect(1)
            .create_async()
            .await;

        let engine = engine_for(&server.url(), "amazon.titan-embed-text-v1", 2).await;
        let err = engine.embed(&["alpha"]).await.expect_err("must fail");

        assert!(
            matches!(err, EmbeddingError::HttpError(_)),
            "a 400 ThrottlingException is retryable, not an ApiError: {err:?}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn empty_input_issues_no_request() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .expect(0)
            .create_async()
            .await;

        let engine = engine_for(&server.url(), "amazon.titan-embed-text-v1", 2).await;
        assert!(engine.embed(&[]).await.expect("no-op").is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn blank_inputs_are_sanitized_and_zeroed() {
        let mut server = mockito::Server::new_async().await;
        // The sanitizer replaces the blank text with "." before it hits the wire.
        let mock = server
            .mock("POST", "/model/amazon.titan-embed-text-v1/invoke")
            .match_body(mockito::Matcher::Json(
                serde_json::json!({"inputText": "."}),
            ))
            .with_status(200)
            .with_body(r#"{"embedding":[3.0,4.0]}"#)
            .expect(1)
            .create_async()
            .await;

        let engine = engine_for(&server.url(), "amazon.titan-embed-text-v1", 2).await;
        let out = engine.embed(&["   "]).await.expect("embeds");

        assert_eq!(out, vec![vec![0.0, 0.0]], "an unembeddable slot is zeroed");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn a_cohere_count_mismatch_is_an_api_error() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/model/cohere.embed-english-v3/invoke")
            .with_status(200)
            .with_body(r#"{"embeddings":[[1.0,0.0]]}"#)
            .create_async()
            .await;

        let engine = engine_for(&server.url(), "cohere.embed-english-v3", 2).await;
        let err = engine
            .embed(&["alpha", "beta"])
            .await
            .expect_err("two texts, one embedding");
        assert!(matches!(err, EmbeddingError::ApiError(_)), "{err:?}");
    }

    #[tokio::test]
    async fn an_empty_model_is_rejected_at_construction() {
        let config = EmbeddingConfig {
            provider: EmbeddingProvider::Bedrock,
            model: "   ".to_string(),
            ..EmbeddingConfig::default()
        };
        let err = BedrockEmbeddingEngine::new(&config)
            .await
            .expect_err("no model id");
        assert!(matches!(err, EmbeddingError::ConfigError(_)), "{err:?}");
    }

    #[tokio::test]
    async fn trait_accessors_report_the_configured_values() {
        let mut config = config_for("http://127.0.0.1:1", "amazon.titan-embed-text-v2:0", 1024);
        config.batch_size = 12;
        config.max_completion_tokens = 8191;
        let engine = BedrockEmbeddingEngine::new(&config).await.expect("builds");

        assert_eq!(engine.dimension(), 1024);
        assert_eq!(engine.batch_size(), 12);
        assert_eq!(engine.max_sequence_length(), 8191);
    }
}
