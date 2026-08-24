//! §1.4.2 route selection: which Bedrock API a model id resolves to.
//!
//! Port of litellm's `BedrockModelInfo.get_bedrock_route`
//! (`llms/bedrock/common_utils.py`). The order matters:
//!
//! 1. an explicit route prefix wins (`converse/`, `invoke/`, `converse_like/`,
//!    `agent/`, `agentcore/`, `async_invoke/`, `openai/`, `claude_platform/`,
//!    `mantle/`), matched only as a **leading path segment**;
//! 2. the `nova/` / `nova-2/` custom-model spec prefixes → converse;
//! 3. an **application-inference-profile ARN** → converse (a common enterprise
//!    deployment shape: the ARN ends in an opaque id with no provider
//!    substring, so only the provider-free converse route can serve it);
//! 4. otherwise the §1.4.1-**normalised** id (or the merely prefix-stripped id)
//!    is looked up in [`BEDROCK_CONVERSE_MODELS`] → converse;
//! 5. else → invoke.
//!
//! Step 4 is the one that must not be skipped — see [`super::model_id`].
//!
//! # Scope
//!
//! Only [`BedrockRoute::Converse`] is implemented by this adapter. Per plan
//! §6.7 the legacy `/invoke` **chat** transforms are deliberately out of scope
//! ("no model cognee ships routes to invoke"), so an invoke-routed chat model
//! gets a clear [`crate::error::LlmError::FeatureNotSupported`] rather than a
//! silently wrong request. The InvokeModel *embedding* bodies are R4's, not
//! this module's.

/// Which Bedrock API a model id routes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BedrockRoute {
    /// `POST /model/{id}/converse` — the only route this adapter implements.
    Converse,
    /// `POST /model/{id}/invoke` — legacy per-family chat transforms, out of
    /// scope (plan §6.7).
    Invoke,
    /// litellm's `converse_like/` passthrough.
    ConverseLike,
    /// Bedrock Agents.
    Agent,
    /// Bedrock AgentCore.
    AgentCore,
    /// Asynchronous InvokeModel.
    AsyncInvoke,
    /// OpenAI-shaped Bedrock endpoint.
    OpenAi,
    /// Claude Platform on AWS.
    ClaudePlatform,
    /// bedrock-mantle Anthropic `/messages` endpoint.
    Mantle,
}

impl BedrockRoute {
    /// The route token as litellm spells it, for error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Converse => "converse",
            Self::Invoke => "invoke",
            Self::ConverseLike => "converse_like",
            Self::Agent => "agent",
            Self::AgentCore => "agentcore",
            Self::AsyncInvoke => "async_invoke",
            Self::OpenAi => "openai",
            Self::ClaudePlatform => "claude_platform",
            Self::Mantle => "mantle",
        }
    }
}

/// Explicit route prefixes, in litellm's `route_mappings` order.
const ROUTE_PREFIXES: [(&str, BedrockRoute); 9] = [
    ("invoke/", BedrockRoute::Invoke),
    ("claude_platform/", BedrockRoute::ClaudePlatform),
    ("converse_like/", BedrockRoute::ConverseLike),
    ("converse/", BedrockRoute::Converse),
    ("agent/", BedrockRoute::Agent),
    ("agentcore/", BedrockRoute::AgentCore),
    ("async_invoke/", BedrockRoute::AsyncInvoke),
    ("openai/", BedrockRoute::OpenAi),
    ("mantle/", BedrockRoute::Mantle),
];

/// The marker that identifies an application-inference-profile ARN.
const APPLICATION_INFERENCE_PROFILE: &str = ":application-inference-profile/";

