//! Summarization data models.
//!
//! Port of Python's:
//! - cognee/tasks/summarization/models.py (TextSummary)
//! - cognee/shared/data_models.py (SummarizedContent)

use cognee_models::DataPoint;
use cognee_models::HasDataPoint;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
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
/// Extends DataPoint (matching Python's `TextSummary(DataPoint)`).
/// Links back to the original chunk via `made_from`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSummary {
    /// Base DataPoint fields (id, timestamps, metadata, etc.)
    #[serde(flatten)]
    pub base: DataPoint,

    /// The chunk this summary was generated from (matches Python's `made_from: DocumentChunk`)
    pub made_from: Option<Uuid>,

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

        let mut base = DataPoint::new("TextSummary", None);
        base.id = id;
        base.metadata
            .insert("index_fields".to_string(), json!(["text"]));

        Self {
            base,
            made_from: Some(chunk_id),
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

impl HasDataPoint for TextSummary {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    // for_each_child_mut: default no-op — TextSummary's `made_from`
    // reference is a `Option<Uuid>`, not an owned child.
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
        assert_eq!(summary1.base.id, summary2.base.id);

        // Different chunk_id should produce different summary id
        let different_chunk_id = Uuid::new_v4();
        let summary3 = TextSummary::new(
            different_chunk_id,
            "Test summary".to_string(),
            None,
            "gpt-4".to_string(),
        );
        assert_ne!(summary1.base.id, summary3.base.id);
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

        assert_eq!(text_summary.made_from, Some(chunk_id));
        assert_eq!(text_summary.text, summarized.summary);
        assert_eq!(text_summary.description, Some(summarized.description));
        assert_eq!(text_summary.model, "llama3");
        assert_eq!(
            text_summary.base.id,
            Uuid::new_v5(&chunk_id, b"TextSummary")
        );
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

        assert_eq!(summary.base.id, deserialized.base.id);
        assert_eq!(summary.made_from, deserialized.made_from);
        assert_eq!(summary.text, deserialized.text);
        assert_eq!(summary.description, deserialized.description);
        assert_eq!(summary.model, deserialized.model);
    }

    #[test]
    fn test_data_point_base_fields() {
        let chunk_id = Uuid::new_v4();
        let summary = TextSummary::new(
            chunk_id,
            "Test summary".to_string(),
            None,
            "gpt-4".to_string(),
        );

        // Verify DataPoint base fields are properly set
        assert_eq!(summary.base.data_type, "TextSummary");
        assert_eq!(
            summary.base.metadata.get("index_fields"),
            Some(&json!(["text"]))
        );
        assert!(summary.base.created_at > 0);
        assert!(summary.base.updated_at > 0);
        assert_eq!(summary.base.version, 1);
    }

    #[test]
    fn text_summary_implements_has_datapoint() {
        let chunk_id = Uuid::new_v4();
        let summary = TextSummary::new(
            chunk_id,
            "Summary text".to_string(),
            None,
            "gpt-4".to_string(),
        );
        let dp_id = summary.base.id;
        assert_eq!(summary.data_point().id, dp_id);
        let mut s2 = summary;
        assert_eq!(s2.data_point_mut().id, dp_id);
    }
}
