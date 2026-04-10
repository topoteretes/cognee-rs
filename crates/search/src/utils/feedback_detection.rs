use cognee_llm::{Llm, Message, generate_json_schema};

use crate::types::FeedbackDetectionResult;

const FEEDBACK_DETECTION_SYSTEM_PROMPT: &str = "\
You are an AI assistant that analyzes user messages to determine if they contain \
feedback about a previous AI response. Your task is to detect explicit or implicit \
feedback such as corrections, quality evaluations, or expressions of satisfaction/dissatisfaction.

Analyze the user message and determine:
1. Whether it contains feedback about a previous response
2. What the feedback text is (if present)
3. A quality score from 1 (very negative) to 5 (very positive), if inferable
4. A polite acknowledgment response for the user (if feedback was detected)
5. Whether the message also contains a follow-up question

Return a JSON object matching the required schema.";

/// Detect whether a user message contains feedback about a previous response.
///
/// Uses the LLM with structured output. On failure (LLM error or parse error),
/// returns `FeedbackDetectionResult::no_feedback()` so the main search is never blocked.
pub async fn detect_feedback(llm: &dyn Llm, user_message: &str) -> FeedbackDetectionResult {
    if user_message.trim().is_empty() {
        return FeedbackDetectionResult::no_feedback();
    }

    let schema = generate_json_schema::<FeedbackDetectionResult>();

    let messages = vec![
        Message::system(FEEDBACK_DETECTION_SYSTEM_PROMPT.to_string()),
        Message::user(user_message.to_string()),
    ];

    match llm
        .create_structured_output_with_messages_raw(messages, &schema, None)
        .await
    {
        Ok(value) => {
            serde_json::from_value(value).unwrap_or_else(|_| FeedbackDetectionResult::no_feedback())
        }
        Err(_) => FeedbackDetectionResult::no_feedback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_feedback_default_is_correct() {
        let result = FeedbackDetectionResult::no_feedback();
        assert!(!result.feedback_detected);
        assert!(!result.contains_followup_question);
        assert!(result.feedback_text.is_none());
    }
}
