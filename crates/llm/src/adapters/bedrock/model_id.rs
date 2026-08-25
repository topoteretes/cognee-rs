//! §1.4.1 model-id normalisation — the step that happens **before** routing.
//!
//! Port of litellm's `get_bedrock_base_model` (`llms/bedrock/common_utils.py`)
//! and the four helpers it composes: `strip_bedrock_routing_prefix`,
//! `extract_model_name_from_bedrock_arn`, `strip_bedrock_throughput_suffix`
//! (which also strips the context-window suffix), and the cross-region
//! inference-prefix strip.
//!
//! # Why this module exists
//!
//! `BEDROCK_CONVERSE_MODELS` (see [`super::route`]) stores only **bare** ids —
//! `anthropic.claude-sonnet-4-5-20250929-v1:0`, never
//! `eu.anthropic.claude-sonnet-4-5-20250929-v1:0`. Every model cognee ships by
//! default is `eu.`-prefixed, so a port that looks the raw id up in that table
//! routes 3 of 3 shipped models to `invoke` — i.e. the adapter fails on its own
//! defaults. Plan §1.4.1 calls this "the single highest-consequence correction"
//! in the whole plan.
//!
//! # The one rule that must not be forgotten
//!
//! **The normalised id feeds routing and the capability lookup only. The
//! request URL keeps the ORIGINAL id, cross-region prefix included** — see
//! [`super::converse::converse_url`] and plan §1.4.2 ("the URL carries the
//! original id, prefix included — normalisation feeds the routing decision
//! only, never the request path").
//!
//! # Deliberate omission
//!
//! litellm has a fifth leg: an id whose first `/`-segment is a *full* AWS
//! region (`us-east-1/anthropic.claude-…`) has that segment stripped and reused
//! as the region. It needs litellm's complete Bedrock region list to be exact,
//! no model cognee ships uses that shape, and the failure mode of not porting
//! it is loud rather than silent (the id falls through to `invoke`, which
//! returns a clear `FeatureNotSupported`). Plan §4 R3 does not list it; it is
//! left out on purpose.

/// litellm routing prefixes stripped by `strip_bedrock_routing_prefix`,
/// **in litellm's order** — the loop is sequential, so `bedrock/converse/x`
/// loses both prefixes while `converse/bedrock/x` keeps the inner one.
pub const ROUTING_PREFIXES: [&str; 6] = [
    "bedrock/",
    "converse/",
    "invoke/",
    "openai/",
    "nova-2/",
    "nova/",
];

/// Cross-region inference prefixes
/// (`get_bedrock_cross_region_inference_regions`). These are the *abbreviated*
/// geography prefixes, not AWS region names.
pub const CROSS_REGION_PREFIXES: [&str; 7] = ["global", "us", "eu", "apac", "jp", "au", "us-gov"];

/// Base id litellm reports for the `nova/` custom-model spec prefix.
pub const NOVA_CUSTOM_BASE_MODEL: &str = "amazon.nova-custom";

/// Base id litellm reports for the `nova-2/` custom-model spec prefix.
pub const NOVA_2_CUSTOM_BASE_MODEL: &str = "amazon.nova-2-custom";

/// Strip litellm routing prefixes, sequentially, in [`ROUTING_PREFIXES`] order.
///
/// Faithful to litellm's loop: it re-tests every prefix against the partially
/// stripped value, so more than one can come off in a single call.
pub fn strip_routing_prefix(model: &str) -> &str {
    let mut model = model;
    for prefix in ROUTING_PREFIXES {
        if let Some(rest) = model.strip_prefix(prefix) {
            model = rest;
        }
    }
    model
}

