//! Ollama embedding engine.
//!
//! Calls the Ollama `/api/embed` endpoint with a batched array `input`,
//! sub-batched by `batch_size`, falling back to one concurrent request per text
//! on servers that do not accept array input. Supports all three response
//! shapes that Ollama can return:
//! - `{"embeddings": [[...]]}` — standard Ollama `/api/embed`
//! - `{"embedding": [...]}` — legacy Ollama `/api/embeddings`
//! - `{"data": [{"embedding": [...]}]}` — OpenAI-compatible fallback shape

use async_trait::async_trait;
use futures::future;
use serde::Serialize;
use serde_json::Value;

use crate::config::EmbeddingConfig;
use crate::engine::EmbeddingEngine;
use crate::error::{EmbeddingError, EmbeddingResult};
use crate::utils::{handle_embedding_response, sanitize_embedding_inputs};

// ─── Request type ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct OllamaEmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

/// Batched request body: recent Ollama `/api/embed` accepts an array `input`
/// and returns one embedding per element under the `embeddings` key.
#[derive(Serialize)]
struct OllamaBatchEmbedRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

// ─── Engine ───────────────────────────────────────────────────────────────────

/// Embedding engine that calls the Ollama `/api/embed` HTTP endpoint.
///
/// Sends a batched array `input` per request, sub-batched by `batch_size`, and
/// falls back to one concurrent request per text (via
/// `futures::future::join_all`) for servers that do not accept array input.
/// Transient HTTP errors (network failures, 429, 5xx) are retried with
/// exponential back-off starting at 8 s (doubling to 128 s) for up to 128 s total.
///
/// # Response shapes
///
/// Ollama can return embeddings in three shapes depending on the version and endpoint:
/// - `{"embeddings": [[...]]}` — standard `/api/embed` response
/// - `{"embedding": [...]}` — legacy single-embedding response
/// - `{"data": [{"embedding": [...]}]}` — OpenAI-compatible shape
///
/// All three shapes are handled transparently.
pub struct OllamaEmbeddingEngine {
    client: reqwest::Client,
    /// Full URL to the Ollama embed endpoint, e.g. `http://localhost:11434/api/embed`.
    endpoint: String,
    model: String,
    dimensions: usize,
    batch_size: usize,
    max_completion_tokens: usize,
}

impl OllamaEmbeddingEngine {
    /// Construct a new engine from the given [`EmbeddingConfig`].
    ///
    /// Returns [`EmbeddingError::ConfigError`] if the `reqwest` client cannot
    /// be built (e.g. invalid TLS or API key header value).
    pub fn new(config: &EmbeddingConfig) -> EmbeddingResult<Self> {
        let endpoint = config
            .endpoint
            .clone()
            .unwrap_or_else(|| "http://localhost:11434/api/embed".to_string());

        let mut default_headers = reqwest::header::HeaderMap::new();

        if let Some(api_key) = &config.api_key
            && !api_key.is_empty()
        {
            let bearer = format!("Bearer {api_key}");
            let auth_value = reqwest::header::HeaderValue::from_str(&bearer)
                .map_err(|e| EmbeddingError::ConfigError(format!("Invalid API key value: {e}")))?;
            default_headers.insert(reqwest::header::AUTHORIZATION, auth_value);
        }

        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| {
                EmbeddingError::ConfigError(format!("Failed to build HTTP client: {e}"))
            })?;