/// litellm's `BEDROCK_CONVERSE_MODELS` (`litellm/constants.py`), verbatim.
///
/// Hand-maintained: these are **bare** ids, so anything with a cross-region
/// prefix, an ARN wrapper or a throughput/context suffix must be normalised by
/// [`super::model_id::base_model`] before it is looked up here.
pub const BEDROCK_CONVERSE_MODELS: &[&str] = &[
    "qwen.qwen3-coder-480b-a35b-v1:0",
    "qwen.qwen3-coder-next",
    "qwen.qwen3-235b-a22b-2507-v1:0",
    "qwen.qwen3-coder-30b-a3b-v1:0",
    "qwen.qwen3-32b-v1:0",
    "deepseek.v3-v1:0",
    "deepseek.v3.2",
    "openai.gpt-oss-20b-1:0",
    "openai.gpt-oss-120b-1:0",
    "anthropic.claude-haiku-4-5-20251001-v1:0",
    "anthropic.claude-sonnet-4-5-20250929-v1:0",
    "anthropic.claude-fable-5",
    "anthropic.claude-sonnet-5",
    "anthropic.claude-opus-5",
    "anthropic.claude-opus-4-8",
    "anthropic.claude-opus-4-7",
    "anthropic.claude-opus-4-6-v1:0",
    "anthropic.claude-opus-4-6-v1",
    "anthropic.claude-sonnet-4-6",
    "anthropic.claude-opus-4-1-20250805-v1:0",
    "anthropic.claude-opus-4-20250514-v1:0",
    "anthropic.claude-sonnet-4-20250514-v1:0",
    "anthropic.claude-3-7-sonnet-20250219-v1:0",
    "anthropic.claude-3-5-haiku-20241022-v1:0",
    "anthropic.claude-3-5-sonnet-20241022-v2:0",
    "anthropic.claude-3-5-sonnet-20240620-v1:0",
    "anthropic.claude-3-opus-20240229-v1:0",
    "anthropic.claude-3-sonnet-20240229-v1:0",
    "anthropic.claude-3-haiku-20240307-v1:0",
    "anthropic.claude-v2",
    "anthropic.claude-v2:1",
    "anthropic.claude-v1",
    "anthropic.claude-instant-v1",
    "ai21.jamba-instruct-v1:0",
    "ai21.jamba-1-5-mini-v1:0",
    "ai21.jamba-1-5-large-v1:0",
    "meta.llama3-70b-instruct-v1:0",
    "meta.llama3-8b-instruct-v1:0",
    "meta.llama3-1-8b-instruct-v1:0",
    "meta.llama3-1-70b-instruct-v1:0",
    "meta.llama3-1-405b-instruct-v1:0",
    "mistral.mistral-large-2407-v1:0",
    "mistral.mistral-large-2402-v1:0",
    "mistral.mistral-small-2402-v1:0",
    "meta.llama3-2-1b-instruct-v1:0",
    "meta.llama3-2-3b-instruct-v1:0",
    "meta.llama3-2-11b-instruct-v1:0",
    "meta.llama3-2-90b-instruct-v1:0",
    "amazon.nova-lite-v1:0",
    "amazon.nova-2-lite-v1:0",
    "amazon.nova-2-pro-preview-20251202-v1:0",
    "amazon.nova-pro-v1:0",
    "writer.palmyra-x4-v1:0",
    "writer.palmyra-x5-v1:0",
    "minimax.minimax-m2.1",
    "moonshotai.kimi-k2.5",
];

/// Whether a route token appears as a leading path segment of `model`.
///
/// Port of `_model_has_route_prefix`: a plain `contains` would match the
/// `bedrock_mantle/` *provider* prefix against the `mantle/` *route*, so the
/// token is anchored to the start or to a `/` boundary.
pub fn has_route_prefix(model: &str, prefix: &str) -> bool {
    model.starts_with(prefix) || model.contains(&format!("/{prefix}"))
}

/// Whether `model` is an application-inference-profile ARN.
pub fn is_application_inference_profile_arn(model: &str) -> bool {
    model.contains(APPLICATION_INFERENCE_PROFILE)
}

/// Whether the §1.4.1-normalised `model` is served by the Converse API.
pub fn is_converse_model(model: &str) -> bool {
    let base = super::model_id::base_model(model);
    let alt = super::model_id::strip_routing_prefix(model);
    BEDROCK_CONVERSE_MODELS.contains(&base.as_str()) || BEDROCK_CONVERSE_MODELS.contains(&alt)
}

/// Select the Bedrock route for `model` (§1.4.2).
pub fn select_route(model: &str) -> BedrockRoute {
    for (prefix, route) in ROUTE_PREFIXES {
        if has_route_prefix(model, prefix) {
            return route;
        }
    }

    // `nova/` and `nova-2/` name a custom-model spec, not a route token, so
    // they are checked after one leading `bedrock/` comes off.
    let after_bedrock = model.strip_prefix("bedrock/").unwrap_or(model);
    if after_bedrock.starts_with("nova-2/") || after_bedrock.starts_with("nova/") {
        return BedrockRoute::Converse;
    }

    if is_application_inference_profile_arn(model) {
        return BedrockRoute::Converse;
    }

    if is_converse_model(model) {
        BedrockRoute::Converse
    } else {
        BedrockRoute::Invoke
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

    #[test]
    fn route_prefix_is_anchored_to_a_path_segment() {
        assert!(has_route_prefix("mantle/x", "mantle/"));
        assert!(has_route_prefix("bedrock/mantle/x", "mantle/"));
        // The `bedrock_mantle/` provider prefix must not be read as the
        // `mantle/` route.
        assert!(!has_route_prefix(
            "bedrock_mantle/openai.gpt-oss-20b-1:0",
            "mantle/"
        ));
    }

    #[test]
    fn converse_table_lookup_uses_the_normalised_id() {
        assert!(is_converse_model(
            "eu.anthropic.claude-sonnet-4-5-20250929-v1:0"
        ));
        assert!(is_converse_model(
            "anthropic.claude-sonnet-4-5-20250929-v1:0"
        ));
        assert!(!is_converse_model("cohere.command-text-v14"));
    }
}
