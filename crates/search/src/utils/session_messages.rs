use cognee_llm::Message;
use cognee_session::SessionContext;

/// Build the full LLM message list, prepending session history to the system
/// prompt when present, followed by the current user prompt.
///
/// When `session.formatted_history` is non-empty, it is prepended to the
/// system prompt with a `\nTASK:` separator so the LLM sees prior context
/// before the current instructions. When history is empty, the system prompt
/// is passed through unchanged.
pub fn build_messages_with_history(
    system_prompt: String,
    user_prompt: String,
    session: &SessionContext,
) -> Vec<Message> {
    let effective_system_prompt = if session.formatted_history.is_empty() {
        system_prompt
    } else {
        format!("{}\nTASK:{}", session.formatted_history, system_prompt)
    };

    vec![
        Message::system(effective_system_prompt),
        Message::user(user_prompt),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_session::SessionContext;

    #[test]
    fn empty_history_passes_system_prompt_unchanged() {
        let messages = build_messages_with_history(
            "system instructions".to_string(),
            "user question".to_string(),
            &SessionContext::default(),
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "system instructions");
        assert_eq!(messages[1].content, "user question");
    }

    #[test]
    fn history_prepended_to_system_prompt_with_task_separator() {
        let session = SessionContext {
            session_id: Some("s1".to_string()),
            history: vec![],
            formatted_history: "Previous conversation:\n\nQUESTION: hi\nANSWER: hello\n\n"
                .to_string(),
            graph_context: None,
        };
        let messages = build_messages_with_history(
            "You are a helpful assistant.".to_string(),
            "What is Rust?".to_string(),
            &session,
        );
        assert_eq!(messages.len(), 2);
        assert!(messages[0].content.starts_with("Previous conversation:"));
        assert!(
            messages[0]
                .content
                .contains("\nTASK:You are a helpful assistant.")
        );
        assert_eq!(messages[1].content, "What is Rust?");
    }
}
