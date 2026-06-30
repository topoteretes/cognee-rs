//! Shared construction of OpenAI-compatible LLM adapters.
//!
//! The embedded component manager (`cognee-lib`) and the standalone HTTP server
//! (`cognee-http-server`) both wire the LLM the same way: an [`OpenAIAdapter`]
//! built from the configured model / key / endpoint, with structured-output and
//! network retries applied. Centralising that here keeps the two wiring paths in
//! sync (see issue #17).
//!
//! Several providers in the Python SDK are themselves OpenAI-compatible HTTP
//! endpoints, so they need only factory routing — not a new adapter. This module
//! routes `openai`, `ollama`, `mistral`, `gemini`, and `custom` /
//! `openai_compatible` onto the same [`OpenAIAdapter`], differing only in the base
//! URL, whether an API key is mandatory, and litellm-style model-prefix stripping.
//! The OpenAI-only request quirks in the adapter are gated on the
//! `api.openai.com` host, so pointing it at another compatible endpoint does not
//! trigger any OpenAI-specific behaviour.

use crate::{LlmError, LlmResult, OpenAIAdapter};

/// Default OpenAI-compatible base URL for a local Ollama server.
const OLLAMA_DEFAULT_ENDPOINT: &str = "http://localhost:11434/v1";
/// Default Mistral OpenAI-compatible base URL.
const MISTRAL_DEFAULT_ENDPOINT: &str = "https://api.mistral.ai/v1";
/// Default Gemini OpenAI-compatible base URL.
const GEMINI_DEFAULT_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/openai/";

/// Providers this factory routes onto [`OpenAIAdapter`]. Single source of truth so
/// the "unsupported provider" message can never drift from the match arms below.
const SUPPORTED_PROVIDERS: &[&str] = &[
    "openai",
    "ollama",
    "mistral",
    "gemini",
    "custom",
    "openai_compatible",
];

/// Build an [`OpenAIAdapter`] for an OpenAI-compatible `provider`.
///
/// Supported providers (case-insensitive): `openai`, `ollama`, `mistral`,
/// `gemini`, and `custom` / `openai_compatible`.
///
/// `endpoint` is the raw configured value; an empty or whitespace-only string is
/// treated as "unset" so the provider's default base URL is used (or, for
/// `openai`, the adapter's built-in OpenAI default). `custom` /
/// `openai_compatible` has no default and requires an explicit endpoint.
///
/// `max_retries` is floored at 1 and applied to both the structured-output and
/// network retry loops, matching the previous inline wiring.
///
/// litellm-style provider prefixes on the model (`ollama/`, `mistral/`,
/// `gemini/`) are stripped so provider-qualified config values keep working
/// (the adapter itself only strips `openai/`).
///
/// Returns [`LlmError::ConfigError`] when a required API key or endpoint is
/// missing, or the provider is unsupported, so each caller can decide whether to
/// hard-fail (component manager) or skip and wire `None` (HTTP server).
pub fn build_openai_compatible_adapter(
    provider: &str,
    model: &str,
    api_key: &str,
    endpoint: &str,
    max_retries: u32,
) -> LlmResult<OpenAIAdapter> {
    let retries = max_retries.max(1);
    let provider = provider.to_ascii_lowercase();
    let endpoint = endpoint.trim();

    // Per provider: resolved base URL and the litellm-style model prefix to strip.
    let (base_url, strip_prefix): (Option<String>, Option<&str>) = match provider.as_str() {
        // Empty endpoint → None so the adapter uses its OpenAI default.
        "openai" => (non_empty(endpoint), None),
        "ollama" => (
            Some(endpoint_or(endpoint, OLLAMA_DEFAULT_ENDPOINT)),
            Some("ollama/"),
        ),
        "mistral" => (
            Some(endpoint_or(endpoint, MISTRAL_DEFAULT_ENDPOINT)),
            Some("mistral/"),
        ),
        "gemini" => (
            Some(endpoint_or(endpoint, GEMINI_DEFAULT_ENDPOINT)),
            Some("gemini/"),
        ),
        "custom" | "openai_compatible" => {
            if endpoint.is_empty() {
                return Err(LlmError::ConfigError(format!(
                    "llm_endpoint must be configured for provider '{provider}'"
                )));
            }
            (Some(endpoint.to_string()), None)
        }
        other => {
            return Err(LlmError::ConfigError(format!(
                "Unsupported llm_provider '{other}'. Supported: {}.",
                SUPPORTED_PROVIDERS.join(", ")
            )));
        }
    };

    // Every supported provider requires an API key, matching the Python SDK's
    // _API_KEY_REQUIRED_PROVIDERS (openai, ollama, mistral, gemini, custom).
    if api_key.is_empty() {
        return Err(LlmError::ConfigError(
            "llm_api_key must be configured".to_string(),
        ));
    }

    let model = match strip_prefix {
        Some(prefix) => model.strip_prefix(prefix).unwrap_or(model),
        None => model,
    };

    let adapter = OpenAIAdapter::new(model.to_string(), api_key.to_string(), base_url)?
        .with_structured_output_retries(retries)
        .with_network_retries(retries);
    Ok(adapter)
}

