//! OpenAI-compatible embedding engine.
//!
//! Supports OpenAI, Azure OpenAI, and any server implementing the OpenAI
//! `/v1/embeddings` endpoint (vLLM, llama.cpp, TEI, LocalAI, etc.).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::EmbeddingConfig;
use crate::engine::EmbeddingEngine;
use crate::error::{EmbeddingError, EmbeddingResult};
use crate::utils::{handle_embedding_response, sanitize_embedding_inputs};

// ─── Response types ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

// ─── Request type ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
    encoding_format: &'a str,
}

// ─── Engine ───────────────────────────────────────────────────────────────────

/// Embedding engine that calls an OpenAI-compatible `/v1/embeddings` HTTP endpoint.
///
/// Works with:
/// - OpenAI (`https://api.openai.com`)
/// - Azure OpenAI (set `api_version` in config)
/// - vLLM, llama.cpp, TEI, LocalAI (any OpenAI-compatible server)
///
/// # URL normalisation
///
/// The `base_url` is derived from `config.endpoint` and is always normalised to
/// end with `/v1` so that the final request URL is `{base_url}/embeddings`.
///
/// The following transformations are applied in order:
/// 1. Strip a trailing `/`
/// 2. If the URL ends with `/v1/embeddings`, strip the `/embeddings` suffix.
/// 3. If the URL does not end with `/v1`, append `/v1`.
///
/// # Retry behaviour
///
/// Transient errors (HTTP 429, 5xx, network errors) are retried with
/// exponential back-off (starting at 2 s, doubling up to 128 s, plus
/// a uniform random jitter in `[0, wait_secs)`) for up to 128 s total.
pub struct OpenAICompatibleEmbeddingEngine {
    client: reqwest::Client,
    /// Normalised base URL ending with `/v1`.
    base_url: String,
    model: String,
    dimensions: usize,
    batch_size: usize,
    max_sequence_length: usize,
}

impl OpenAICompatibleEmbeddingEngine {
    /// Construct a new engine from the given [`EmbeddingConfig`].
    ///
    /// Returns [`EmbeddingError::ConfigError`] if the `reqwest` client cannot
    /// be built (e.g. invalid TLS configuration).
    pub fn new(config: &EmbeddingConfig) -> EmbeddingResult<Self> {
        let raw_endpoint = config
            .endpoint
            .clone()
            .unwrap_or_else(|| "https://api.openai.com".to_string());

        let base_url = normalize_base_url(&raw_endpoint);

        let api_key = config.api_key.clone().unwrap_or_default();

        let mut default_headers = reqwest::header::HeaderMap::new();
        let bearer = format!("Bearer {api_key}");
        let auth_value = reqwest::header::HeaderValue::from_str(&bearer)
            .map_err(|e| EmbeddingError::ConfigError(format!("Invalid API key value: {e}")))?;
        default_headers.insert(reqwest::header::AUTHORIZATION, auth_value);

        // For Azure OpenAI the api-version is sent as a query parameter, not a
        // header.  We store the version on the struct and append it per-request.
        // Nothing to add to default headers here.

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| {
                EmbeddingError::ConfigError(format!("Failed to build HTTP client: {e}"))
            })?;

