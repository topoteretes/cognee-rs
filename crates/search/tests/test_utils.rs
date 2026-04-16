use cognee_llm::OpenAIAdapter;
use std::sync::Arc;

/// Read a required environment variable, loading `.env` first (idempotent).
///
/// Accepts Python-compatible canonical names (`LLM_API_KEY`, `LLM_ENDPOINT`,
/// `LLM_MODEL`) as fallbacks for the legacy Rust test aliases (`OPENAI_TOKEN`,
/// `OPENAI_URL`, `OPENAI_MODEL`) so that a single `.env` with the canonical
/// names works for both the CLI and integration tests.
pub fn require_env(var_name: &str) -> String {
    let _ = dotenv::dotenv();

    // Legacy alias → canonical fallback mapping
    let canonical_fallback = match var_name {
        "OPENAI_TOKEN" => Some("LLM_API_KEY"),
        "OPENAI_URL" => Some("LLM_ENDPOINT"),
        "OPENAI_MODEL" => Some("LLM_MODEL"),
        _ => None,
    };

    if let Ok(v) = std::env::var(var_name)
        && !v.is_empty()
    {
        return v;
    }
    if let Some(canonical) = canonical_fallback
        && let Ok(v) = std::env::var(canonical)
        && !v.is_empty()
    {
        return v;
    }
    panic!("❌ Required environment variable '{var_name}' is not set")
}

pub fn create_adapter_from_env() -> Arc<OpenAIAdapter> {
    let base_url = require_env("OPENAI_URL");
    let api_token = require_env("OPENAI_TOKEN");
    let model = std::env::var("LLM_MODEL")
        .or_else(|_| std::env::var("OPENAI_MODEL"))
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());

    Arc::new(
        OpenAIAdapter::new(model, api_token, Some(base_url))
            .unwrap_or_else(|e| panic!("❌ Failed to create OpenAI adapter: {e}")),
    )
}

/// Extract the embedding model directory from `COGNEE_E2E_EMBED_MODEL_PATH`.
pub fn get_embedding_model_dir() -> String {
    if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_string_lossy().to_string();
    }
    "./target/models".to_string()
}
