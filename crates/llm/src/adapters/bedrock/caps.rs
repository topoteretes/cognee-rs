//! Per-model capability and limit table — the Rust stand-in for litellm's
//! `model_prices_and_context_window.json` (plan §1.4.3 / §4 R3 step 2).
//!
//! Three flags drive real wire differences:
//!
//! * `supports_native_structured_output` selects between Converse's
//!   `outputConfig.textFormat` (native) and the synthetic `json_tool_call`
//!   tool (fallback) — see [`super::converse::apply_structured_output`];
//! * `supports_tool_choice` decides whether the fallback branch may *force*
//!   `toolConfig.toolChoice`. `amazon.nova-lite-v1:0` does **not** advertise
//!   it and is documented to reject a specific `toolChoice`, so forcing it
//!   unconditionally 400s one of the three models cognee ships;
//! * `max_output_tokens` is the cap `inferenceConfig.maxTokens` is clamped to
//!   (§1.0).
//!
//! Against cognee's three shipped models this resolves exactly as plan §1.4.3's
//! table says:
//!
//! | Model | Branch | `toolChoice` forced? |
//! |---|---|---|
//! | `eu.anthropic.claude-sonnet-4-5-20250929-v1:0` | native `outputConfig` | n/a |
//! | `eu.anthropic.claude-haiku-4-5-20251001-v1:0` | native `outputConfig` | n/a |
//! | `eu.amazon.nova-lite-v1:0` | `json_tool_call` | **no** |
//!
//! # Keying
//!
//! Entries are keyed on the §1.4.1-**normalised** id and matched **exactly**,
//! the way litellm keys `model_cost`. Do not reuse
//! `AnthropicAdapter::model_max_output_tokens`
//! (`crates/llm/src/adapters/anthropic.rs`): it substring-matches `claude-*`
//! names and never matches a Bedrock id such as
//! `anthropic.claude-sonnet-4-5-20250929-v1:0`.
//!
//! # Maintenance
//!
//! Hand-maintained, sourced from litellm's
//! `model_prices_and_context_window.json`. Adding a model is a data edit here.
//! An id that is not listed falls back to [`UNKNOWN_MODEL_CAPS`].

use tracing::debug;

/// Capabilities and limits for one Bedrock model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelCaps {
    /// Model accepts Converse's native `outputConfig.textFormat` structured
    /// output. When `false`, structured output falls back to the synthetic
    /// `json_tool_call` tool.
    pub supports_native_structured_output: bool,
    /// Model accepts a forced `toolConfig.toolChoice`.
    pub supports_tool_choice: bool,
    /// Documented output-token cap (`max_output_tokens`).
    pub max_output_tokens: u32,
    /// Documented context window (`max_input_tokens`).
    pub max_input_tokens: u32,
    /// Model accepts Converse `image` content blocks.
    pub supports_vision: bool,
}

/// Caps applied to a model absent from [`MODEL_CAPS`].
///
/// Deliberately **conservative**, per plan §4 R3 step 2: no native structured
/// output and no forced tool choice, because guessing `true` for either
/// produces a request the model rejects — a terminal 400 — while guessing
/// `false` only picks the more widely-supported shape. The 4 096-token output
/// cap is the floor of the Bedrock converse families (Llama 3.1 caps at 2 048,
/// Claude 3 and Llama 3.2 at 4 096), so an unlisted model under-budgets its
/// output until the table below is refreshed rather than 400ing on
/// `maxTokens > model limit`.
pub const UNKNOWN_MODEL_CAPS: ModelCaps = ModelCaps {
    supports_native_structured_output: false,
    supports_tool_choice: false,
    max_output_tokens: 4_096,
    max_input_tokens: 128_000,
    supports_vision: false,
};

/// Shorthand for a table row.
const fn caps(
    supports_native_structured_output: bool,
    supports_tool_choice: bool,
    max_output_tokens: u32,
    max_input_tokens: u32,
    supports_vision: bool,
) -> ModelCaps {
    ModelCaps {
        supports_native_structured_output,
        supports_tool_choice,
        max_output_tokens,
        max_input_tokens,
        supports_vision,
    }
}

