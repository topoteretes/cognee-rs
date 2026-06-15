use cognee_llm::{Llm, Message, generate_json_schema};

use crate::types::FeedbackDetectionResult;

/// System prompt for conversational feedback detection.
///
/// Vendored byte-for-byte from Python's
/// `cognee/infrastructure/llm/prompts/feedback_detection_system.txt`. Kept in sync
/// via the prompt-parity drift guard.
const FEEDBACK_DETECTION_SYSTEM_PROMPT: &str =
    include_str!("prompts/feedback_detection_system.txt");

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

    #[test]
    fn feedback_prompt_matches_vendored_txt() {
        let vendored = include_str!("prompts/feedback_detection_system.txt");
        assert_eq!(
            FEEDBACK_DETECTION_SYSTEM_PROMPT, vendored,
            "const drifted from vendored .txt"
        );
        assert!(
            vendored.contains("Set feedback_detected to true ONLY"),
            "Python specificity marker missing"
        );
        assert!(
            vendored.contains("response_to_user:"),
            "response_to_user field marker missing"
        );
    }
}
