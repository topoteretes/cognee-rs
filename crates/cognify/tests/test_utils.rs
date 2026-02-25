use cognee_llm::OpenAIAdapter;
use std::sync::Arc;

pub fn require_env(var_name: &str) -> String {
    std::env::var(var_name)
        .unwrap_or_else(|_| panic!("❌ Required environment variable '{}' is not set", var_name))
}

#[allow(dead_code)]
pub fn create_adapter_from_env() -> Arc<OpenAIAdapter> {
    let base_url = require_env("OPENAI_URL");
    let api_token = require_env("OPENAI_TOKEN");
    let model = require_env("OPENAI_MODEL");

    Arc::new(
        OpenAIAdapter::new(model, api_token, Some(base_url))
            .unwrap_or_else(|e| panic!("❌ Failed to create OpenAI adapter: {}", e)),
    )
}
