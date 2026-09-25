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
/// SDK: the config defaults in `cognee-lib` and `cognee-http-server`, the
/// Anthropic adapter's fallback, and the cap the OpenAI adapter substitutes on
/// *option-less* structured-output calls. Kept in one place so all of them move
/// in lockstep. The per-request value is still clamped to each model's
/// documented cap (see `AnthropicAdapter::effective_max_tokens`).
///
/// Deliberately *not* the value of [`GenerationOptions::default`]'s
/// `max_tokens`: a default sitting in that field is indistinguishable from a
/// budget the caller chose, and the truncation-recovery path has to tell those
/// apart. See that impl.
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
    /// `max_tokens` defaults to `None` — "no budget of my own".
    ///
    /// It carried `Some(DEFAULT_MAX_COMPLETION_TOKENS)` until SDK-581, which made
    /// `Some(_)` ambiguous. `GenerationOptions { temperature: Some(0.1),
    /// ..Default::default() }` handed the adapter a 16384 the caller never
    /// picked, and the OpenAI truncation-recovery path — which refuses to raise a
    /// budget the caller *chose* — read that as a deliberate constraint and
    /// failed the call terminally, naming a number nobody had asked for.
    ///
    /// With `None` here, `Some(n)` is only ever a value a caller wrote, so the
    /// adapter carries intent instead of inferring it from `Option`. Both
    /// option-less paths still apply a cap of their own and are unchanged:
    /// `OpenAIAdapter::resolve_options` substitutes the configured
    /// `default_max_tokens` for `generate`, and the structured-output path
    /// substitutes [`DEFAULT_MAX_COMPLETION_TOKENS`].
    ///
    /// This does change behaviour for one spelling: a caller who builds options
    /// with `..Default::default()` and never sets `max_tokens` now sends no cap,
    /// so the provider's own default applies rather than 16384. That is the
    /// documented meaning of an explicit `max_tokens: None`, and it matches
    /// Python parity — `acreate_structured_output` passes no cap either.
    fn default() -> Self {
        Self {
            temperature: Some(0.0),
            max_tokens: None,
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
/// probe. [`Self::Tools`], [`Self::Functions`] and [`Self::Json`] each **pin**
/// one shape: the other two are never sent, and exhausting the pinned mode is
/// terminal rather than falling through.
///
/// Pinning is the cheaper answer whenever the operator already knows what the
/// endpoint speaks. A vLLM deployment started without
/// `--enable-auto-tool-choice --tool-call-parser` answers no tool call at all,
/// and the miss probe still has to spend its threshold rediscovering that in
/// every fresh process — while a pin costs nothing and is exact.
///
/// [`Self::JsonSchema`] is the odd one out: it adds a *fourth* shape ahead of
/// the cascade rather than choosing among the three, and it is not a pin. See
/// its docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
// `snake_case` rather than `lowercase` so `JsonSchema` spells itself
// `json_schema`. The other four variants are single words, so their wire
// spelling is unchanged.
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputMode {
    /// Try each mode in cascade order, skipping any the miss probe has tripped.
    /// The default, and the behaviour before this knob existed.
    #[default]
    Auto,
    /// **Prefer** constrained decoding — `response_format: {"type":
    /// "json_schema", "json_schema": {"strict": true, …}}` — ahead of the
    /// cascade, and demote out of it when the endpoint says no.
    ///
    /// The opt-in half of SDK-630. Python reaches the same request shape on its
    /// default `litellm_native` path, but only for a model litellm's table
    /// advertises as `supports_response_schema`; Rust has no such table for an
    /// arbitrary OpenAI-compatible base URL, so the operator supplies the
    /// knowledge instead — this knob. (The Bedrock adapter *does* have the
    /// table, in `adapters::bedrock::caps`, and picks its own native
    /// `outputConfig` branch from it without consulting this enum.)
    ///
    /// **Not a pin, deliberately.** The other three values exclude everything
    /// else and make exhaustion terminal; this one is a *preference* with a
    /// demotion ladder — strict → non-strict → out of the mode entirely, at
    /// which point the ordinary `tools` → `functions` → `json` cascade runs and
    /// behaves exactly as under [`Self::Auto`]. A hard pin here would be
    /// unusable on the fleet this exists for: Baseten's `gpt-oss-120b` answers
    /// HTTP 501 to a constrained request, so pinning would fail every call
    /// rather than costing one probe. The demotion is memoised per schema, so
    /// an endpoint that refuses pays once per distinct schema per process.
    JsonSchema,
    /// Only native `tools`.
    Tools,
    /// Only the legacy `functions` / `function_call` pair.
    Functions,
    /// Only `response_format: {"type": "json_object"}`.
    Json,
}

impl StructuredOutputMode {
    /// Whether the constrained `response_format: json_schema` shape may be sent.
    ///
    /// False under [`Self::Auto`]: unlike the other three, this mode is not
    /// something an unknown endpoint can be probed for cheaply — the shapes that
    /// fail do so with a hard HTTP error rather than an unusable 200, and the
    /// SDK's own default deployment target is one of them. Opting in is the
    /// operator's call.
    pub fn allows_json_schema(self) -> bool {
        matches!(self, Self::JsonSchema)
    }

    /// Whether native tool-calling may be sent.
    pub fn allows_tools(self) -> bool {
        matches!(self, Self::Auto | Self::Tools | Self::JsonSchema)
    }

    /// Whether the legacy `functions` shape may be sent.
    pub fn allows_functions(self) -> bool {
        matches!(self, Self::Auto | Self::Functions | Self::JsonSchema)
    }

    /// Whether JSON mode may be sent.
    ///
    /// Under [`Self::Auto`] this is always true: JSON mode is the cascade's
    /// terminal fallback and the one mode with no miss probe.
    pub fn allows_json(self) -> bool {
        matches!(self, Self::Auto | Self::Json | Self::JsonSchema)
    }

    /// Whether a single mode is pinned, i.e. the cascade is disabled.
    ///
    /// Used to phrase an exhaustion error honestly: under a pin there is no
    /// further mode to fall through to, so the message should name the pinned
    /// mode rather than implying the whole cascade ran. It also switches off the
    /// miss probes, which exist only to choose *between* modes.
    ///
    /// False for [`Self::JsonSchema`], which leaves the whole cascade available
    /// behind it — every one of those uses wants the cascade's behaviour there.
    pub fn is_pinned(self) -> bool {
        matches!(self, Self::Tools | Self::Functions | Self::Json)
    }

    /// The knob spelling, for log and error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::JsonSchema => "json_schema",
            Self::Tools => "tools",
            Self::Functions => "functions",
            Self::Json => "json",
        }
    }
}
