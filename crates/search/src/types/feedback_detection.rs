use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Result of automatic feedback detection on a user query.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct FeedbackDetectionResult {
    /// Whether the user message contains feedback about a previous response.
    pub feedback_detected: bool,
    /// The extracted feedback text, if any.
    pub feedback_text: Option<String>,
    /// Feedback quality score on a 1–5 scale, if extracted.
    pub feedback_score: Option<f32>,
    /// A suggested response to the user acknowledging the feedback.
    pub response_to_user: Option<String>,
    /// Whether the message also contains a follow-up question.
    pub contains_followup_question: bool,
}

impl FeedbackDetectionResult {
    pub fn no_feedback() -> Self {
        Self {
            feedback_detected: false,
            contains_followup_question: false,
            ..Default::default()
        }
    }
}