/// `Some(endpoint)` unless it is empty (already trimmed by the caller).
fn non_empty(endpoint: &str) -> Option<String> {
    if endpoint.is_empty() {
        None
    } else {
        Some(endpoint.to_string())
    }
}

/// The configured endpoint, or `default` when it is empty (already trimmed).
fn endpoint_or(endpoint: &str, default: &str) -> String {
    if endpoint.is_empty() {
        default.to_string()
    } else {
        endpoint.to_string()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::Llm;

    #[test]
    fn builds_openai_adapter_and_strips_provider_prefix() {
        let adapter =
            build_openai_compatible_adapter("openai", "openai/gpt-4o-mini", "sk-test", "", 3)
                .expect("adapter should build");
        // OpenAIAdapter strips the leading `openai/` litellm-style prefix.
        assert_eq!(adapter.model(), "gpt-4o-mini");
    }

    #[test]
    fn openai_requires_api_key() {
        let result = build_openai_compatible_adapter("openai", "gpt-4o-mini", "", "", 3);
        assert!(matches!(result, Err(LlmError::ConfigError(_))));
    }

    #[test]
    fn provider_matching_is_case_insensitive() {
        let adapter = build_openai_compatible_adapter("OpenAI", "gpt-4o-mini", "sk-test", "", 1)
            .expect("adapter should build");
        assert_eq!(adapter.model(), "gpt-4o-mini");
    }

    #[test]
    fn unsupported_provider_errors() {
        let result = build_openai_compatible_adapter("acme", "model", "key", "", 3);
        assert!(matches!(result, Err(LlmError::ConfigError(_))));
    }

    #[test]
    fn ollama_defaults_endpoint_and_strips_prefix() {
        // No endpoint → the Ollama default is used; the `ollama/` prefix is stripped.
        let adapter =
            build_openai_compatible_adapter("ollama", "ollama/llama3.1:8b", "sk-test", "", 3)
                .expect("ollama adapter should build");
        assert_eq!(adapter.model(), "llama3.1:8b");
    }

    #[test]
    fn ollama_honors_custom_endpoint() {
        let adapter = build_openai_compatible_adapter(
            "ollama",
            "llama3.1:8b",
            "sk-test",
            "http://remote:11434/v1",
            3,
        )
        .expect("ollama adapter should build");
        assert_eq!(adapter.model(), "llama3.1:8b");
    }

    #[test]
    fn ollama_requires_api_key() {
        // Parity with Python's _API_KEY_REQUIRED_PROVIDERS: ollama requires a key.
        let result = build_openai_compatible_adapter("ollama", "llama3.1:8b", "", "", 3);
        assert!(matches!(result, Err(LlmError::ConfigError(_))));
    }

    #[test]
    fn mistral_requires_key_and_strips_prefix() {
        let missing =
            build_openai_compatible_adapter("mistral", "mistral/mistral-large", "", "", 3);
        assert!(matches!(missing, Err(LlmError::ConfigError(_))));

        let adapter = build_openai_compatible_adapter(
            "mistral",
            "mistral/mistral-large-latest",
            "sk-test",
            "",
            3,
        )
        .expect("mistral adapter should build");
        assert_eq!(adapter.model(), "mistral-large-latest");
    }

    #[test]
    fn gemini_requires_key_and_strips_prefix() {
        let adapter =
            build_openai_compatible_adapter("gemini", "gemini/gemini-2.0-flash", "sk-test", "", 3)
                .expect("gemini adapter should build");
        assert_eq!(adapter.model(), "gemini-2.0-flash");
    }

    #[test]
    fn custom_requires_endpoint() {
        let missing = build_openai_compatible_adapter("custom", "my-model", "sk-test", "", 3);
        assert!(matches!(missing, Err(LlmError::ConfigError(_))));

        let adapter = build_openai_compatible_adapter(
            "openai_compatible",
            "my-model",
            "sk-test",
            "https://my.host/v1",
            3,
        )
        .expect("custom adapter should build");
        // No prefix stripping for custom — the model is passed through verbatim.
        assert_eq!(adapter.model(), "my-model");
    }

    #[test]
    fn custom_requires_api_key() {
        // Parity with Python's _API_KEY_REQUIRED_PROVIDERS: custom requires a key.
        let result =
            build_openai_compatible_adapter("custom", "my-model", "", "https://my.host/v1", 3);
        assert!(matches!(result, Err(LlmError::ConfigError(_))));
    }
}
