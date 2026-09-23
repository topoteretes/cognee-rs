use std::fs;

use crate::types::SearchError;

pub const DEFAULT_RAG_SYSTEM_PROMPT: &str =
    "Answer the question using the provided context. Be as brief as possible.";
pub const DEFAULT_RAG_USER_PROMPT_TEMPLATE: &str =
    "The question is: `{question}`\nAnd here is the context: `{context}`";
pub const DEFAULT_GRAPH_USER_PROMPT_TEMPLATE: &str = "The question is: `{question}`\nand here is the context provided with a set of relationships from a knowledge graph separated by \\n---\\n each represented as node1 -- relation -- node2 triplet: `{context}`";
pub const DEFAULT_HYBRID_USER_PROMPT_TEMPLATE: &str = "The question is: `{question}`\nAnswer using this sectioned context. Keep the answer brief and do not use information outside the context.\n\nContext:\n`{context}`";

/// The hybrid answer prompt for a model whose window the context had to be
/// budgeted against.
///
/// Identical to [`DEFAULT_HYBRID_USER_PROMPT_TEMPLATE`] but with the question
/// repeated **after** the context, and the repetition is load-bearing on a
/// small model. The context is the whole prompt by volume, so with the
/// question only at the top, generation begins thousands of tokens away from
/// anything that says what is being asked. Measured on Gemma 3 1B with ~2,500
/// tokens of context: asked "Who is Alice" over four passages of Alice
/// dialogue and a graph section, it described the graph section — the text
/// nearest the answer — instead of answering the question. Restating the
/// question immediately before the answer is the standard fix and costs a few
/// tokens.
///
/// It also **drops the "Keep the answer brief" instruction**, which the
/// default carries and which stacks with the system prompt's own "Be as brief
/// as possible". Two brevity instructions plus a restated question is enough
/// to talk a 1B model out of answering at all: asked "Who is Alice" over four
/// passages of dialogue it replied, in full, `Alice`. One instruction about
/// length is a preference; two is a competition the content loses.
///
/// Kept separate rather than folded into the default so the unbudgeted path
/// stays verbatim-identical to Python's `hybrid_context_for_question.txt`: a
/// hosted model with a large window has neither the problem nor a reason to
/// diverge.
pub const HYBRID_SMALL_WINDOW_USER_PROMPT_TEMPLATE: &str = "The question is: `{question}`\nAnswer using this sectioned context. Do not use information outside the context.\n\nContext:\n`{context}`\n\nNow answer this question in two or three sentences, using only the context above: `{question}`";

/// The system prompt that goes with
/// [`HYBRID_SMALL_WINDOW_USER_PROMPT_TEMPLATE`].
///
/// [`DEFAULT_RAG_SYSTEM_PROMPT`] is shared with every other retriever and with
/// the cloud path, and a parity test pins it to Python's wording, so the half
/// of the doubled brevity instruction that lives there cannot be changed
/// where it stands. This replaces it for the budgeted path only. The
/// difference that matters is the last sentence: told merely to be brief, the
/// model answers `Alice`; told to answer from the context and to say when the
/// context does not contain the answer, it has something to do other than be
/// short.
pub const HYBRID_SMALL_WINDOW_SYSTEM_PROMPT: &str = "Answer the question using only the provided context. Quote or paraphrase \
     the context rather than adding what you already know. If the context does \
     not contain the answer, say so.";

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
    fn hybrid_user_prompt_template_matches_python() {
        // Verbatim parity with hybrid_context_for_question.txt (modulo the
        // Jinja2 `{{ }}` -> Rust `{ }` placeholder syntax used across all
        // DEFAULT_*_USER_PROMPT_TEMPLATE constants). Backticks are literal.
        assert_eq!(
            DEFAULT_HYBRID_USER_PROMPT_TEMPLATE,
            "The question is: `{question}`\nAnswer using this sectioned context. Keep the answer brief and do not use information outside the context.\n\nContext:\n`{context}`"
        );

        // render_user_prompt substitutes both placeholders correctly.
        let rendered = render_user_prompt(
            Some(DEFAULT_HYBRID_USER_PROMPT_TEMPLATE),
            "Who knows Bob?",
            "Alice knows Bob.",
        );
        assert!(rendered.contains("The question is: `Who knows Bob?`"));
        assert!(rendered.contains("Context:\n`Alice knows Bob.`"));
        assert!(!rendered.contains("{question}"));
        assert!(!rendered.contains("{context}"));
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
