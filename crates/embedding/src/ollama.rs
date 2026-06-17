//! Ollama embedding engine.
//!
//! Calls the Ollama `/api/embed` endpoint for each input text concurrently.
//! Supports all three response shapes that Ollama can return:
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

// ─── Engine ───────────────────────────────────────────────────────────────────

/// Embedding engine that calls the Ollama `/api/embed` HTTP endpoint.
///
/// Sends one request per input text concurrently using `futures::future::join_all`.
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

    /// Embed all texts concurrently, one request per text.
    async fn embed_all(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        let sanitized = sanitize_embedding_inputs(texts);
        let sanitized_refs: Vec<&str> = sanitized.iter().map(|s| s.as_ref()).collect();

        let futures: Vec<_> = sanitized_refs
            .iter()
            .map(|&text| self.embed_single_with_retry(text))
            .collect();

        let results = future::join_all(futures).await;

        let embeddings: EmbeddingResult<Vec<Vec<f32>>> = results.into_iter().collect();

        Ok(handle_embedding_response(
            texts,
            embeddings?,
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
}