/// Exact-match capability table, keyed on the normalised model id.
///
/// Columns: `supports_native_structured_output`, `supports_tool_choice`,
/// `max_output_tokens`, `max_input_tokens`, `supports_vision` — every value
/// copied from litellm's `model_prices_and_context_window.json`.
pub const MODEL_CAPS: &[(&str, ModelCaps)] = &[
    // ---- Anthropic (the two ids cognee ships are the first two rows) ----
    (
        "anthropic.claude-sonnet-4-5-20250929-v1:0",
        caps(true, true, 64_000, 200_000, true),
    ),
    (
        "anthropic.claude-haiku-4-5-20251001-v1:0",
        caps(true, true, 64_000, 200_000, true),
    ),
    (
        "anthropic.claude-opus-4-5-20251101-v1:0",
        caps(true, true, 64_000, 200_000, true),
    ),
    (
        "anthropic.claude-opus-4-6-v1",
        caps(true, true, 128_000, 1_000_000, true),
    ),
    // Not in litellm's pricing file under this exact spelling, but it is in
    // BEDROCK_CONVERSE_MODELS; same model as the row above.
    (
        "anthropic.claude-opus-4-6-v1:0",
        caps(true, true, 128_000, 1_000_000, true),
    ),
    (
        "anthropic.claude-opus-4-7",
        caps(true, true, 128_000, 1_000_000, true),
    ),
    (
        "anthropic.claude-opus-4-8",
        caps(true, true, 128_000, 1_000_000, true),
    ),
    (
        "anthropic.claude-opus-5",
        caps(true, true, 128_000, 1_000_000, true),
    ),
    (
        "anthropic.claude-sonnet-5",
        caps(true, true, 128_000, 1_000_000, true),
    ),
    (
        "anthropic.claude-fable-5",
        caps(true, true, 128_000, 1_000_000, true),
    ),
    (
        "anthropic.claude-sonnet-4-6",
        caps(true, true, 64_000, 1_000_000, true),
    ),
    (
        "anthropic.claude-sonnet-4-20250514-v1:0",
        caps(false, true, 64_000, 1_000_000, true),
    ),
    (
        "anthropic.claude-opus-4-20250514-v1:0",
        caps(false, true, 32_000, 200_000, true),
    ),
    (
        "anthropic.claude-opus-4-1-20250805-v1:0",
        caps(false, true, 32_000, 200_000, true),
    ),
    (
        "anthropic.claude-3-7-sonnet-20250219-v1:0",
        caps(false, true, 8_192, 200_000, true),
    ),
    (
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
        caps(false, true, 8_192, 1_000_000, true),
    ),
    (
        "anthropic.claude-3-5-sonnet-20240620-v1:0",
        caps(false, true, 4_096, 1_000_000, true),
    ),
    (
        "anthropic.claude-3-5-haiku-20241022-v1:0",
        caps(false, true, 8_192, 200_000, false),
    ),
    (
        "anthropic.claude-3-opus-20240229-v1:0",
        caps(false, true, 4_096, 200_000, true),
    ),
    (
        "anthropic.claude-3-sonnet-20240229-v1:0",
        caps(false, true, 4_096, 200_000, true),
    ),
    (
        "anthropic.claude-3-haiku-20240307-v1:0",
        caps(false, true, 4_096, 200_000, true),
    ),
    (
        "anthropic.claude-v2:1",
        caps(false, true, 8_191, 100_000, false),
    ),
    // Absent from the pricing file; the v2 and v2:1 checkpoints share limits.
    (
        "anthropic.claude-v2",
        caps(false, true, 8_191, 100_000, false),
    ),
    (
        "anthropic.claude-v1",
        caps(false, false, 8_191, 100_000, false),
    ),
    (
        "anthropic.claude-instant-v1",
        caps(false, true, 8_191, 100_000, false),
    ),
    // ---- Amazon Nova (nova-lite is the third id cognee ships) ----
    (
        "amazon.nova-lite-v1:0",
        caps(false, false, 10_000, 300_000, true),
    ),
    (
        "amazon.nova-pro-v1:0",
        caps(false, false, 10_000, 300_000, true),
    ),
    (
        "amazon.nova-micro-v1:0",
        caps(false, false, 10_000, 128_000, false),
    ),
    (
        "amazon.nova-2-lite-v1:0",
        caps(false, false, 64_000, 1_000_000, true),
    ),
    (
        "amazon.nova-2-pro-preview-20251202-v1:0",
        caps(false, false, 64_000, 1_000_000, true),
    ),
    // ---- Meta Llama 3.x ----
    (
        "meta.llama3-70b-instruct-v1:0",
        caps(false, false, 8_192, 8_192, false),
    ),
    (
        "meta.llama3-8b-instruct-v1:0",
        caps(false, false, 8_192, 8_192, false),
    ),
    (
        "meta.llama3-1-8b-instruct-v1:0",
        caps(false, false, 2_048, 128_000, false),
    ),
    (
        "meta.llama3-1-70b-instruct-v1:0",
        caps(false, false, 2_048, 128_000, false),
    ),
    (
        "meta.llama3-1-405b-instruct-v1:0",
        caps(false, false, 4_096, 128_000, false),
    ),
    (
        "meta.llama3-2-1b-instruct-v1:0",
        caps(false, false, 4_096, 128_000, false),
    ),
    (
        "meta.llama3-2-3b-instruct-v1:0",
        caps(false, false, 4_096, 128_000, false),
    ),
    (
        "meta.llama3-2-11b-instruct-v1:0",
        caps(false, false, 4_096, 128_000, true),
    ),
    (
        "meta.llama3-2-90b-instruct-v1:0",
        caps(false, false, 4_096, 128_000, true),
    ),
    // ---- Mistral ----
    (
        "mistral.mistral-large-2407-v1:0",
        caps(false, true, 8_191, 128_000, false),
    ),
    (
        "mistral.mistral-large-2402-v1:0",
        caps(false, false, 8_191, 32_000, false),
    ),
    (
        "mistral.mistral-small-2402-v1:0",
        caps(false, false, 8_191, 32_000, false),
    ),
    // ---- DeepSeek / Qwen / OpenAI OSS ----
    ("deepseek.v3-v1:0", caps(true, true, 81_920, 163_840, false)),
    ("deepseek.v3.2", caps(true, true, 163_840, 163_840, false)),
    (
        "qwen.qwen3-coder-480b-a35b-v1:0",
        caps(true, true, 65_536, 262_000, false),
    ),
    (
        "qwen.qwen3-235b-a22b-2507-v1:0",
        caps(true, true, 131_072, 262_144, false),
    ),
    (
        "qwen.qwen3-coder-30b-a3b-v1:0",
        caps(true, true, 131_072, 262_144, false),
    ),
    (
        "qwen.qwen3-32b-v1:0",
        caps(true, true, 16_384, 131_072, false),
    ),
    (
        "qwen.qwen3-coder-next",
        caps(false, true, 8_192, 262_144, false),
    ),
    (
        "openai.gpt-oss-20b-1:0",
        caps(false, true, 128_000, 128_000, false),
    ),
    (
        "openai.gpt-oss-120b-1:0",
        caps(false, true, 128_000, 128_000, false),
    ),
    // ---- AI21 Jamba ----
    (
        "ai21.jamba-instruct-v1:0",
        caps(false, false, 4_096, 70_000, false),
    ),
    (
        "ai21.jamba-1-5-mini-v1:0",
        caps(false, false, 256_000, 256_000, false),
    ),
    (
        "ai21.jamba-1-5-large-v1:0",
        caps(false, false, 256_000, 256_000, false),
    ),
    // ---- Writer / MiniMax / Moonshot ----
    (
        "writer.palmyra-x4-v1:0",
        caps(false, false, 8_192, 128_000, false),
    ),
    (
        "writer.palmyra-x5-v1:0",
        caps(false, false, 8_192, 1_000_000, false),
    ),
    (
        "minimax.minimax-m2.1",
        caps(false, true, 8_192, 196_000, false),
    ),
    (
        "moonshotai.kimi-k2.5",
        caps(false, true, 262_144, 262_144, true),
    ),
];

