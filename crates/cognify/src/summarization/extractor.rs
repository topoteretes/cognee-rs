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

/// Default system prompt for text summarization.
///
/// Based on Python's prompts/summarize_content.txt.
/// Instructs the LLM to create brief, concise summaries while preserving key information.
const DEFAULT_SUMMARY_PROMPT: &str = r#"You are a top-tier summarization engine. Your task is to summarize text and make it versatile.
Be brief and concise, but keep the important information and the subject.
Use synonym words where possible in order to change the wording but keep the meaning."#;

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
}

impl SummaryExtractor {
    /// Create a new summary extractor with the given LLM.
    ///
    /// # Arguments
    /// * `llm` - An LLM implementation (e.g., OpenAIAdapter, OllamaAdapter)
    ///
    /// # Returns
    /// A new SummaryExtractor instance
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }

    /// Extract a summary from text.
    ///
    /// Mirrors Python's `extract_summary` function.
    /// Uses the LLM to generate a SummarizedContent (summary + description).
    ///
    /// # Arguments
    /// * `text` - Input text to summarize
    /// * `custom_prompt` - Optional custom system prompt (uses DEFAULT_SUMMARY_PROMPT if None)
    ///
    /// # Returns
    /// A SummarizedContent containing summary and description
    ///
    /// # Errors
    /// Returns CognifyError::LlmError if the LLM call fails
    pub async fn extract_summary(
        &self,
        text: &str,
        custom_prompt: Option<&str>,
    ) -> Result<SummarizedContent, CognifyError> {
        let system_prompt = custom_prompt.unwrap_or(DEFAULT_SUMMARY_PROMPT);

        let summarized: SummarizedContent = self
            .llm
            .create_structured_output(
                text,
                system_prompt,
                Some(GenerationOptions {
                    temperature: Some(0.3), // Slightly creative for paraphrasing
                    max_tokens: Some(500),  // Summaries should be brief
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| CognifyError::LlmError(e.to_string()))?;

        Ok(summarized)
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
            let prompt_clone = custom_prompt.clone();
            let text = chunk.text.clone();

            let task = tokio::spawn(async move {
                let extractor = SummaryExtractor { llm: llm_clone };
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

    // Note: Tests that require LLM are in integration tests (tests/)
    // These are just structural tests

    #[test]
    fn test_default_prompt_not_empty() {
        assert!(!DEFAULT_SUMMARY_PROMPT.is_empty());
        assert!(DEFAULT_SUMMARY_PROMPT.contains("summarization"));
    }
}
