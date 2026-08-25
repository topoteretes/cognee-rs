//! §1.4.1 model-id normalisation and §1.4.2 route selection.
//!
//! This suite exists for one regression above all: **every model cognee ships
//! by default is `eu.`-prefixed, while litellm's converse table stores only
//! bare ids.** A port that looks the raw id up in that table routes 3 of 3
//! shipped models to `invoke` — the adapter fails on its own defaults. Plan
//! §1.4.1 calls that the single highest-consequence correction in the plan, and
//! [`the_three_shipped_models_route_to_converse`] is the test that catches it.
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use cognee_llm::adapters::bedrock::caps::MODEL_CAPS;
use cognee_llm::adapters::bedrock::converse::converse_url;
use cognee_llm::adapters::bedrock::model_id::{
    CROSS_REGION_PREFIXES, NOVA_2_CUSTOM_BASE_MODEL, NOVA_CUSTOM_BASE_MODEL, base_model,
    extract_model_name_from_arn, strip_context_window_suffix, strip_cross_region_prefix,
    strip_routing_prefix, strip_throughput_suffix, wire_model_id,
};
use cognee_llm::adapters::bedrock::route::{BEDROCK_CONVERSE_MODELS, BedrockRoute, select_route};

/// The three models cognee's `/settings` endpoint offers today (plan §P2).
const SHIPPED_MODELS: [&str; 3] = [
    "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
    "eu.anthropic.claude-haiku-4-5-20251001-v1:0",
    "eu.amazon.nova-lite-v1:0",
];

#[test]
fn the_three_shipped_models_route_to_converse() {
    for model in SHIPPED_MODELS {
        assert_eq!(
            select_route(model),
            BedrockRoute::Converse,
            "{model} must route to converse — it only does so via §1.4.1 normalisation",
        );
    }
}

/// The mirror image of the test above: proves it can actually fail. If the raw
/// ids were in the table, normalisation could be deleted and the routing test
/// would still pass.
#[test]
fn the_raw_shipped_ids_are_absent_from_the_converse_table() {
    for model in SHIPPED_MODELS {
        assert!(
            !BEDROCK_CONVERSE_MODELS.contains(&model),
            "{model} is stored bare in BEDROCK_CONVERSE_MODELS — the normalisation \
             regression test above would no longer be able to fail",
        );
        // ...and the normalised form *is* in the table.
        assert!(
            BEDROCK_CONVERSE_MODELS.contains(&base_model(model).as_str()),
            "normalised {model} is missing from BEDROCK_CONVERSE_MODELS",
        );
    }
}

#[test]
fn foundation_model_and_inference_profile_arns_unwrap_to_the_bare_id() {
    assert_eq!(
        base_model(
            "arn:aws:bedrock:eu-west-1::foundation-model/anthropic.claude-3-haiku-20240307-v1:0"
        ),
        "anthropic.claude-3-haiku-20240307-v1:0"
    );
    assert_eq!(
        base_model(
            "arn:aws:bedrock:eu-central-1:123456789012:inference-profile/eu.anthropic.claude-haiku-4-5-20251001-v1:0"
        ),
        "anthropic.claude-haiku-4-5-20251001-v1:0"
    );
    // The helper is a "last path segment" grab, exactly as upstream.
    assert_eq!(
        extract_model_name_from_arn(
            "arn:aws:bedrock:us-east-1::foundation-model/meta.llama3-8b-instruct-v1:0"
        ),
        "meta.llama3-8b-instruct-v1:0"
    );
    // A non-ARN id is returned untouched.
    assert_eq!(
        extract_model_name_from_arn("amazon.nova-lite-v1:0"),
        "amazon.nova-lite-v1:0"
    );
}

#[test]
fn every_cross_region_inference_prefix_is_stripped() {
    for prefix in CROSS_REGION_PREFIXES {
        let prefixed = format!("{prefix}.anthropic.claude-sonnet-4-5-20250929-v1:0");
        assert_eq!(
            base_model(&prefixed),
            "anthropic.claude-sonnet-4-5-20250929-v1:0",
            "the {prefix}. cross-region prefix was not stripped",
        );
        assert_eq!(
            select_route(&prefixed),
            BedrockRoute::Converse,
            "{prefixed} must route to converse",
        );
    }
    // A provider segment that merely *looks* like a prefix is not stripped.
    assert_eq!(
        strip_cross_region_prefix("anthropic.claude-v2"),
        "anthropic.claude-v2"
    );
    assert_eq!(
        strip_cross_region_prefix("amazon.nova-lite-v1:0"),
        "amazon.nova-lite-v1:0"
    );
}