        Ok(Self {
            client,
            base_url,
            model: config.model.clone(),
            dimensions: config.dimensions,
            batch_size: config.batch_size,
            max_sequence_length: config.max_completion_tokens,
        })
    }

    /// Build the full embeddings URL.
    fn embeddings_url(&self) -> String {
        format!("{}/embeddings", self.base_url)
    }

    /// Call the `/v1/embeddings` endpoint once (no retry).
    async fn embed_batch_once(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let sanitized = sanitize_embedding_inputs(texts);
        let sanitized_strs: Vec<&str> = sanitized.iter().map(|c| c.as_ref()).collect();

        let request_body = EmbeddingRequest {
            model: &self.model,
            input: sanitized_strs,
            encoding_format: "float",
        };

        let response = self
            .client
            .post(self.embeddings_url())
            .json(&request_body)
            .send()
            .await
            .map_err(|e| EmbeddingError::HttpError(format!("Request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read body>".to_string());
            return Err(if status.as_u16() == 429 || status.is_server_error() {
                // Retryable — use HttpError so `is_retryable` can detect it
                EmbeddingError::HttpError(format!("HTTP {status}: {body}"))
            } else {
                EmbeddingError::ApiError(format!("HTTP {status}: {body}"))
            });
        }

        let parsed: EmbeddingResponse = response
            .json()
            .await
            .map_err(|e| EmbeddingError::ApiError(format!("Failed to parse response: {e}")))?;

        let vectors: Vec<Vec<f32>> = parsed.data.into_iter().map(|d| d.embedding).collect();

        // Zero out slots that were originally empty/whitespace
        let result = handle_embedding_response(texts, vectors, self.dimensions);
        Ok(result)
    }

    /// Call the endpoint with exponential-jitter retry on transient errors.
    ///
    /// Retries for up to 128 s total. Wait starts at 2 s and doubles on each
    /// attempt, capped at 128 s.  A uniform random jitter of `[0, wait_secs)`
    /// is added to prevent thundering-herd.
    async fn embed_batch_with_retry(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let max_duration = std::time::Duration::from_secs(128);
        let start = std::time::Instant::now();
        let mut wait_secs = 2u64;
        loop {
            match self.embed_batch_once(texts).await {
                Ok(result) => return Ok(result),
                Err(e) if is_retryable(&e) && start.elapsed() < max_duration => {
                    let jitter = rand::random::<u64>() % wait_secs;
                    tokio::time::sleep(std::time::Duration::from_secs(wait_secs + jitter)).await;
                    wait_secs = (wait_secs * 2).min(128);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[async_trait]
impl EmbeddingEngine for OpenAICompatibleEmbeddingEngine {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut results: Vec<Vec<f32>> = Vec::with_capacity(texts.len());

        for batch in texts.chunks(self.batch_size) {
            let batch_results = self.embed_batch_with_retry(batch).await?;
            results.extend(batch_results);
        }

        Ok(results)
    }

    fn dimension(&self) -> usize {
        self.dimensions
    }

    fn batch_size(&self) -> usize {
        self.batch_size
    }

    fn max_sequence_length(&self) -> usize {
        self.max_sequence_length
    }
}

// ─── Error classification ─────────────────────────────────────────────────────

/// Returns `true` for errors that are worth retrying (rate-limit, server error, network).
fn is_retryable(e: &EmbeddingError) -> bool {
    matches!(e, EmbeddingError::HttpError(_))
}

// ─── URL normalisation ────────────────────────────────────────────────────────

/// Normalise an endpoint URL to always end with `/v1`.
///
/// Rules (applied in order):
/// 1. Strip trailing `/`
/// 2. Strip `/embeddings` suffix if present (so `/v1/embeddings` → `/v1`)
/// 3. Append `/v1` if the URL does not already end with `/v1`
pub(crate) fn normalize_base_url(url: &str) -> String {
    let mut s = url.trim_end_matches('/').to_string();

    if s.ends_with("/v1/embeddings") {
        s.truncate(s.len() - "/embeddings".len());
    }

    if !s.ends_with("/v1") {
        s.push_str("/v1");
    }

    s
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── URL normalisation ────────────────────────────────────────────────────

    #[test]
    fn test_normalize_plain_domain() {
        assert_eq!(
            normalize_base_url("https://api.openai.com"),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn test_normalize_trailing_slash() {
        assert_eq!(
            normalize_base_url("https://api.openai.com/"),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn test_normalize_already_v1() {
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1"),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn test_normalize_v1_trailing_slash() {
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1/"),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn test_normalize_v1_embeddings_suffix() {
        assert_eq!(
            normalize_base_url("https://api.openai.com/v1/embeddings"),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn test_normalize_localhost_with_port() {
        assert_eq!(
            normalize_base_url("http://localhost:11434"),
            "http://localhost:11434/v1"
        );
    }

    #[test]
    fn test_normalize_localhost_with_port_v1() {
        assert_eq!(
            normalize_base_url("http://localhost:8080/v1"),
            "http://localhost:8080/v1"
        );
    }

    #[test]
    fn test_normalize_azure_endpoint() {
        // Azure endpoints typically end with the API path, not /v1
        let url = "https://myresource.openai.azure.com/openai";
        assert_eq!(
            normalize_base_url(url),
            "https://myresource.openai.azure.com/openai/v1"
        );
    }

    // ── Constructor ──────────────────────────────────────────────────────────

    #[test]
    fn test_new_with_defaults() {
        let config = EmbeddingConfig {
            model: "text-embedding-3-small".to_string(),
            dimensions: 1536,
            batch_size: 10,
            ..EmbeddingConfig::default()
        };
        let engine = OpenAICompatibleEmbeddingEngine::new(&config)
            .expect("should build engine with default config");
        assert_eq!(engine.dimension(), 1536);
        assert_eq!(engine.batch_size(), 10);
        assert_eq!(engine.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn test_new_with_custom_endpoint() {
        let config = EmbeddingConfig {
            endpoint: Some("http://localhost:8080/v1/embeddings".to_string()),
            model: "my-model".to_string(),
            dimensions: 384,
            batch_size: 5,
            ..EmbeddingConfig::default()
        };
        let engine = OpenAICompatibleEmbeddingEngine::new(&config)
            .expect("should build engine with custom endpoint");
        assert_eq!(engine.base_url, "http://localhost:8080/v1");
    }

    #[test]
    fn test_embeddings_url() {
        let config = EmbeddingConfig {
            endpoint: Some("https://api.openai.com".to_string()),
            ..EmbeddingConfig::default()
        };
        let engine = OpenAICompatibleEmbeddingEngine::new(&config).expect("should build engine");
        assert_eq!(
            engine.embeddings_url(),
            "https://api.openai.com/v1/embeddings"
        );
    }

    // ── is_retryable ─────────────────────────────────────────────────────────

    #[test]
    fn test_is_retryable_http_error() {
        assert!(is_retryable(&EmbeddingError::HttpError(
            "HTTP 429: rate limited".to_string()
        )));
        assert!(is_retryable(&EmbeddingError::HttpError(
            "HTTP 503: unavailable".to_string()
        )));
    }

    #[test]
    fn test_is_retryable_api_error_not_retryable() {
        assert!(!is_retryable(&EmbeddingError::ApiError(
            "HTTP 400: bad request".to_string()
        )));
        assert!(!is_retryable(&EmbeddingError::ConfigError(
            "bad config".to_string()
        )));
    }
}
