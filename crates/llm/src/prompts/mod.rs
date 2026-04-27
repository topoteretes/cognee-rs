//! Compiled-in LLM prompt templates ported from Python.
//!
//! Mirrors [`cognee/infrastructure/llm/prompts/`](https://github.com/topoteretes/cognee/tree/main/cognee/infrastructure/llm/prompts).
//! Filenames are kept identical for cross-SDK diffing.
//!
//! Templates use `{{KEY}}` placeholders. Only flat substitution is supported —
//! the Python prompts use a tiny subset of Jinja that this is compatible with.

use std::collections::HashMap;

use thiserror::Error;

// ─── Compiled-in prompt files ─────────────────────────────────────────────────

pub const CUSTOM_PROMPT_GENERATION_USER: &str = include_str!("custom_prompt_generation_user.txt");
pub const CUSTOM_PROMPT_GENERATION_SYSTEM: &str =
    include_str!("custom_prompt_generation_system.txt");
pub const INFER_SCHEMA_USER: &str = include_str!("infer_schema_user.txt");
pub const INFER_SCHEMA_SYSTEM: &str = include_str!("infer_schema_system.txt");

// ─── PromptError ──────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum PromptError {
    #[error("unknown prompt template: {0}")]
    UnknownTemplate(String),
}

// ─── render_prompt ────────────────────────────────────────────────────────────

/// Render the named prompt template with the given context map.
///
/// Returns the prompt string with every `{{KEY}}` placeholder replaced by the
/// corresponding `ctx[key]` value. Keys not in `ctx` are left as-is so a
/// missing context entry surfaces visibly in the rendered prompt rather than
/// silently swallowed.
///
/// Recognised template names: `custom_prompt_generation_user`,
/// `custom_prompt_generation_system`, `infer_schema_user`, `infer_schema_system`.
pub fn render_prompt(name: &str, ctx: &HashMap<&str, &str>) -> Result<String, PromptError> {
    let raw = match name {
        "custom_prompt_generation_user" => CUSTOM_PROMPT_GENERATION_USER,
        "custom_prompt_generation_system" => CUSTOM_PROMPT_GENERATION_SYSTEM,
        "infer_schema_user" => INFER_SCHEMA_USER,
        "infer_schema_system" => INFER_SCHEMA_SYSTEM,
        other => return Err(PromptError::UnknownTemplate(other.to_string())),
    };

    let mut out = raw.to_string();
    for (key, value) in ctx {
        let needle = format!("{{{{{}}}}}", key);
        out = out.replace(&needle, value);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_prompt_user_renders_graph_schema() {
        let mut ctx = HashMap::new();
        ctx.insert("GRAPH_SCHEMA_JSON", "{\"entity_types\": []}");
        let out = render_prompt("custom_prompt_generation_user", &ctx).expect("render");
        assert!(out.contains("{\"entity_types\": []}"));
        assert!(!out.contains("{{GRAPH_SCHEMA_JSON}}"));
    }

    #[test]
    fn test_custom_prompt_system_loads() {
        let ctx = HashMap::new();
        let out = render_prompt("custom_prompt_generation_system", &ctx).expect("render");
        assert!(!out.is_empty());
    }

    #[test]
    fn test_infer_schema_user_renders_sample_text() {
        let mut ctx = HashMap::new();
        ctx.insert("SAMPLE_TEXT", "Alice met Bob.");
        let out = render_prompt("infer_schema_user", &ctx).expect("render");
        assert!(out.contains("Alice met Bob."));
        assert!(!out.contains("{{SAMPLE_TEXT}}"));
    }

    #[test]
    fn test_infer_schema_system_loads() {
        let ctx = HashMap::new();
        let out = render_prompt("infer_schema_system", &ctx).expect("render");
        assert!(!out.is_empty());
    }

    #[test]
    fn test_unknown_template_errors() {
        let ctx = HashMap::new();
        let err = render_prompt("does_not_exist", &ctx).unwrap_err();
        match err {
            PromptError::UnknownTemplate(name) => assert_eq!(name, "does_not_exist"),
        }
    }
}
