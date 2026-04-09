use cognee_llm::Message;
use cognee_session::SessionContext;

/// Build the full LLM message list, injecting any session history between the
/// system prompt and the current user prompt.
pub fn build_messages_with_history(
    system_prompt: String,
    user_prompt: String,
    session: &SessionContext,
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(2 + session.history.len());
    messages.push(Message::system(system_prompt));
    messages.extend(session.history.iter().cloned());
    messages.push(Message::user(user_prompt));
    messages
}
