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

use tracing::warn;

use crate::{LlmError, LlmResult, OpenAIAdapter};

/// Default OpenAI-compatible base URL for a local Ollama server.
const OLLAMA_DEFAULT_ENDPOINT: &str = "http://localhost:11434/v1";
/// Default Mistral OpenAI-compatible base URL.
const MISTRAL_DEFAULT_ENDPOINT: &str = "https://api.mistral.ai/v1";
/// Default Gemini OpenAI-compatible base URL.
const GEMINI_DEFAULT_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/openai/";

/// Known litellm *provider* prefixes that may appear as the leading
/// `<provider>/` segment of a configured model name.
///
/// Python cognee routes every LLM call through litellm, whose model strings are
/// provider-qualified (e.g. `openai/gpt-4o-mini`, `baseten/openai/gpt-oss-120b`).
/// litellm strips the leading provider segment and sends the remainder to that
/// provider's endpoint. We mirror that: [`strip_litellm_provider_prefix`] removes
/// exactly one leading provider segment so the slug sent on the wire matches what
/// litellm would send. Crucially only the *first* segment is stripped — for
/// `baseten/openai/gpt-oss-120b` we strip `baseten/` and keep `openai/gpt-oss-120b`,
/// because `openai/` is part of Baseten's real model slug (org "openai"), exactly
/// as litellm does.
///
/// This is a hand-maintained *subset* of litellm's full provider registry —
/// enough to cover the providers users actually configure here. An unrecognised
/// prefix is simply left in place (the model is sent verbatim), so the worst case
/// for a missing entry is a provider-qualified slug reaching the wire unchanged,
/// not a wrong strip. Add entries as needed.
const LITELLM_PROVIDER_PREFIXES: &[&str] = &[
    "openai",
    "azure",
    "azure_ai",
    "anthropic",
    "baseten",
    "ollama",
    "mistral",
    "gemini",
    "vertex_ai",
    "bedrock",
    "cohere",
    "groq",
    "together_ai",
    "fireworks_ai",
    "deepinfra",
    "deepseek",
    "anyscale",
    "perplexity",
    "xai",
    "openrouter",
    "cerebras",
    "sambanova",
    "nvidia_nim",
    "watsonx",
    "replicate",
    "huggingface",
];