#[test]
fn provisioned_throughput_and_context_window_suffixes_are_stripped() {
    assert_eq!(
        base_model("anthropic.claude-3-5-sonnet-20241022-v2:0:51k"),
        "anthropic.claude-3-5-sonnet-20241022-v2:0"
    );
    assert_eq!(
        base_model("us.anthropic.claude-opus-4-6-v1[1m]"),
        "anthropic.claude-opus-4-6-v1"
    );
    assert_eq!(
        strip_throughput_suffix("anthropic.claude-3-5-sonnet-20241022-v2:0:18k"),
        "anthropic.claude-3-5-sonnet-20241022-v2:0"
    );
    assert_eq!(
        strip_context_window_suffix("anthropic.claude-opus-4-6-v1[200k]"),
        "anthropic.claude-opus-4-6-v1"
    );
    // A bare version suffix must survive — stripping it would break every id.
    assert_eq!(
        strip_throughput_suffix("amazon.nova-lite-v1:0"),
        "amazon.nova-lite-v1:0"
    );
    // Both suffixed forms still route to converse.
    assert_eq!(
        select_route("anthropic.claude-3-5-sonnet-20241022-v2:0:51k"),
        BedrockRoute::Converse
    );
    assert_eq!(
        select_route("us.anthropic.claude-opus-4-6-v1[1m]"),
        BedrockRoute::Converse
    );
}

#[test]
fn a_leading_bedrock_prefix_is_stripped() {
    assert_eq!(
        base_model("bedrock/eu.anthropic.claude-sonnet-4-5-20250929-v1:0"),
        "anthropic.claude-sonnet-4-5-20250929-v1:0"
    );
    assert_eq!(
        strip_routing_prefix("bedrock/converse/amazon.nova-lite-v1:0"),
        "amazon.nova-lite-v1:0"
    );
    assert_eq!(
        select_route("bedrock/eu.amazon.nova-lite-v1:0"),
        BedrockRoute::Converse
    );
}

#[test]
fn nova_spec_prefixes_normalise_to_the_synthetic_custom_ids() {
    assert_eq!(
        base_model("nova/arn:aws:bedrock:us-east-1:1:custom-model/abc"),
        NOVA_CUSTOM_BASE_MODEL
    );
    assert_eq!(
        base_model("bedrock/nova-2/arn:aws:bedrock:us-east-1:1:custom-model/abc"),
        NOVA_2_CUSTOM_BASE_MODEL
    );
}

#[test]
fn every_explicit_route_prefix_wins() {
    let cases: [(&str, BedrockRoute); 9] = [
        // `converse/` on a model the table does NOT contain — so the route can
        // only come from the explicit prefix.
        ("converse/cohere.command-text-v14", BedrockRoute::Converse),
        // `invoke/` on a model the table DOES contain — the prefix must beat the
        // table lookup, or the check is vacuous.
        (
            "invoke/anthropic.claude-sonnet-4-5-20250929-v1:0",
            BedrockRoute::Invoke,
        ),
        (
            "converse_like/cohere.command-text-v14",
            BedrockRoute::ConverseLike,
        ),
        ("agent/my-agent-id", BedrockRoute::Agent),
        ("agentcore/my-runtime", BedrockRoute::AgentCore),
        (
            "async_invoke/amazon.nova-lite-v1:0",
            BedrockRoute::AsyncInvoke,
        ),
        ("openai/openai.gpt-oss-20b-1:0", BedrockRoute::OpenAi),
        (
            "claude_platform/anthropic.claude-sonnet-4-5-20250929-v1:0",
            BedrockRoute::ClaudePlatform,
        ),
        ("mantle/anthropic.claude-opus-4-6-v1", BedrockRoute::Mantle),
    ];
    for (model, expected) in cases {
        assert_eq!(select_route(model), expected, "route for {model}");
    }
    // A prefix is honoured after a leading `bedrock/` too, because it is matched
    // as a path segment rather than only at position 0.
    assert_eq!(
        select_route("bedrock/invoke/anthropic.claude-sonnet-4-5-20250929-v1:0"),
        BedrockRoute::Invoke
    );
    // ...but the `bedrock_mantle/` *provider* prefix is not the `mantle/` route.
    assert_ne!(
        select_route("bedrock_mantle/openai.gpt-oss-20b-1:0"),
        BedrockRoute::Mantle
    );
}

#[test]
fn application_inference_profile_arns_route_to_converse() {
    // The ARN ends in an opaque id with no provider substring, so it is not in
    // the converse table and only the dedicated branch can route it.
    let arn =
        "arn:aws:bedrock:eu-central-1:123456789012:application-inference-profile/ab12cd34ef56";
    assert!(
        !BEDROCK_CONVERSE_MODELS.contains(&base_model(arn).as_str()),
        "the opaque profile id must not be table-resolvable, or this test is vacuous",
    );
    assert_eq!(select_route(arn), BedrockRoute::Converse);
}

