//! §1.4.3 structured output: the branch is chosen from the capability table,
//! never hard-coded.
//!
//! The plan is explicit that the forced `json_tool_call` tool is the
//! **fallback**, not the primary: "an R3 that implements only the forced tool
//! diverges from Python on all three defaults — and, since Nova is documented
//! to reject a specific `toolChoice`, may 400 outright on nova-lite."
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use cognee_llm::adapters::bedrock::caps::{UNKNOWN_MODEL_CAPS, caps_for};
use cognee_llm::adapters::bedrock::converse::{
    ConverseResponse, RESPONSE_FORMAT_TOOL_NAME, apply_structured_output, corrective_instruction,
    force_additional_properties_false,
};
use serde_json::{Value, json};

const SONNET: &str = "eu.anthropic.claude-sonnet-4-5-20250929-v1:0";
const HAIKU: &str = "eu.anthropic.claude-haiku-4-5-20251001-v1:0";
const NOVA_LITE: &str = "eu.amazon.nova-lite-v1:0";
/// Advertises `supports_tool_choice` but **not** native structured output — the
/// only combination that produces a forced `toolChoice`.
const TOOL_CHOICE_ONLY: &str = "eu.anthropic.claude-3-5-sonnet-20241022-v2:0";

fn schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "friends": {
                "type": "array",
                "items": { "type": "object", "properties": { "name": { "type": "string" } } }
            }
        },
        "required": ["name"]
    })
}

fn structured_body(model: &str) -> Value {
    let mut body = json!({ "messages": [], "inferenceConfig": { "maxTokens": 1024 } });
    apply_structured_output(&mut body, &schema(), &caps_for(model));
    body
}

#[test]
fn both_shipped_anthropic_models_take_the_native_output_config_branch() {
    for model in [SONNET, HAIKU] {
        let caps = caps_for(model);
        assert!(
            caps.supports_native_structured_output,
            "{model} must advertise native structured output",
        );

        let body = structured_body(model);
        assert_eq!(
            body["outputConfig"]["textFormat"]["type"], "json_schema",
            "{model} must use outputConfig.textFormat",
        );
        assert!(
            body.get("toolConfig").is_none(),
            "{model} must NOT fall back to the synthetic tool",
        );

        // The schema travels as a JSON string inside the text format.
        let embedded = body["outputConfig"]["textFormat"]["structure"]["jsonSchema"]["schema"]
            .as_str()
            .expect("the schema is embedded as a JSON string");
        let parsed: Value = serde_json::from_str(embedded).unwrap();
        assert_eq!(parsed["properties"]["name"]["type"], "string");
        assert!(
            parsed.get("$schema").is_none(),
            "the `$schema` meta key is not expected by Bedrock",
        );
    }
}