/// Routing tokens that must never reach the wire.
///
/// A strict subset of [`ROUTING_PREFIXES`]. `openai/` is excluded because it
/// selects a different handler entirely (those ids are rejected in
/// `BedrockAdapter::new`, so they never reach a URL).
///
/// `nova/` and `nova-2/` **are** stripped: they name a custom-model spec, and
/// `converse_handler.py:293-296` removes them from the id it encodes into the
/// path, exactly as it removes the route tokens.
const WIRE_STRIPPED_PREFIXES: [&str; 5] = ["bedrock/", "converse/", "invoke/", "nova-2/", "nova/"];

/// The model id as it must appear in a Bedrock request path.
///
/// litellm strips its own routing tokens and the nova custom-model spec prefix
/// before building the URL (`converse_handler.py:278-296`), and only those: a
/// cross-region prefix (`eu.`, `us.`, …) and an ARN are part of the real Bedrock
/// identifier and **must** survive. This is the §1.4.1 counterpart to
/// [`base_model`] — that one normalises for the routing *decision*, this one for
/// the request *path*.
///
/// litellm applies the route tokens and the nova prefix in two separate passes
/// over two different variables, which leaves `bedrock/` on the wire for the
/// combined `bedrock/nova/x` spelling. That quirk is not reproduced: litellm's
/// own router strips the provider prefix upstream, so the case never arises
/// there in practice, and reproducing it here would put a token on the wire that
/// Bedrock rejects.
///
/// Sequential like [`strip_routing_prefix`], so `bedrock/converse/x` loses both.
pub fn wire_model_id(model: &str) -> &str {
    let mut model = model;
    for prefix in WIRE_STRIPPED_PREFIXES {
        if let Some(rest) = model.strip_prefix(prefix) {
            model = rest;
        }
    }
    model
}

/// Unwrap a Bedrock ARN to the bare model id: everything after the last `/`.
///
/// Faithful to `extract_model_name_from_bedrock_arn`, which triggers on the
/// substring `arn` appearing **anywhere** in the (lowercased) id rather than on
/// a real ARN parse. That looseness is upstream's, and it is kept so a shared
/// id normalises identically on both SDKs.
pub fn extract_model_name_from_arn(model: &str) -> &str {
    if !model.to_ascii_lowercase().contains("arn") {
        return model;
    }
    // `split('/').last()` on a value with no `/` is the value itself, matching
    // Python's `model.split("/")[-1]`.
    model.rsplit('/').next().unwrap_or(model)
}

/// Strip a provisioned-throughput suffix: `…:<digits>:<digits>k` → `…:<digits>`.
///
/// Hand-rolled port of litellm's `re.sub(r"(:\d+):\d+k$", r"\1", model)` —
/// `cognee-llm` carries no `regex` dependency.
pub fn strip_throughput_suffix(model: &str) -> &str {
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());

    let Some(last_colon) = model.rfind(':') else {
        return model;
    };
    let Some(throughput) = model[last_colon + 1..].strip_suffix('k') else {
        return model;
    };
    if !all_digits(throughput) {
        return model;
    }
    let head = &model[..last_colon];
    let Some(prev_colon) = head.rfind(':') else {
        return model;
    };
    if !all_digits(&head[prev_colon + 1..]) {
        return model;
    }
    head
}

/// Strip a context-window suffix: `…[1m]`, `…[200k]` → `…`.
///
/// Port of litellm's `re.sub(r"\[\w+\]$", "", model)`; `\w` is
/// `[A-Za-z0-9_]`.
pub fn strip_context_window_suffix(model: &str) -> &str {
    let Some(without_bracket) = model.strip_suffix(']') else {
        return model;
    };
    let Some(open) = without_bracket.rfind('[') else {
        return model;
    };
    let inner = &without_bracket[open + 1..];
    if inner.is_empty()
        || !inner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return model;
    }
    &model[..open]
}

/// Strip a cross-region inference prefix (`eu.`, `us.`, `apac.`, …).
///
/// Only the [`CROSS_REGION_PREFIXES`] set counts: `anthropic.claude-…` keeps
/// its `anthropic.` provider segment because `anthropic` is not one of them.
pub fn strip_cross_region_prefix(model: &str) -> &str {
    match model.split_once('.') {
        Some((prefix, rest)) if CROSS_REGION_PREFIXES.contains(&prefix) => rest,
        _ => model,
    }
}

