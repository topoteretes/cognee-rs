//! Cohere InvokeModel wire shapes (`cohere.embed-*`).
//!
//! Port of litellm's `llms/bedrock/embed/cohere_transformation.py` (plan §1.5).
//! Unlike the Titan families, Cohere takes **every text in one request** and
//! answers with `{"embeddings": [[...]]}`.

use serde::{Deserialize, Serialize};

use crate::error::{EmbeddingError, EmbeddingResult};

/// litellm's hard-coded `input_type` for the embedding path.
///
/// cognee embeds corpus text, not queries, and Python never exposes a knob for
/// this — so it is a constant here too rather than a config field.
pub(crate) const INPUT_TYPE_SEARCH_DOCUMENT: &str = "search_document";

/// The Cohere embed body.
///
/// `embedding_types` and `output_dimension` are modelled (they are in the §1.5
/// table) but never populated: `EmbeddingConfig` has no knob for either, and
/// sending `embedding_types` would change the response envelope from a bare
/// list to `{"float": [...]}` — see [`parse_response`].
#[derive(Debug, Serialize)]
pub(crate) struct CohereEmbeddingRequest<'a> {
    texts: Vec<&'a str>,
    input_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding_types: Option<Vec<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_dimension: Option<usize>,
}

impl<'a> CohereEmbeddingRequest<'a> {
    /// A `search_document` request over the whole batch.
    pub(crate) fn search_document(texts: &[&'a str]) -> Self {
        Self {
            texts: texts.to_vec(),
            input_type: INPUT_TYPE_SEARCH_DOCUMENT,
            embedding_types: None,
            output_dimension: None,
        }
    }
}

/// `{"embeddings": [[...]]}`.
#[derive(Debug, Deserialize)]
struct CohereEmbeddingResponse {
    embeddings: Vec<Vec<f32>>,
}

/// Parse `{"embeddings": [[...]]}`.
///
/// Only the bare-list envelope is accepted. Cohere switches to
/// `{"embeddings": {"float": [[...]]}}` when the request sets
/// `embedding_types`, which [`CohereEmbeddingRequest`] never does — so a body
/// in that shape means the request was not the one this module built, and
/// failing loudly beats guessing.
pub(crate) fn parse_response(body: &[u8]) -> EmbeddingResult<Vec<Vec<f32>>> {
    let parsed: CohereEmbeddingResponse = serde_json::from_slice(body).map_err(|e| {
        EmbeddingError::ApiError(format!(
            "Failed to parse the Cohere embedding response: {e}; body: {}",
            String::from_utf8_lossy(body)
        ))
    })?;
    Ok(parsed.embeddings)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn body_batches_every_text_with_the_search_document_input_type() {
        let request = CohereEmbeddingRequest::search_document(&["alpha", "beta"]);
        assert_eq!(
            serde_json::to_value(&request).expect("request serialises"),
            json!({ "texts": ["alpha", "beta"], "input_type": "search_document" }),
            "embedding_types / output_dimension must stay absent"
        );
    }

    #[test]
    fn response_unwraps_the_embedding_list() {
        let body = br#"{"embeddings":[[0.1,0.2],[0.3,0.4]]}"#;
        assert_eq!(
            parse_response(body).expect("parses"),
            vec![vec![0.1, 0.2], vec![0.3, 0.4]]
        );
    }

    #[test]
    fn typed_embedding_envelope_is_rejected_rather_than_guessed() {
        let err = parse_response(br#"{"embeddings":{"float":[[0.1]]}}"#)
            .expect_err("we never ask for embedding_types");
        assert!(matches!(err, EmbeddingError::ApiError(_)), "{err:?}");
    }
}
