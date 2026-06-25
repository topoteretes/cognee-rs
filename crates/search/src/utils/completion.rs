use std::fs;

use crate::types::SearchError;

pub const DEFAULT_RAG_SYSTEM_PROMPT: &str =
    "Answer the question using the provided context. Be as brief as possible.";
pub const DEFAULT_RAG_USER_PROMPT_TEMPLATE: &str =
    "The question is: `{question}`\nAnd here is the context: `{context}`";
pub const DEFAULT_GRAPH_USER_PROMPT_TEMPLATE: &str = "The question is: `{question}`\nand here is the context provided with a set of relationships from a knowledge graph separated by \\n---\\n each represented as node1 -- relation -- node2 triplet: `{context}`";

pub fn resolve_system_prompt(
    system_prompt: Option<&str>,
    system_prompt_path: Option<&str>,
) -> Result<String, SearchError> {
    // Check inline prompt first (matches Python: `system_prompt if system_prompt else read_query_prompt(path)`)
    if let Some(inline_prompt) = system_prompt {
        return Ok(inline_prompt.to_string());
    }

    if let Some(path) = system_prompt_path {
        // The default config points `default_system_prompt_path` at the
        // conventional Python filename (e.g. `answer_simple_question.txt`),
        // but the Rust SDK ships its prompts compiled-in rather than as
        // on-disk files. Reading a bare filename therefore fails for every
        // entry point (CLI/HTTP/library) under the default config. Fall back
        // to the built-in default prompt — whose text matches Python's
        // `answer_simple_question.txt` — instead of failing the search, while
        // logging so a genuinely mistyped custom path is still visible.
        match fs::read_to_string(path) {
            Ok(prompt) => return Ok(prompt),
            Err(error) => {
                tracing::warn!(
                    system_prompt_path = path,
                    %error,
                    "system prompt path not readable; using built-in default prompt"
                );
                return Ok(DEFAULT_RAG_SYSTEM_PROMPT.to_string());
            }
        }
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
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

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

        assert!(result.contains("The question is: `question`"));
        assert!(result.contains("And here is the context: `context`"));
        // Should NOT contain graph-specific text
        assert!(!result.contains("knowledge graph"));
    }

    #[test]
    fn graph_and_rag_templates_are_different() {
        let graph = render_graph_user_prompt(None, "q", "c");
        let rag = render_user_prompt(None, "q", "c");

        assert_ne!(graph, rag);
    }

    #[test]
    fn resolve_system_prompt_inline_takes_priority_over_path() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "prompt from file").unwrap();
        let path = tmp.path().to_str().unwrap();

        let result = resolve_system_prompt(Some("inline prompt"), Some(path)).unwrap();

        assert_eq!(result, "inline prompt");
    }

    #[test]
    fn resolve_system_prompt_uses_path_when_inline_is_none() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "prompt from file").unwrap();
        let path = tmp.path().to_str().unwrap();

        let result = resolve_system_prompt(None, Some(path)).unwrap();

        assert_eq!(result, "prompt from file");
    }

    #[test]
    fn resolve_system_prompt_uses_default_when_both_are_none() {
        let result = resolve_system_prompt(None, None).unwrap();

        assert_eq!(result, DEFAULT_RAG_SYSTEM_PROMPT);
    }

    #[test]
    fn resolve_system_prompt_falls_back_to_default_for_missing_path() {
        // The default config points at the bundled-but-not-on-disk
        // `answer_simple_question.txt`; a missing path must not fail search.
        let result = resolve_system_prompt(None, Some("answer_simple_question.txt")).unwrap();

        assert_eq!(result, DEFAULT_RAG_SYSTEM_PROMPT);
    }
}