/// Look up caps for an already-normalised base model id.
pub fn caps_for_base_model(base_model: &str) -> ModelCaps {
    match MODEL_CAPS.iter().find(|(id, _)| *id == base_model) {
        Some((_, caps)) => *caps,
        None => {
            // Surfaced at debug level so the conservative fallback is
            // diagnosable when tracing the LLM path, without warn-spamming a
            // per-request log line.
            debug!(
                base_model,
                "Bedrock model not in MODEL_CAPS; using the conservative capability defaults",
            );
            UNKNOWN_MODEL_CAPS
        }
    }
}

/// Look up caps for a raw model id, normalising it first (§1.4.1).
pub fn caps_for(model: &str) -> ModelCaps {
    caps_for_base_model(&super::model_id::base_model(model))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn every_table_key_is_already_normalised() {
        // A key that still carries a cross-region prefix, an ARN wrapper or a
        // throughput suffix could never be hit, because the lookup normalises
        // first. This pins the table against that silent dead entry.
        for (id, _) in MODEL_CAPS {
            assert_eq!(
                &super::super::model_id::base_model(id),
                id,
                "capability key {id} is not in normalised form",
            );
        }
    }

    #[test]
    fn unknown_ids_take_the_conservative_defaults() {
        let unknown = caps_for("eu.acme.some-future-model-v9:0");
        assert_eq!(unknown, UNKNOWN_MODEL_CAPS);
        assert!(!unknown.supports_native_structured_output);
        assert!(!unknown.supports_tool_choice);
    }
}
