//! Summarization data models.
//!
//! Port of Python's:
//! - cognee/tasks/summarization/models.py (TextSummary)
//! - cognee/shared/data_models.py (SummarizedContent)

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// LLM output model for summarized content.
///
/// This is the structured output format expected from the LLM.
/// Used as the response_model in extract_summary calls.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SummarizedContent {
    /// Brief summary of the content (1-2 sentences)
    pub summary: String,

    /// Detailed description with key information preserved
    pub description: String,
}

/// Text summary derived from a document chunk.
///
/// Represents a hierarchical summary that can be stored and retrieved.
/// Links back to the original chunk via chunk_id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSummary {
    /// Unique identifier (uuid5 based on chunk_id + "TextSummary")
    pub id: Uuid,

    /// The chunk this summary was generated from
    pub chunk_id: Uuid,

    /// The summary text
    pub text: String,

    /// Optional description (from SummarizedContent.description)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// The model used to generate this summary (e.g., "gpt-4", "llama3.2")
    pub model: String,
}

impl TextSummary {
    /// Create a new TextSummary with deterministic UUID v5 ID.
    ///
    /// # Arguments
    /// * `chunk_id` - UUID of the source DocumentChunk
    /// * `text` - Summary text
    /// * `description` - Optional detailed description
    /// * `model` - Model name used for generation
    ///
    /// # Returns
    /// A new TextSummary with uuid5(chunk_id, "TextSummary") as id
    pub fn new(chunk_id: Uuid, text: String, description: Option<String>, model: String) -> Self {
        // Deterministic ID: uuid5(chunk_id, "TextSummary")
        let id = Uuid::new_v5(&chunk_id, b"TextSummary");

        Self {
            id,
            chunk_id,
            text,
            description,
            model,
        }
    }

    /// Create from a chunk ID and SummarizedContent (LLM output).
    ///
    /// # Arguments
    /// * `chunk_id` - UUID of the source chunk
    /// * `summarized` - LLM-generated SummarizedContent
    /// * `model` - Model name
    pub fn from_summarized_content(
        chunk_id: Uuid,
        summarized: SummarizedContent,
        model: String,
    ) -> Self {
        Self::new(
            chunk_id,
            summarized.summary,
            Some(summarized.description),
            model,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_summary_deterministic_id() {
        let chunk_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

        let summary1 = TextSummary::new(
            chunk_id,
            "Test summary".to_string(),
            None,
            "gpt-4".to_string(),
        );

        let summary2 = TextSummary::new(
            chunk_id,
            "Different text".to_string(),
            None,
            "gpt-3.5-turbo".to_string(),
        );

        // Same chunk_id should produce same summary id (deterministic)
        assert_eq!(summary1.id, summary2.id);

        // Different chunk_id should produce different summary id
        let different_chunk_id = Uuid::new_v4();
        let summary3 = TextSummary::new(
            different_chunk_id,
            "Test summary".to_string(),
            None,
            "gpt-4".to_string(),
        );
        assert_ne!(summary1.id, summary3.id);
    }

    #[test]
    fn test_from_summarized_content() {
        let chunk_id = Uuid::new_v4();
        let summarized = SummarizedContent {
            summary: "Brief summary".to_string(),
            description: "Detailed description with key points.".to_string(),
        };

        let text_summary = TextSummary::from_summarized_content(
            chunk_id,
            summarized.clone(),
            "llama3".to_string(),
        );

        assert_eq!(text_summary.chunk_id, chunk_id);
        assert_eq!(text_summary.text, summarized.summary);
        assert_eq!(text_summary.description, Some(summarized.description));
        assert_eq!(text_summary.model, "llama3");
        assert_eq!(text_summary.id, Uuid::new_v5(&chunk_id, b"TextSummary"));
    }

    #[test]
    fn test_serialization() {
        let chunk_id = Uuid::new_v4();
        let summary = TextSummary::new(
            chunk_id,
            "Summary text".to_string(),
            Some("Description".to_string()),
            "gpt-4".to_string(),
        );

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: TextSummary = serde_json::from_str(&json).unwrap();

        assert_eq!(summary.id, deserialized.id);
        assert_eq!(summary.chunk_id, deserialized.chunk_id);
        assert_eq!(summary.text, deserialized.text);
        assert_eq!(summary.description, deserialized.description);
        assert_eq!(summary.model, deserialized.model);
    }
}
