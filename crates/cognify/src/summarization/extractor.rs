//! Summary extractor using LLM for text summarization.
//!
//! Port of Python's:
//! - cognee/infrastructure/llm/extraction/extract_summary.py
//! - cognee/tasks/summarization/summarize_text.py

use std::sync::Arc;

use cognee_llm::{GenerationOptions, Llm, LlmExt};
use cognee_models::DocumentChunk;

use super::models::{SummarizedContent, TextSummary};
use crate::error::CognifyError;

/// Default summarization options shared by both the typed and dynamic paths.
fn default_summary_options() -> GenerationOptions {
    GenerationOptions {
        temperature: Some(0.3),
        max_tokens: Some(500),
        ..Default::default()
    }
}

/// Default system prompt for text summarization.
///
/// Vendored byte-for-byte from Python's
/// `cognee/infrastructure/llm/prompts/summarize_content.txt` (structured
/// categories + ordered facts, ≤200 tokens). Kept in sync via the prompt-parity
/// drift guard.
const DEFAULT_SUMMARY_PROMPT: &str = include_str!("prompts/summarize_content.txt");

/// Summary extractor for text chunks.
///
/// Uses an LLM (via the Llm trait) to generate hierarchical summaries from text chunks.
/// Produces TextSummary objects linked to source chunks via deterministic UUIDs.
///
/// # Example
/// ```ignore
/// use cognee_cognify::SummaryExtractor;
/// use cognee_llm::OpenAIAdapter;
/// use std::sync::Arc;
///
/// let llm = Arc::new(OpenAIAdapter::new("gpt-4", "sk-...", None)?);
/// let extractor = SummaryExtractor::new(llm);
///
/// let text = "Long article text here...";
/// let summary = extractor.extract_summary(text, None).await?;
///
/// println!("Summary: {}", summary.summary);
/// ```
#[derive(Clone)]
pub struct SummaryExtractor {
    llm: Arc<dyn Llm>,
    /// When `Some`, the dynamic schema path is taken instead of the typed
    /// `SummarizedContent` path (Python `summarization_model` parity).
    summary_schema: Option<serde_json::Value>,
}

