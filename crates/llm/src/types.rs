//! Common types for LLM operations.

use serde::{Deserialize, Serialize};

/// Message role in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// A message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
        }
    }
}

/// Default output-token ceiling (`llm_max_completion_tokens`) shared across the
/// SDK: `GenerationOptions::default`, the config defaults in `cognee-lib` and
/// `cognee-http-server`, and the Anthropic adapter's fallback. Kept in one place
/// so all of them move in lockstep. The per-request value is still clamped to
/// each model's documented cap (see `AnthropicAdapter::effective_max_tokens`).
pub const DEFAULT_MAX_COMPLETION_TOKENS: u32 = 16384;

/// Options for LLM generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationOptions {
    /// Temperature for sampling (0.0 = deterministic, 1.0 = creative).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Maximum number of tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Top-p sampling parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Frequency penalty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    /// Presence penalty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// Stop sequences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

impl Default for GenerationOptions {
    fn default() -> Self {
        Self {
            temperature: Some(0.0),
            max_tokens: Some(DEFAULT_MAX_COMPLETION_TOKENS),
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
        }
    }
}

/// Response from LLM generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationResponse {
    /// Generated text content.
    pub content: String,

    /// Model used for generation.
    pub model: String,

    /// Token usage information.
    pub usage: Option<TokenUsage>,

    /// Finish reason (e.g., "stop", "length", "content_filter").
    pub finish_reason: Option<String>,
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Which structured-output request shape the OpenAI-compatible adapter may send.
///
/// The adapter's default behaviour is a three-mode cascade — native `tools`,
/// then legacy `functions`, then `response_format: {"type": "json_object"}` —
/// because an OpenAI-compatible endpoint may accept any one of the three and
/// reject the others, and nothing in the protocol advertises which. The cascade
/// is a Rust-only mechanism: Python pins one instructor mode per provider from a
/// static table (`instructor_modes.py`) and exposes `llm_instructor_mode` to
/// override it. This knob is the counterpart of that override.
///
/// [`Self::Auto`] keeps the cascade, bounded per mode by the adapter's own miss
/// probe. Every other variant **pins** one shape: the other two are never sent,
/// and exhausting the pinned mode is terminal rather than falling through.
///
/// Pinning is the cheaper answer whenever the operator already knows what the
/// endpoint speaks. A vLLM deployment started without
/// `--enable-auto-tool-choice --tool-call-parser` answers no tool call at all,
/// and the miss probe still has to spend its threshold rediscovering that in
/// every fresh process — while a pin costs nothing and is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StructuredOutputMode {
    /// Try each mode in cascade order, skipping any the miss probe has tripped.
    /// The default, and the behaviour before this knob existed.
    #[default]
    Auto,
    /// Only native `tools`.
    Tools,
    /// Only the legacy `functions` / `function_call` pair.
    Functions,
    /// Only `response_format: {"type": "json_object"}`.
    Json,
}

impl StructuredOutputMode {
    /// Whether native tool-calling may be sent.
    pub fn allows_tools(self) -> bool {
        matches!(self, Self::Auto | Self::Tools)
    }

    /// Whether the legacy `functions` shape may be sent.
    pub fn allows_functions(self) -> bool {
        matches!(self, Self::Auto | Self::Functions)
    }

    /// Whether JSON mode may be sent.
    ///
    /// Under [`Self::Auto`] this is always true: JSON mode is the cascade's
    /// terminal fallback and the one mode with no miss probe.
    pub fn allows_json(self) -> bool {
        matches!(self, Self::Auto | Self::Json)
    }

    /// Whether a single mode is pinned, i.e. the cascade is disabled.
    ///
    /// Used to phrase an exhaustion error honestly: under a pin there is no
    /// further mode to fall through to, so the message should name the pinned
    /// mode rather than implying the whole cascade ran.
    pub fn is_pinned(self) -> bool {
        !matches!(self, Self::Auto)
    }

    /// The knob spelling, for log and error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Tools => "tools",
            Self::Functions => "functions",
            Self::Json => "json",
        }
    }
}