        Ok(Self {
            client,
            endpoint,
            model: config.model.clone(),
            dimensions: config.dimensions,
            batch_size: config.batch_size,
            max_completion_tokens: config.max_completion_tokens,
        })
    }

    /// Truncate `text` to at most `max_completion_tokens * 4` characters.
    ///
    /// Truncation is on a Unicode character boundary, not a byte boundary.
    /// The factor of 4 is the same heuristic used by the Python SDK.
    fn truncate_text<'a>(&self, text: &'a str) -> &'a str {
        let char_limit = self.max_completion_tokens * 4;
        let byte_pos = text
            .char_indices()
            .nth(char_limit)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        &text[..byte_pos]
    }

    /// Call the Ollama endpoint once for a single text (no retry).
    async fn embed_single_once(&self, text: &str) -> EmbeddingResult<Vec<f32>> {
        let truncated = self.truncate_text(text);

        let request_body = OllamaEmbedRequest {
            model: &self.model,
            input: truncated,
            // Only send `dimensions` if it's non-zero; some older Ollama versions
            // reject unknown fields.
            dimensions: if self.dimensions > 0 {
                Some(self.dimensions)
            } else {
                None
            },
        };

        let response = self
            .client
            .post(&self.endpoint)
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
                EmbeddingError::HttpError(format!("HTTP {status}: {body}"))
            } else {
                EmbeddingError::ApiError(format!("HTTP {status}: {body}"))
            });
        }

        let value: Value = response
            .json()
            .await
            .map_err(|e| EmbeddingError::ApiError(format!("Failed to parse response: {e}")))?;

        extract_embedding_from_value(&value)
    }

    /// Call the endpoint with exponential-jitter retry on transient errors.
    ///
    /// Retries for up to 128 s total. Wait starts at 8 s (matching the Python
    /// Ollama engine) and doubles on each attempt, capped at 128 s.  A uniform
    /// random jitter of `[0, wait_secs)` is added to prevent thundering-herd.
    async fn embed_single_with_retry(&self, text: &str) -> EmbeddingResult<Vec<f32>> {
        let max_duration = std::time::Duration::from_secs(128);
        let start = std::time::Instant::now();
        let mut wait_secs = 8u64;
        loop {
            match self.embed_single_once(text).await {
                Ok(v) => return Ok(v),
                Err(e)
                    if matches!(e, EmbeddingError::HttpError(_))
                        && start.elapsed() < max_duration =>
                {
                    let jitter = rand::random::<u64>() % wait_secs;
                    tokio::time::sleep(std::time::Duration::from_secs(wait_secs + jitter)).await;
                    wait_secs = (wait_secs * 2).min(128);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Call the endpoint once with an array `input` (no retry).
    ///
    /// Returns [`EmbeddingError::ApiError`] if the server returns a number of
    /// embeddings that does not match the number of inputs — which is also how
    /// servers that ignore array `input` surface, letting [`embed_all`] fall
    /// back to one request per text.
    async fn embed_batch_once(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let truncated: Vec<&str> = texts.iter().map(|t| self.truncate_text(t)).collect();

        let request_body = OllamaBatchEmbedRequest {
            model: &self.model,
            input: truncated,
            dimensions: if self.dimensions > 0 {
                Some(self.dimensions)
            } else {
                None
            },
        };

        let response = self
            .client
            .post(&self.endpoint)
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
                EmbeddingError::HttpError(format!("HTTP {status}: {body}"))
            } else {
                EmbeddingError::ApiError(format!("HTTP {status}: {body}"))
            });
        }

        let value: Value = response
            .json()
            .await
            .map_err(|e| EmbeddingError::ApiError(format!("Failed to parse response: {e}")))?;

        let embeddings = extract_all_embeddings_from_value(&value)?;
        if embeddings.len() != texts.len() {
            return Err(EmbeddingError::ApiError(format!(
                "Expected {} embeddings for batch, got {}",
                texts.len(),
                embeddings.len()
            )));
        }
        Ok(embeddings)
    }

    /// Batch variant of [`embed_single_with_retry`], retrying transient errors.
    async fn embed_batch_with_retry(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let max_duration = std::time::Duration::from_secs(128);
        let start = std::time::Instant::now();
        let mut wait_secs = 8u64;
        loop {
            match self.embed_batch_once(texts).await {
                Ok(v) => return Ok(v),
                Err(e)
                    if matches!(e, EmbeddingError::HttpError(_))
                        && start.elapsed() < max_duration =>
                {
                    let jitter = rand::random::<u64>() % wait_secs;
                    tokio::time::sleep(std::time::Duration::from_secs(wait_secs + jitter)).await;
                    wait_secs = (wait_secs * 2).min(128);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Embed all texts, sub-batched by `batch_size` using array `input`.
    ///
    /// If a batch fails with a non-transient [`EmbeddingError::ApiError`] (e.g.
    /// an older Ollama that does not accept array `input`), that batch falls
    /// back to the legacy path of one concurrent request per text, so behavior
    /// is preserved on every server version.
    async fn embed_all(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let sanitized = sanitize_embedding_inputs(texts);
        let sanitized_refs: Vec<&str> = sanitized.iter().map(|s| s.as_ref()).collect();

        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        for batch in sanitized_refs.chunks(self.batch_size) {
            match self.embed_batch_with_retry(batch).await {
                Ok(batch_embeddings) => embeddings.extend(batch_embeddings),
                Err(EmbeddingError::ApiError(_)) => {
                    let futures: Vec<_> = batch
                        .iter()
                        .map(|&text| self.embed_single_with_retry(text))
                        .collect();
                    for result in future::join_all(futures).await {
                        embeddings.push(result?);
                    }
                }
                Err(e) => return Err(e),
            }
        }

        Ok(handle_embedding_response(
            texts,
            embeddings,
            self.dimensions,
        ))
    }
}

#[async_trait]
impl EmbeddingEngine for OllamaEmbeddingEngine {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.embed_all(texts).await
    }

    fn dimension(&self) -> usize {
        self.dimensions
    }

    fn batch_size(&self) -> usize {
        self.batch_size
    }

    fn max_sequence_length(&self) -> usize {
        self.max_completion_tokens
    }
}

// ─── Response parsing ─────────────────────────────────────────────────────────

/// Extract a `Vec<f32>` from any of the three response shapes Ollama can return.
///
/// Shape 1 — standard `/api/embed`:
/// ```json
/// {"embeddings": [[0.1, 0.2, ...]]}
/// ```
///
/// Shape 2 — legacy `/api/embeddings` (single embedding):
/// ```json
/// {"embedding": [0.1, 0.2, ...]}
/// ```
///
/// Shape 3 — OpenAI-compatible:
/// ```json
/// {"data": [{"embedding": [0.1, 0.2, ...]}]}
/// ```
fn extract_embedding_from_value(value: &Value) -> EmbeddingResult<Vec<f32>> {
    // Shape 1: {"embeddings": [[...]]}
    if let Some(embeddings) = value.get("embeddings") {
        if let Some(first) = embeddings.get(0) {
            return parse_f32_array(first);
        }
        return Err(EmbeddingError::ApiError(
            "Response 'embeddings' array is empty".to_string(),
        ));
    }

    // Shape 2: {"embedding": [...]}
    if let Some(embedding) = value.get("embedding") {
        return parse_f32_array(embedding);
    }

    // Shape 3: {"data": [{"embedding": [...]}]}
    if let Some(data) = value.get("data") {
        if let Some(first) = data.get(0)
            && let Some(embedding) = first.get("embedding")
        {
            return parse_f32_array(embedding);
        }
        return Err(EmbeddingError::ApiError(
            "Response 'data' array is empty or missing 'embedding' field".to_string(),
        ));
    }

    Err(EmbeddingError::ApiError(format!(
        "Unrecognised response shape; expected 'embeddings', 'embedding', or 'data' key. Got: {value}"
    )))
}

/// Extract every embedding from a batched response (array `input`).
///
/// Handles the same shapes as [`extract_embedding_from_value`] but returns all
/// embeddings rather than just the first:
/// - `{"embeddings": [[...], [...]]}` — standard `/api/embed`
/// - `{"data": [{"embedding": [...]}, ...]}` — OpenAI-compatible
/// - `{"embedding": [...]}` — single embedding, returned as a one-element vec
fn extract_all_embeddings_from_value(value: &Value) -> EmbeddingResult<Vec<Vec<f32>>> {
    if let Some(embeddings) = value.get("embeddings").and_then(|v| v.as_array()) {
        return embeddings.iter().map(parse_f32_array).collect();
    }

    if let Some(data) = value.get("data").and_then(|v| v.as_array()) {
        return data
            .iter()
            .map(|item| {
                item.get("embedding").ok_or_else(|| {
                    EmbeddingError::ApiError("Response 'data' item missing 'embedding'".to_string())
                })
            })
            .map(|embedding| embedding.and_then(parse_f32_array))
            .collect();
    }

    if let Some(embedding) = value.get("embedding") {
        return Ok(vec![parse_f32_array(embedding)?]);
    }

    Err(EmbeddingError::ApiError(format!(
        "Unrecognised response shape; expected 'embeddings', 'embedding', or 'data' key. Got: {value}"
    )))
}

/// Parse a JSON array of numbers into a `Vec<f32>`.
fn parse_f32_array(value: &Value) -> EmbeddingResult<Vec<f32>> {
    let arr = value.as_array().ok_or_else(|| {
        EmbeddingError::ApiError(format!("Expected a JSON array for embedding, got: {value}"))
    })?;

    arr.iter()
        .map(|v| {
            v.as_f64().map(|f| f as f32).ok_or_else(|| {
                EmbeddingError::ApiError(format!("Non-numeric value in embedding array: {v}"))
            })
        })
        .collect()
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
    use crate::config::EmbeddingConfig;
    use crate::provider::EmbeddingProvider;

    fn make_config() -> EmbeddingConfig {
        EmbeddingConfig {
            provider: EmbeddingProvider::Ollama,
            model: "avr/sfr-embedding-mistral:latest".to_string(),
            dimensions: 1024,
            endpoint: None,
            api_key: None,
            api_version: None,
            max_completion_tokens: 8191,
            batch_size: 10,
            mock: false,
            mock_mode: Default::default(),
            #[cfg(feature = "onnx")]
            onnx: Default::default(),
            huggingface_tokenizer: None,
        }
    }

    #[test]
    fn test_constructor_defaults() {
        let config = make_config();
        let engine = OllamaEmbeddingEngine::new(&config).expect("should construct engine");
        assert_eq!(engine.endpoint, "http://localhost:11434/api/embed");
        assert_eq!(engine.model, "avr/sfr-embedding-mistral:latest");
        assert_eq!(engine.dimension(), 1024);
        assert_eq!(engine.batch_size(), 10);
        assert_eq!(engine.max_sequence_length(), 8191);
    }

    #[test]
    fn test_constructor_custom_endpoint() {
        let config = EmbeddingConfig {
            endpoint: Some("http://my-ollama:11434/api/embed".to_string()),
            ..make_config()
        };
        let engine = OllamaEmbeddingEngine::new(&config).expect("should construct engine");
        assert_eq!(engine.endpoint, "http://my-ollama:11434/api/embed");
    }

    #[test]
    fn test_truncate_text_short() {
        let config = EmbeddingConfig {
            max_completion_tokens: 10,
            ..make_config()
        };
        let engine = OllamaEmbeddingEngine::new(&config).expect("should construct engine");
        // "hello" is 5 chars, limit is 10 * 4 = 40 — no truncation
        let result = engine.truncate_text("hello");
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_text_exact_limit() {
        let config = EmbeddingConfig {
            max_completion_tokens: 2,
            ..make_config()
        };
        let engine = OllamaEmbeddingEngine::new(&config).expect("should construct engine");
        // limit = 2 * 4 = 8 chars; "abcdefgh" is exactly 8 chars → no truncation
        let result = engine.truncate_text("abcdefgh");
        assert_eq!(result, "abcdefgh");
    }

    #[test]
    fn test_truncate_text_over_limit() {
        let config = EmbeddingConfig {
            max_completion_tokens: 2,
            ..make_config()
        };
        let engine = OllamaEmbeddingEngine::new(&config).expect("should construct engine");
        // limit = 2 * 4 = 8 chars; "abcdefghij" has 10 chars → truncated to 8
        let result = engine.truncate_text("abcdefghij");
        assert_eq!(result, "abcdefgh");
    }

    #[test]
    fn test_truncate_text_unicode_boundary() {
        let config = EmbeddingConfig {
            max_completion_tokens: 1,
            ..make_config()
        };
        let engine = OllamaEmbeddingEngine::new(&config).expect("should construct engine");
        // limit = 1 * 4 = 4 chars
        // "héllo" has 5 chars; 'é' is 2 bytes — must truncate at char boundary
        let result = engine.truncate_text("héllo");
        // First 4 chars: 'h', 'é', 'l', 'l'
        assert_eq!(result, "héll");
        // Must be valid UTF-8
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn test_truncate_text_empty() {
        let config = make_config();
        let engine = OllamaEmbeddingEngine::new(&config).expect("should construct engine");
        assert_eq!(engine.truncate_text(""), "");
    }

    // ── Response shape parsing ───────────────────────────────────────────────

    #[test]
    fn test_parse_shape1_embeddings() {
        let json = serde_json::json!({
            "embeddings": [[0.1_f64, 0.2_f64, 0.3_f64]]
        });
        let result = extract_embedding_from_value(&json).expect("should parse shape 1");
        assert_eq!(result.len(), 3);
        assert!((result[0] - 0.1_f32).abs() < 1e-6);
        assert!((result[1] - 0.2_f32).abs() < 1e-6);
        assert!((result[2] - 0.3_f32).abs() < 1e-6);
    }

    #[test]
    fn test_parse_shape2_embedding() {
        let json = serde_json::json!({
            "embedding": [0.4_f64, 0.5_f64]
        });
        let result = extract_embedding_from_value(&json).expect("should parse shape 2");
        assert_eq!(result.len(), 2);
        assert!((result[0] - 0.4_f32).abs() < 1e-6);
        assert!((result[1] - 0.5_f32).abs() < 1e-6);
    }

    #[test]
    fn test_parse_shape3_data() {
        let json = serde_json::json!({
            "data": [{"embedding": [0.6_f64, 0.7_f64, 0.8_f64]}]
        });
        let result = extract_embedding_from_value(&json).expect("should parse shape 3");
        assert_eq!(result.len(), 3);
        assert!((result[0] - 0.6_f32).abs() < 1e-6);
        assert!((result[1] - 0.7_f32).abs() < 1e-6);
        assert!((result[2] - 0.8_f32).abs() < 1e-6);
    }

    #[test]
    fn test_parse_unrecognised_shape() {
        let json = serde_json::json!({ "unknown": "value" });
        let result = extract_embedding_from_value(&json);
        assert!(result.is_err());
        assert!(matches!(result, Err(EmbeddingError::ApiError(_))));
    }

    #[test]
    fn test_parse_empty_embeddings_array() {
        let json = serde_json::json!({ "embeddings": [] });
        let result = extract_embedding_from_value(&json);
        assert!(result.is_err());
        assert!(matches!(result, Err(EmbeddingError::ApiError(_))));
    }

    #[test]
    fn test_parse_empty_data_array() {
        let json = serde_json::json!({ "data": [] });
        let result = extract_embedding_from_value(&json);
        assert!(result.is_err());
        assert!(matches!(result, Err(EmbeddingError::ApiError(_))));
    }

    #[test]
    fn test_parse_non_numeric_values() {
        let json = serde_json::json!({ "embedding": ["not", "numbers"] });
        let result = extract_embedding_from_value(&json);
        assert!(result.is_err());
        assert!(matches!(result, Err(EmbeddingError::ApiError(_))));
    }

    // ── Batched response parsing (array input) ───────────────────────────────

    #[test]
    fn test_parse_all_embeddings_shape1() {
        let json = serde_json::json!({
            "embeddings": [[0.1_f64, 0.2_f64], [0.3_f64, 0.4_f64]]
        });
        let result = extract_all_embeddings_from_value(&json).expect("should parse batch");
        assert_eq!(result.len(), 2);
        assert!((result[0][0] - 0.1_f32).abs() < 1e-6);
        assert!((result[1][1] - 0.4_f32).abs() < 1e-6);
    }

    #[test]
    fn test_parse_all_embeddings_data_shape() {
        let json = serde_json::json!({
            "data": [{"embedding": [0.1_f64]}, {"embedding": [0.2_f64]}]
        });
        let result = extract_all_embeddings_from_value(&json).expect("should parse batch");
        assert_eq!(result.len(), 2);
        assert!((result[0][0] - 0.1_f32).abs() < 1e-6);
        assert!((result[1][0] - 0.2_f32).abs() < 1e-6);
    }

    #[test]
    fn test_parse_all_embeddings_single_shape() {
        let json = serde_json::json!({ "embedding": [0.5_f64, 0.6_f64] });
        let result = extract_all_embeddings_from_value(&json).expect("should parse single");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 2);
    }

    #[test]
    fn test_parse_all_embeddings_unrecognised() {
        let json = serde_json::json!({ "nope": 1 });
        assert!(matches!(
            extract_all_embeddings_from_value(&json),
            Err(EmbeddingError::ApiError(_))
        ));
    }

    // ── End-to-end batching / fallback (mock HTTP server) ────────────────────

    fn config_for(server_url: &str) -> EmbeddingConfig {
        EmbeddingConfig {
            dimensions: 2,
            endpoint: Some(format!("{server_url}/api/embed")),
            ..make_config()
        }
    }

    #[tokio::test]
    async fn embed_batches_array_input() {
        let mut server = mockito::Server::new_async().await;
        let batch = server
            .mock("POST", "/api/embed")
            .match_body(mockito::Matcher::Regex(r#""input":\["#.to_string()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"embeddings":[[1.0,0.0],[0.0,1.0]]}"#)
            .create_async()
            .await;

        let engine = OllamaEmbeddingEngine::new(&config_for(&server.url())).unwrap();
        let out = engine.embed(&["alpha", "beta"]).await.unwrap();

        assert_eq!(out, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        batch.assert_async().await;
    }

    #[tokio::test]
    async fn embed_falls_back_to_per_text_when_array_rejected() {
        let mut server = mockito::Server::new_async().await;
        // Array request rejected with a non-retryable 4xx (older Ollama).
        let batch = server
            .mock("POST", "/api/embed")
            .match_body(mockito::Matcher::Regex(r#""input":\["#.to_string()))
            .with_status(400)
            .with_body("array input not supported")
            .create_async()
            .await;
        // Per-text requests succeed; distinct vectors verify ordering is kept.
        let single_a = server
            .mock("POST", "/api/embed")
            .match_body(mockito::Matcher::Regex(r#""input":"alpha""#.to_string()))
            .with_status(200)
            .with_body(r#"{"embedding":[1.0,0.0]}"#)
            .create_async()
            .await;
        let single_b = server
            .mock("POST", "/api/embed")
            .match_body(mockito::Matcher::Regex(r#""input":"beta""#.to_string()))
            .with_status(200)
            .with_body(r#"{"embedding":[0.0,1.0]}"#)
            .create_async()
            .await;

        let engine = OllamaEmbeddingEngine::new(&config_for(&server.url())).unwrap();
        let out = engine.embed(&["alpha", "beta"]).await.unwrap();

        assert_eq!(out, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        batch.assert_async().await;
        single_a.assert_async().await;
        single_b.assert_async().await;
    }
}