impl SummaryExtractor {
    /// Create a new summary extractor using the built-in `SummarizedContent` schema.
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self {
            llm,
            summary_schema: None,
        }
    }

    /// Create a new summary extractor with an optional custom output schema.
    ///
    /// When `schema` is `Some`, the LLM is called via the dynamic raw path and
    /// the `summary` string field is extracted from the response. When `None`,
    /// the built-in typed `SummarizedContent` path is used.
    pub fn new_with_schema(llm: Arc<dyn Llm>, schema: Option<serde_json::Value>) -> Self {
        Self {
            llm,
            summary_schema: schema,
        }
    }

    /// Extract a summary from text.
    ///
    /// When `summary_schema` is `None`, uses the typed `SummarizedContent` path.
    /// When `Some`, calls the LLM with the custom schema and extracts the
    /// `summary` string field from the raw response (Python parity).
    pub async fn extract_summary(
        &self,
        text: &str,
        custom_prompt: Option<&str>,
    ) -> Result<SummarizedContent, CognifyError> {
        let system_prompt = custom_prompt.unwrap_or(DEFAULT_SUMMARY_PROMPT);
        let options = Some(default_summary_options());

        match &self.summary_schema {
            None => {
                let summarized: SummarizedContent = self
                    .llm
                    .create_structured_output(text, system_prompt, options)
                    .await
                    .map_err(|e| CognifyError::LlmError(e.to_string()))?;
                Ok(summarized)
            }
            Some(schema) => {
                let raw: serde_json::Value = self
                    .llm
                    .create_structured_output_raw(text, system_prompt, schema, options)
                    .await
                    .map_err(|e| CognifyError::LlmError(e.to_string()))?;
                let summary = raw.get("summary").and_then(|v| v.as_str()).ok_or_else(|| {
                    CognifyError::LlmError(
                        "summary_schema output missing string `summary` field".to_string(),
                    )
                })?;
                Ok(SummarizedContent {
                    summary: summary.to_string(),
                    description: String::new(),
                })
            }
        }
    }

    /// Summarize multiple text chunks in parallel.
    ///
    /// # Arguments
    /// * `chunks` - Slice of DocumentChunks to summarize
    /// * `custom_prompt` - Optional custom system prompt
    ///
    /// # Returns
    /// A vector of TextSummary objects, one per input chunk
    ///
    /// # Errors
    /// Returns CognifyError::LlmError if any LLM call fails
    pub async fn summarize_chunks(
        &self,
        chunks: &[DocumentChunk],
        custom_prompt: Option<String>,
    ) -> Result<Vec<TextSummary>, CognifyError> {
        if chunks.is_empty() {
            return Ok(vec![]);
        }

        let mut tasks = Vec::new();

        for chunk in chunks {
            let llm_clone = Arc::clone(&self.llm);
            let schema_clone = self.summary_schema.clone();
            let prompt_clone = custom_prompt.clone();
            let text = chunk.text.clone();

            let task = tokio::spawn(async move {
                let extractor = SummaryExtractor {
                    llm: llm_clone,
                    summary_schema: schema_clone,
                };
                extractor
                    .extract_summary(&text, prompt_clone.as_deref())
                    .await
            });

            tasks.push(task);
        }

        let results = futures::future::join_all(tasks).await;

        // Get model name from LLM
        let model_name = self.llm.model().to_string();

        let mut summaries = Vec::new();
        for (chunk_index, result) in results.into_iter().enumerate() {
            let chunk = &chunks[chunk_index];
            let summarized =
                result.map_err(|e| CognifyError::LlmError(format!("Task join error: {}", e)))??;

            let text_summary =
                TextSummary::from_summarized_content(chunk.base.id, summarized, model_name.clone());

            summaries.push(text_summary);
        }

        Ok(summaries)
    }

    /// Get a reference to the underlying LLM.
    pub fn llm(&self) -> &Arc<dyn Llm> {
        &self.llm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::validate_summary_schema;

    // Note: Tests that require LLM are in integration tests (tests/)
    // These are just structural tests

    #[test]
    fn test_default_prompt_not_empty() {
        assert!(!DEFAULT_SUMMARY_PROMPT.is_empty());
        assert!(DEFAULT_SUMMARY_PROMPT.contains("Summarize the chunk for retrieval"));
    }

    #[test]
    fn summary_prompt_matches_vendored_txt() {
        let vendored = include_str!("prompts/summarize_content.txt");
        assert_eq!(
            DEFAULT_SUMMARY_PROMPT, vendored,
            "const drifted from vendored .txt"
        );
        assert!(
            vendored.contains("Output two sections only"),
            "Python two-section structure marker missing"
        );
        assert!(
            vendored.contains("Max 200 tokens"),
            "token-limit marker missing"
        );
    }

    #[test]
    fn new_returns_no_schema() {
        // new() leaves summary_schema as None
        let llm: Arc<dyn Llm> = Arc::new(NoopLlm);
        let extractor = SummaryExtractor::new(llm);
        assert!(extractor.summary_schema.is_none());
    }

    #[test]
    fn new_with_schema_stores_schema() {
        let llm: Arc<dyn Llm> = Arc::new(NoopLlm);
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } }
        });
        let extractor = SummaryExtractor::new_with_schema(llm, Some(schema.clone()));
        assert_eq!(extractor.summary_schema, Some(schema));
    }

    #[test]
    fn validate_summary_schema_accepts_valid() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } }
        });
        assert!(validate_summary_schema(&schema).is_ok());
    }

    #[test]
    fn validate_summary_schema_rejects_missing_summary() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "other_field": { "type": "string" } }
        });
        assert!(validate_summary_schema(&schema).is_err());
    }

    #[test]
    fn validate_summary_schema_rejects_non_string_summary() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "integer" } }
        });
        assert!(validate_summary_schema(&schema).is_err());
    }

    #[test]
    fn validate_summary_schema_rejects_non_object() {
        let schema = serde_json::json!([1, 2, 3]);
        assert!(validate_summary_schema(&schema).is_err());
    }

    // Minimal no-op LLM for structural tests only.
    struct NoopLlm;

    #[async_trait::async_trait]
    impl Llm for NoopLlm {
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _options: Option<cognee_llm::types::GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
            unimplemented!()
        }
        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<cognee_llm::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<cognee_llm::types::GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            unimplemented!()
        }
        fn model(&self) -> &str {
            "noop"
        }
        fn max_context_length(&self) -> u32 {
            4096
        }
    }
}
