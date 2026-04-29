//! DTOs for `/api/v1/settings/*` per `routers/settings.md §4`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

// ── Selectable provider/model lists ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigChoice {
    pub value: String,
    pub label: String,
}

// ── GET response ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LLMConfigOutputDTO {
    pub provider: String,
    pub model: String,
    pub endpoint: Option<String>,
    pub api_version: Option<String>,
    pub api_key: Option<String>,
    pub providers: Vec<ConfigChoice>,
    pub models: BTreeMap<String, Vec<ConfigChoice>>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VectorDBConfigOutputDTO {
    pub provider: String,
    pub url: String,
    pub api_key: String,
    pub providers: Vec<ConfigChoice>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDTO {
    pub llm: LLMConfigOutputDTO,
    pub vector_db: VectorDBConfigOutputDTO,
}

// ── POST request body ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LLMConfigInputDTO {
    pub provider: LlmProvider,
    pub model: String,
    #[serde(alias = "api_key")]
    pub api_key: String,
}

/// Provider enum for `LLMConfigInputDTO::provider`. Note that `bedrock`
/// is **not** in this list — Python's GET advertises it but the save
/// `Literal` rejects it (`routers/settings.md §6.4`).
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LlmProvider {
    Openai,
    Ollama,
    Anthropic,
    Gemini,
    Mistral,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VectorDBConfigInputDTO {
    pub provider: VectorDbProvider,
    pub url: String,
    #[serde(alias = "api_key")]
    pub api_key: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VectorDbProvider {
    Lancedb,
    Chromadb,
    Pgvector,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPayloadDTO {
    #[serde(default)]
    pub llm: Option<LLMConfigInputDTO>,
    #[serde(default, alias = "vector_db")]
    pub vector_db: Option<VectorDBConfigInputDTO>,
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Mirrors Python's `(key[0:10] + "*" * (len(key) - 10)) if key else None`.
///
/// - `None` / empty → `None` (Python returns the empty-key short-circuit).
/// - Up to 10 chars → return the key as-is (no stars).
/// - Longer → first 10 chars + `(len - 10)` stars.
pub fn redact_api_key(key: Option<&str>) -> Option<String> {
    let key = key.filter(|k| !k.is_empty())?;
    let len = key.len();
    if len <= 10 {
        // No stars; return as-is.
        return Some(key.to_string());
    }
    let mut head = String::with_capacity(len);
    head.push_str(&key[..10]);
    head.push_str(&"*".repeat(len - 10));
    Some(head)
}

/// Mirrors Python's `'*****' not in key and len(key.strip()) > 0` guard.
pub fn should_persist_api_key(submitted: &str) -> bool {
    !submitted.contains("*****") && !submitted.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_empty_returns_none() {
        assert_eq!(redact_api_key(None), None);
        assert_eq!(redact_api_key(Some("")), None);
    }

    #[test]
    fn redact_short_key_returns_as_is() {
        assert_eq!(redact_api_key(Some("short")), Some("short".into()));
    }

    #[test]
    fn redact_long_key_masks_tail() {
        let r = redact_api_key(Some("sk-1234567890ABC")).expect("some");
        // 10 chars + 6 stars
        assert_eq!(r, "sk-1234567******");
    }

    #[test]
    fn should_persist_rejects_empty() {
        assert!(!should_persist_api_key(""));
        assert!(!should_persist_api_key("   "));
    }

    #[test]
    fn should_persist_rejects_redacted() {
        assert!(!should_persist_api_key("sk-prefix*****abc"));
        assert!(!should_persist_api_key("AAAAAAAAAA*****"));
    }

    #[test]
    fn should_persist_accepts_real_key() {
        assert!(should_persist_api_key("sk-real-key"));
    }
}
