use std::fs;

use crate::types::SearchError;

pub const DEFAULT_RAG_SYSTEM_PROMPT: &str = "You are a helpful assistant. Answer the user question using the provided context. If the context is insufficient, say what is missing.";
pub const DEFAULT_RAG_USER_PROMPT_TEMPLATE: &str = "Question:\n{question}\n\nContext:\n{context}";
pub const DEFAULT_GRAPH_USER_PROMPT_TEMPLATE: &str = "The question is: `{question}`\nand here is the context provided with a set of relationships from a knowledge graph separated by \\n---\\n each represented as node1 -- relation -- node2 triplet: `{context}`";

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

/// Renders the user prompt for graph-based retrievers.
///
/// If a custom template is provided, it is used. Otherwise, the
/// `DEFAULT_GRAPH_USER_PROMPT_TEMPLATE` is used (matching Python's
/// `graph_context_for_question.txt`).
pub fn render_graph_user_prompt(template: Option<&str>, question: &str, context: &str) -> String {
    template
        .unwrap_or(DEFAULT_GRAPH_USER_PROMPT_TEMPLATE)
        .replace("{question}", question)
        .replace("{context}", context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_graph_user_prompt_default_template() {
        let result = render_graph_user_prompt(None, "Who knows Bob?", "Alice --[KNOWS]--> Bob");

        assert!(result.contains("The question is: `Who knows Bob?`"));
        assert!(result.contains("knowledge graph"));
        assert!(result.contains("Alice --[KNOWS]--> Bob"));
        // Verify literal \n---\n is present (not actual newlines)
        assert!(result.contains("\\n---\\n"));
    }

    #[test]
    fn render_graph_user_prompt_custom_template() {
        let result = render_graph_user_prompt(
            Some("Q={question} C={context}"),
            "test question",
            "test context",
        );

        assert_eq!(result, "Q=test question C=test context");
    }

    #[test]
    fn render_user_prompt_uses_rag_template_by_default() {
        let result = render_user_prompt(None, "question", "context");

        assert!(result.contains("Question:\nquestion"));
        assert!(result.contains("Context:\ncontext"));
        // Should NOT contain graph-specific text
        assert!(!result.contains("knowledge graph"));
    }

    #[test]
    fn graph_and_rag_templates_are_different() {
        let graph = render_graph_user_prompt(None, "q", "c");
        let rag = render_user_prompt(None, "q", "c");

        assert_ne!(graph, rag);
    }
}
