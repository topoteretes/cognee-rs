//! Titan InvokeModel wire shapes: `titan-embed-text-v1` (g1),
//! `titan-embed-text-v2` and the `titan-embed-image-v1` multimodal variant.
//!
//! Port of litellm's `llms/bedrock/embed/amazon_titan_g1_transformation.py`,
//! `amazon_titan_v2_transformation.py` and
//! `amazon_titan_multimodal_transformation.py`, which is what Python cognee
//! reaches through `litellm.aembedding` (plan §1.5).
//!
//! All three families take **one text per request** — litellm's
//! `_single_func_embeddings` loop — and answer with the same
//! `{"embedding": [...], "inputTextTokenCount": n}` envelope.

use serde::{Deserialize, Serialize};

use crate::error::{EmbeddingError, EmbeddingResult};

/// The Titan text body: `{"inputText": …}` for g1, plus the two v2-only knobs.
///
/// `dimensions` and `normalize` are serialised **only when set**, so the g1
/// constructor produces exactly `{"inputText": …}` — g1 rejects unknown fields.
#[derive(Debug, Serialize)]
pub(crate) struct TitanTextRequest<'a> {
    #[serde(rename = "inputText")]
    input_text: &'a str,
    /// v2 only — the requested output length (`1024` | `512` | `256`).
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
    /// v2 only — ask Bedrock to return a unit-norm vector server-side.
    #[serde(skip_serializing_if = "Option::is_none")]
    normalize: Option<bool>,
}

impl<'a> TitanTextRequest<'a> {
    /// `amazon.titan-embed-text-v1` (g1): `{"inputText": …}` and nothing else.
    pub(crate) fn g1(input_text: &'a str) -> Self {
        Self {
            input_text,
            dimensions: None,
            normalize: None,
        }
    }

    /// `amazon.titan-embed-text-v2:0`: adds `dimensions` / `normalize` when
    /// they are configured.
    pub(crate) fn v2(
        input_text: &'a str,
        dimensions: Option<usize>,
        normalize: Option<bool>,
    ) -> Self {
        Self {
            input_text,
            dimensions,
            normalize,
        }
    }
}

/// `embeddingConfig` on the multimodal body.
#[derive(Debug, Serialize)]
pub(crate) struct TitanEmbeddingConfig {
    #[serde(rename = "outputEmbeddingLength")]
    output_embedding_length: usize,
}

/// The `amazon.titan-embed-image-v1` body.
///
/// litellm picks `inputImage` over `inputText` when the input parses as base64
/// (`_is_base64`). That branch is unreachable here: [`crate::EmbeddingEngine`]
/// is a *text* embedding contract — `embed(&[&str])` has no way to say "this
/// string is an image" — so cognee's Rust path always fills `inputText`. The
/// field is modelled anyway so the shape stays honest against the plan's §1.5
/// table.
#[derive(Debug, Serialize)]
pub(crate) struct TitanMultimodalRequest<'a> {
    #[serde(rename = "inputText", skip_serializing_if = "Option::is_none")]
    input_text: Option<&'a str>,
    #[serde(rename = "inputImage", skip_serializing_if = "Option::is_none")]
    input_image: Option<&'a str>,
    #[serde(rename = "embeddingConfig")]
    embedding_config: TitanEmbeddingConfig,
}

impl<'a> TitanMultimodalRequest<'a> {
    /// A text-only multimodal request asking for `output_embedding_length`
    /// dimensions.
    pub(crate) fn text(input_text: &'a str, output_embedding_length: usize) -> Self {
        Self {
            input_text: Some(input_text),
            input_image: None,
            embedding_config: TitanEmbeddingConfig {
                output_embedding_length,
            },
        }
    }
}

/// The response envelope shared by all three Titan families.
#[derive(Debug, Deserialize)]
struct TitanEmbeddingResponse {
    embedding: Vec<f32>,
    /// Reported by every Titan family; cognee has nowhere to put an embedding
    /// token count, so it is parsed and dropped rather than silently ignored.
    #[serde(rename = "inputTextTokenCount", default)]
    #[allow(
        dead_code,
        reason = "documents the wire shape; cognee has no sink for it"
    )]
    input_text_token_count: Option<u64>,
}

/// Parse `{"embedding": [...], "inputTextTokenCount": n}`.
///
/// An unparseable or differently-shaped body is an
/// [`EmbeddingError::ApiError`]: re-POSTing the same request cannot change a
/// response shape, so it must not be classified as transient.
pub(crate) fn parse_response(body: &[u8]) -> EmbeddingResult<Vec<f32>> {
    let parsed: TitanEmbeddingResponse = serde_json::from_slice(body).map_err(|e| {
        EmbeddingError::ApiError(format!(
            "Failed to parse the Titan embedding response: {e}; body: {}",
            String::from_utf8_lossy(body)
        ))
    })?;
    Ok(parsed.embedding)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn body_of<T: Serialize>(request: &T) -> Value {
        serde_json::to_value(request).expect("request serialises")
    }

    #[test]
    fn g1_body_is_exactly_input_text() {
        assert_eq!(
            body_of(&TitanTextRequest::g1("hello")),
            json!({ "inputText": "hello" })
        );
    }

    #[test]
    fn v2_body_omits_unconfigured_knobs() {
        assert_eq!(
            body_of(&TitanTextRequest::v2("hello", None, None)),
            json!({ "inputText": "hello" })
        );
    }

    #[test]
    fn v2_body_carries_dimensions_and_normalize_when_configured() {
        assert_eq!(
            body_of(&TitanTextRequest::v2("hello", Some(1024), Some(true))),
            json!({ "inputText": "hello", "dimensions": 1024, "normalize": true })
        );
    }

    #[test]
    fn multimodal_body_sends_input_text_and_the_embedding_config() {
        assert_eq!(
            body_of(&TitanMultimodalRequest::text("hello", 1024)),
            json!({
                "inputText": "hello",
                "embeddingConfig": { "outputEmbeddingLength": 1024 },
            }),
            "inputImage must be absent — the text path never fills it"
        );
    }

    #[test]
    fn response_unwraps_the_embedding() {
        let body = br#"{"embedding":[0.25,0.5],"inputTextTokenCount":3}"#;
        assert_eq!(parse_response(body).expect("parses"), vec![0.25, 0.5]);
    }

    #[test]
    fn response_without_a_token_count_still_parses() {
        assert_eq!(
            parse_response(br#"{"embedding":[1.0]}"#).expect("parses"),
            vec![1.0]
        );
    }

    #[test]
    fn unexpected_response_shape_is_an_api_error() {
        let err = parse_response(br#"{"embeddings":[[1.0]]}"#).expect_err("must not parse");
        assert!(
            matches!(err, EmbeddingError::ApiError(_)),
            "a wrong shape is terminal, not transient: {err:?}"
        );
    }
}
