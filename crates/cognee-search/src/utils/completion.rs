use std::fs;

use crate::types::SearchError;

pub const DEFAULT_RAG_SYSTEM_PROMPT: &str = "You are a helpful assistant. Answer the user question using the provided context. If the context is insufficient, say what is missing.";
pub const DEFAULT_RAG_USER_PROMPT_TEMPLATE: &str = "Question:\n{question}\n\nContext:\n{context}";

pub fn resolve_system_prompt(
    system_prompt: Option<&str>,
    system_prompt_path: Option<&str>,
) -> Result<String, SearchError> {
    if let Some(path) = system_prompt_path {
        let prompt = fs::read_to_string(path).map_err(|error| {
            SearchError::InvalidInput(format!("failed to read system prompt path: {error}"))
        })?;
        return Ok(prompt);
    }

    if let Some(inline_prompt) = system_prompt {
        return Ok(inline_prompt.to_string());
    }

    Ok(DEFAULT_RAG_SYSTEM_PROMPT.to_string())
}

pub fn render_user_prompt(template: Option<&str>, question: &str, context: &str) -> String {
    template
        .unwrap_or(DEFAULT_RAG_USER_PROMPT_TEMPLATE)
        .replace("{question}", question)
        .replace("{context}", context)
}