/// Strip a single leading litellm provider segment (`<provider>/`) from `model`
/// when `<provider>` is a recognised litellm provider (see
/// [`LITELLM_PROVIDER_PREFIXES`]). Only the first segment is removed, matching
/// litellm's routing: `baseten/openai/gpt-oss-120b` → `openai/gpt-oss-120b`,
/// `openai/gpt-4o-mini` → `gpt-4o-mini`, `gpt-4o-mini` (no prefix) → unchanged.
///
/// Returns the remaining slug and, when a prefix was removed, the recognised
/// prefix itself so the caller can flag a provider mismatch.
fn strip_litellm_provider_prefix(model: &str) -> (&str, Option<&str>) {
    if let Some((head, rest)) = model.split_once('/')
        && !rest.is_empty()
        && LITELLM_PROVIDER_PREFIXES.contains(&head)
    {
        return (rest, Some(head));
    }
    (model, None)
}

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
/// litellm-style provider prefixes on the model (`openai/`, `baseten/`,
/// `ollama/`, `mistral/`, `gemini/`, `groq/`, …) are stripped so
/// provider-qualified config values work exactly as they do under Python's
/// litellm. Only the first `<provider>/` segment is removed, so a real slug that
/// itself contains a slash survives — e.g. `baseten/openai/gpt-oss-120b` becomes
/// `openai/gpt-oss-120b` (Baseten's org is literally "openai"). `custom` /
/// `openai_compatible` pass the model verbatim.
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

    // Per provider: resolved base URL and whether to strip a leading litellm
    // provider prefix from the model. `custom` / `openai_compatible` pass the
    // model verbatim (the user supplied the exact slug their endpoint expects);
    // every other provider strips one litellm provider segment for parity with
    // Python's litellm routing.
    let (base_url, strip): (Option<String>, bool) = match provider.as_str() {
        // Empty endpoint → None so the adapter uses its OpenAI default.
        "openai" => (non_empty(endpoint), true),
        "ollama" => (Some(endpoint_or(endpoint, OLLAMA_DEFAULT_ENDPOINT)), true),
        "mistral" => (Some(endpoint_or(endpoint, MISTRAL_DEFAULT_ENDPOINT)), true),
        "gemini" => (Some(endpoint_or(endpoint, GEMINI_DEFAULT_ENDPOINT)), true),
        "custom" | "openai_compatible" => {
            if endpoint.is_empty() {
                return Err(LlmError::ConfigError(format!(
                    "llm_endpoint must be configured for provider '{provider}'"
                )));
            }
            (Some(endpoint.to_string()), false)
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

    let model = if strip {
        let (stripped, prefix) = strip_litellm_provider_prefix(model);
        // A stripped prefix that differs from the configured provider is a
        // configuration smell: e.g. `provider=openai` + `model=anthropic/claude-3`
        // would send `claude-3` to the OpenAI endpoint and 404. We still strip
        // (this is exactly how the legitimate Baseten config
        // `provider=openai` + `model=baseten/openai/gpt-oss-120b` works — the
        // `baseten/` prefix is litellm routing metadata, not the OpenAI provider),
        // but warn so a genuine mismatch is visible rather than silently mangled.
        if let Some(prefix) = prefix
            && prefix != provider
        {
            warn!(
                configured_provider = %provider,
                stripped_prefix = %prefix,
                original_model = %model,
                wire_model = %stripped,
                "litellm model prefix does not match configured llm_provider; stripping it \
                 and sending the remainder to the configured endpoint. If this is a real \
                 mismatch, drop the prefix from llm_model or set llm_provider=custom.",
            );
        }
        stripped
    } else {
        model
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
    fn openai_strips_only_first_litellm_provider_segment() {
        // litellm-style Baseten config: provider=openai, model carries the
        // litellm provider prefix `baseten/`. We must strip only `baseten/` and
        // keep `openai/gpt-oss-120b` (Baseten's real slug — org "openai").
        let adapter = build_openai_compatible_adapter(
            "openai",
            "baseten/openai/gpt-oss-120b",
            "sk-test",
            "https://inference.baseten.co/v1",
            3,
        )
        .expect("adapter should build");
        assert_eq!(adapter.model(), "openai/gpt-oss-120b");

        // A bare `openai/` prefix against real OpenAI is stripped to the slug.
        let adapter =
            build_openai_compatible_adapter("openai", "openai/gpt-4o-mini", "sk-test", "", 3)
                .expect("adapter should build");
        assert_eq!(adapter.model(), "gpt-4o-mini");
    }

    #[test]
    fn strip_reports_the_removed_prefix_for_mismatch_detection() {
        // #3: the stripper surfaces which recognised prefix it removed so the
        // builder can warn on a provider mismatch.
        assert_eq!(
            strip_litellm_provider_prefix("anthropic/claude-3"),
            ("claude-3", Some("anthropic"))
        );
        assert_eq!(
            strip_litellm_provider_prefix("baseten/openai/gpt-oss-120b"),
            ("openai/gpt-oss-120b", Some("baseten"))
        );
        assert_eq!(
            strip_litellm_provider_prefix("gpt-4o-mini"),
            ("gpt-4o-mini", None)
        );
        // An unrecognised prefix is left in place (sent verbatim).
        assert_eq!(
            strip_litellm_provider_prefix("acme/model"),
            ("acme/model", None)
        );
    }

    #[test]
    fn provider_mismatch_still_strips_and_builds() {
        // #3: `provider=openai` + a non-openai litellm prefix still strips (the
        // legitimate Baseten config relies on this) and builds; the mismatch is
        // surfaced via a `warn!`, not by mangling silently or failing.
        let adapter =
            build_openai_compatible_adapter("openai", "anthropic/claude-3", "sk-test", "", 3)
                .expect("adapter should build");
        assert_eq!(adapter.model(), "claude-3");
    }

    #[test]
    fn custom_provider_never_strips_prefix() {
        // A `custom`/`openai_compatible` endpoint gets the model verbatim, even
        // when it looks like a litellm provider prefix — the user gave the exact
        // slug their endpoint expects.
        let adapter = build_openai_compatible_adapter(
            "custom",
            "openai/gpt-oss-120b",
            "sk-test",
            "https://inference.baseten.co/v1",
            3,
        )
        .expect("adapter should build");
        assert_eq!(adapter.model(), "openai/gpt-oss-120b");
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