/// The full §1.4.1 chain: routing prefixes → ARN unwrap → throughput suffix →
/// context-window suffix → cross-region prefix.
///
/// The `nova/` and `nova-2/` custom-model spec prefixes short-circuit to their
/// synthetic base ids first, exactly as `get_bedrock_base_model` does.
pub fn base_model(model: &str) -> String {
    // Nova spec prefixes are detected *before* the generic strip, because
    // `strip_routing_prefix` would otherwise eat `nova/` and leave the opaque
    // ARN behind.
    let mut spec = model;
    for prefix in ["bedrock/converse/", "bedrock/", "converse/"] {
        if let Some(rest) = spec.strip_prefix(prefix) {
            spec = rest;
            break;
        }
    }
    if spec.starts_with("nova-2/") {
        return NOVA_2_CUSTOM_BASE_MODEL.to_string();
    }
    if spec.starts_with("nova/") {
        return NOVA_CUSTOM_BASE_MODEL.to_string();
    }

    let stripped = strip_routing_prefix(model);
    let stripped = extract_model_name_from_arn(stripped);
    let stripped = strip_throughput_suffix(stripped);
    let stripped = strip_context_window_suffix(stripped);
    strip_cross_region_prefix(stripped).to_string()
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
    fn routing_prefixes_come_off_sequentially() {
        assert_eq!(strip_routing_prefix("bedrock/converse/x"), "x");
        assert_eq!(strip_routing_prefix("bedrock/x"), "x");
        assert_eq!(strip_routing_prefix("invoke/x"), "x");
        // The loop is ordered, so an inner `bedrock/` is not reached.
        assert_eq!(strip_routing_prefix("converse/bedrock/x"), "bedrock/x");
        assert_eq!(strip_routing_prefix("x"), "x");
    }

    #[test]
    fn throughput_suffix_needs_both_numeric_groups() {
        assert_eq!(
            strip_throughput_suffix("anthropic.claude-3-5-sonnet-20241022-v2:0:51k"),
            "anthropic.claude-3-5-sonnet-20241022-v2:0"
        );
        // Not a throughput suffix: no `:<digits>` before it.
        assert_eq!(strip_throughput_suffix("model:51k"), "model:51k");
        // Not a throughput suffix: the tail is not `<digits>k`.
        assert_eq!(strip_throughput_suffix("model:0:v1k"), "model:0:v1k");
        // A plain version suffix survives untouched.
        assert_eq!(
            strip_throughput_suffix("amazon.nova-lite-v1:0"),
            "amazon.nova-lite-v1:0"
        );
    }

    #[test]
    fn context_window_suffix_only_matches_word_characters() {
        assert_eq!(
            strip_context_window_suffix("us.anthropic.claude-opus-4-6-v1[1m]"),
            "us.anthropic.claude-opus-4-6-v1"
        );
        assert_eq!(strip_context_window_suffix("model[200k]"), "model");
        assert_eq!(strip_context_window_suffix("model[]"), "model[]");
        assert_eq!(strip_context_window_suffix("model[a-b]"), "model[a-b]");
        assert_eq!(strip_context_window_suffix("model"), "model");
    }

    #[test]
    fn nova_spec_prefixes_short_circuit_to_synthetic_base_ids() {
        assert_eq!(
            base_model("bedrock/nova-2/arn:aws:bedrock:us-east-1:1:custom-model/abc"),
            NOVA_2_CUSTOM_BASE_MODEL
        );
        assert_eq!(
            base_model("nova/arn:aws:bedrock:us-east-1:1:custom-model/abc"),
            NOVA_CUSTOM_BASE_MODEL
        );
    }
}