#[test]
fn models_outside_the_converse_table_route_to_invoke() {
    assert_eq!(
        select_route("cohere.command-text-v14"),
        BedrockRoute::Invoke
    );
    assert_eq!(
        select_route("amazon.titan-text-express-v1"),
        BedrockRoute::Invoke
    );
}

/// Plan §1.4.2: "the URL carries the **original** id, prefix included —
/// normalisation feeds the routing decision only, never the request path."
#[test]
fn the_request_url_keeps_the_un_normalised_id() {
    let endpoint = "https://bedrock-runtime.eu-central-1.amazonaws.com";
    let url = converse_url(endpoint, "eu.anthropic.claude-sonnet-4-5-20250929-v1:0");

    assert_eq!(
        url,
        "https://bedrock-runtime.eu-central-1.amazonaws.com\
         /model/eu.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse"
    );
    assert!(
        url.contains("/model/eu.anthropic."),
        "the cross-region prefix must survive into the URL: {url}",
    );
    assert!(
        !url.contains("/model/anthropic."),
        "the URL must NOT carry the normalised id: {url}",
    );

    // An ARN survives whole, as a single percent-encoded path segment.
    let arn = "arn:aws:bedrock:eu-central-1:123456789012:application-inference-profile/ab12cd34";
    let arn_url = converse_url(endpoint, arn);
    assert!(
        arn_url.ends_with(
            "/model/arn%3Aaws%3Abedrock%3Aeu-central-1%3A123456789012\
             %3Aapplication-inference-profile%2Fab12cd34/converse"
        ),
        "{arn_url}",
    );
}

// ---------------------------------------------------------------------------
// Table drift and wire-id stripping — the two model-id defects found in review.
// ---------------------------------------------------------------------------

/// Every model with a capability row must be routable to Converse.
///
/// The defect this catches: `MODEL_CAPS` gained rows for models missing from
/// the converse tables, so `BedrockAdapter::new` rejected them at construction
/// with a message asserting they were invoke-only — while litellm routed them
/// to Converse. The caps rows were simultaneously dead code.
#[test]
fn every_capability_row_routes_to_converse() {
    for (model, _caps) in MODEL_CAPS {
        assert_eq!(
            select_route(model),
            BedrockRoute::Converse,
            "`{model}` has a MODEL_CAPS row but does not route to Converse — \
             either add it to a converse table or drop the caps row",
        );
    }
}

/// litellm extends its static constant with the pricing file at import time, so
/// the static table alone is only half the set. Both halves must be consulted.
#[test]
fn the_derived_converse_table_covers_the_pricing_file_models() {
    for model in [
        "amazon.nova-micro-v1:0",
        "anthropic.claude-opus-4-5-20251101-v1:0",
        "meta.llama3-3-70b-instruct-v1:0",
        "zai.glm-5",
    ] {
        assert!(
            !BEDROCK_CONVERSE_MODELS.contains(&model),
            "`{model}` is expected to come from the derived table, not the \
             verbatim constant — this test is asserting the wrong thing",
        );
        assert_eq!(
            select_route(model),
            BedrockRoute::Converse,
            "`{model}` is bedrock_converse in the pricing file and must route \
             to Converse",
        );
        // The cross-region form must work too — that is §1.4.1's whole point.
        assert_eq!(select_route(&format!("eu.{model}")), BedrockRoute::Converse);
    }
}

/// Routing tokens are configuration syntax and must not reach the wire.
///
/// The defect this catches: `bedrock/…` routed and constructed fine, then 400ed
/// on every request against `/model/bedrock%2F…/converse`.
#[test]
fn routing_tokens_are_stripped_from_the_url_but_real_id_parts_survive() {
    let endpoint = "https://bedrock-runtime.eu-central-1.amazonaws.com";

    for configured in [
        "bedrock/eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "converse/eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "bedrock/converse/eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
    ] {
        let url = converse_url(endpoint, configured);
        assert!(
            !url.contains("bedrock%2F") && !url.contains("converse%2F"),
            "routing token leaked into the URL for `{configured}`: {url}",
        );
        assert!(
            url.contains("/model/eu.anthropic.claude-sonnet-4-5-20250929-v1%3A0/converse"),
            "the real id must survive intact for `{configured}`: {url}",
        );
    }

    // `nova/` is a custom-model spec, not a routing token: it stays.
    assert!(
        converse_url(endpoint, "nova/my-custom-model").contains("/model/nova%2Fmy-custom-model/"),
        "the nova/ spec prefix must survive into the URL",
    );

    // A cross-region prefix and an ARN are part of the identifier: unchanged.
    assert_eq!(
        wire_model_id("eu.amazon.nova-lite-v1:0"),
        "eu.amazon.nova-lite-v1:0",
    );
}