/// The case plan §1.4.3 says a naive port breaks.
#[test]
fn nova_lite_takes_the_json_tool_call_branch_without_a_forced_tool_choice() {
    let caps = caps_for(NOVA_LITE);
    assert!(!caps.supports_native_structured_output);
    assert!(
        !caps.supports_tool_choice,
        "nova-lite does not advertise supports_tool_choice and is documented to \
         400 on a specific toolChoice",
    );

    let body = structured_body(NOVA_LITE);
    assert!(
        body.get("outputConfig").is_none(),
        "nova-lite has no native structured output",
    );
    assert_eq!(
        body["toolConfig"]["tools"][0]["toolSpec"]["name"],
        RESPONSE_FORMAT_TOOL_NAME
    );
    // The tool's input schema IS the response schema.
    assert_eq!(
        body["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["properties"]["name"]["type"],
        "string"
    );
    assert!(
        body["toolConfig"].get("toolChoice").is_none(),
        "toolChoice must NOT be forced for nova-lite: {}",
        body["toolConfig"],
    );
}

#[test]
fn a_model_advertising_tool_choice_gets_the_forced_tool_choice() {
    let caps = caps_for(TOOL_CHOICE_ONLY);
    assert!(!caps.supports_native_structured_output);
    assert!(caps.supports_tool_choice);

    let body = structured_body(TOOL_CHOICE_ONLY);
    assert_eq!(
        body["toolConfig"]["toolChoice"]["tool"]["name"],
        RESPONSE_FORMAT_TOOL_NAME
    );
}

#[test]
fn an_unknown_model_takes_the_conservative_default_branch() {
    let caps = caps_for("eu.acme.some-future-model-v9:0");
    assert_eq!(caps, UNKNOWN_MODEL_CAPS);

    let body = structured_body("eu.acme.some-future-model-v9:0");
    assert!(
        body.get("outputConfig").is_none(),
        "an unlisted model must not be assumed to support native structured output",
    );
    assert_eq!(
        body["toolConfig"]["tools"][0]["toolSpec"]["name"],
        RESPONSE_FORMAT_TOOL_NAME
    );
    assert!(
        body["toolConfig"].get("toolChoice").is_none(),
        "an unlisted model must not be assumed to accept a forced toolChoice",
    );
}

#[test]
fn the_native_branch_forces_additional_properties_false_on_every_object_node() {
    let forced = force_additional_properties_false(&json!({
        "type": "object",
        "properties": {
            "nested": { "type": "object", "properties": {} },
            "list": { "type": "array", "items": { "type": "object", "properties": {} } }
        },
        "$defs": { "Extra": { "type": "object", "properties": {} } },
        "anyOf": [{ "type": "object", "properties": {} }]
    }));

    assert_eq!(forced["additionalProperties"], json!(false));
    assert_eq!(
        forced["properties"]["nested"]["additionalProperties"],
        json!(false)
    );
    assert_eq!(
        forced["properties"]["list"]["items"]["additionalProperties"],
        json!(false)
    );
    assert_eq!(
        forced["$defs"]["Extra"]["additionalProperties"],
        json!(false)
    );
    assert_eq!(forced["anyOf"][0]["additionalProperties"], json!(false));
    // A non-object node is untouched.
    assert!(
        forced["properties"]["list"]
            .get("additionalProperties")
            .is_none(),
        "an array node must not get additionalProperties",
    );

    // ...and an explicit `true` set by the caller is respected, not overwritten.
    let explicit = force_additional_properties_false(
        &json!({ "type": "object", "additionalProperties": true }),
    );
    assert_eq!(explicit["additionalProperties"], json!(true));
}

#[test]
fn the_payload_is_unwrapped_from_the_branch_that_produced_it() {
    // Native branch: the object comes back as text and is parsed as JSON.
    let native: ConverseResponse = serde_json::from_value(json!({
        "output": { "message": { "content": [{ "text": "{\"name\":\"Ada\"}" }] } },
        "stopReason": "end_turn"
    }))
    .unwrap();
    let payload = native
        .structured_payload(&caps_for(SONNET))
        .expect("native text output parses as JSON");
    assert_eq!(payload["name"], "Ada");

    // Fallback branch: the object is the tool input.
    let tool_use: ConverseResponse = serde_json::from_value(json!({
        "output": { "message": { "content": [
            { "text": "ignored preamble" },
            { "toolUse": {
                "toolUseId": "tu_1",
                "name": RESPONSE_FORMAT_TOOL_NAME,
                "input": { "name": "Grace" }
            }}
        ]}},
        "stopReason": "tool_use"
    }))
    .unwrap();
    let payload = tool_use
        .structured_payload(&caps_for(NOVA_LITE))
        .expect("the toolUse input is the payload");
    assert_eq!(payload["name"], "Grace");

    // Each branch only reads its own shape: a tool-only response yields nothing
    // for a native model, and vice versa. That mismatch is what drives the
    // repair loop's corrective re-ask.
    assert!(tool_use.structured_payload(&caps_for(SONNET)).is_none());
    assert!(native.structured_payload(&caps_for(NOVA_LITE)).is_none());
}

#[test]
fn truncation_is_reported_from_the_converse_stop_reason() {
    let truncated: ConverseResponse = serde_json::from_value(json!({
        "output": { "message": { "content": [{ "text": "{\"name\":\"Ad" }] } },
        "stopReason": "max_tokens"
    }))
    .unwrap();
    assert!(truncated.is_truncated());

    let complete: ConverseResponse = serde_json::from_value(json!({
        "output": { "message": { "content": [{ "text": "{}" }] } },
        "stopReason": "end_turn"
    }))
    .unwrap();
    assert!(!complete.is_truncated());
}

/// The corrective re-ask must address the branch that actually failed: on the
/// native branch there is no tool to call, so telling the model to call one
/// would be actively misleading — and that is the branch both shipped Anthropic
/// models take.
#[test]
fn the_corrective_re_ask_matches_the_branch_that_failed() {
    let reason = Some("missing required field `name`");

    let native = corrective_instruction(reason, true);
    assert!(
        native.contains("missing required field `name`"),
        "the failure reason is surfaced verbatim: {native}",
    );
    assert!(
        !native.contains(RESPONSE_FORMAT_TOOL_NAME),
        "the native branch has no tool to call: {native}",
    );

    let fallback = corrective_instruction(reason, false);
    assert!(
        fallback.contains(RESPONSE_FORMAT_TOOL_NAME),
        "the fallback branch re-asks for the synthetic tool: {fallback}",
    );
    assert!(fallback.contains("missing required field `name`"));
}
