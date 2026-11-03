use cognee_llm::Message;
use cognee_session::SessionContext;

/// Build the LLM message list a completion sends (Python's
/// `build_completion_prompts`).
///
/// The system prompt is the retriever's task template, nothing else. The user
/// prompt is the session history, then the rendered question and context,
/// then the `guidance` block last; empty layers are skipped and the rest are
/// joined by a blank line.
pub fn build_messages_with_history(
    system_prompt: String,
    user_prompt: String,
    session: &SessionContext,
    guidance: &str,
) -> Vec<Message> {
    let user_prompt = [
        session.formatted_history.as_str(),
        user_prompt.as_str(),
        guidance,
    ]
    .into_iter()
    .filter(|layer| !layer.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");

    vec![Message::system(system_prompt), Message::user(user_prompt)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_session::SessionContext;

    const HISTORY: &str = "Previous conversation:\n\nQUESTION: hi\nANSWER: hello\n\n";

    #[test]
    fn no_session_layers_leave_the_prompts_unchanged() {
        let messages = build_messages_with_history(
            "system instructions".to_string(),
            "user question".to_string(),
            &SessionContext::default(),
            "",
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "system instructions");
        assert_eq!(messages[1].content, "user question");
    }

    #[test]
    fn history_leads_and_guidance_trails_the_user_prompt() {
        let session = SessionContext {
            session_id: Some("s1".to_string()),
            formatted_history: HISTORY.to_string(),
            ..Default::default()
        };
        let messages = build_messages_with_history(
            "You are a helpful assistant.".to_string(),
            "What is Rust?".to_string(),
            &session,
            "## Active session guidance",
        );
        assert_eq!(messages[0].content, "You are a helpful assistant.");
        assert_eq!(
            messages[1].content,
            format!("{HISTORY}\n\nWhat is Rust?\n\n## Active session guidance")
        );
    }

    #[test]
    fn guidance_alone_follows_the_user_prompt() {
        let messages = build_messages_with_history(
            "sys".to_string(),
            "q".to_string(),
            &SessionContext::default(),
            "guide",
        );
        assert_eq!(messages[1].content, "q\n\nguide");
    }
}
