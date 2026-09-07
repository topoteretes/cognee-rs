//! `httpmock` integration test for how the Bedrock structured-output loop
//! classifies a **native-branch** truncation (no real API calls).
//!
//! On a model that advertises native structured output (`supports_native_structured_output`,
//! e.g. Claude Sonnet 4.5), `ConverseResponse::structured_payload` parses the
//! response *text* as JSON — and text cut off at `maxTokens` never parses. So
//! the truncated answer arrives with **no** payload at all, which is the same
//! shape as a model that simply refused to emit JSON. Testing truncation only on
//! a *present* payload therefore misfiled every native truncation as
//! "did not contain parseable JSON" and re-asked at the identical budget, which
//! truncates identically — burning a full generation plus a backoff per attempt
//! until `MaxRetriesExceeded`.
//!
//! These tests pin the classification: a truncated native response is reported
//! as a truncation, is never re-asked at the same budget, and is repaired by
//! raising the budget when the effective cap leaves headroom.
//!
//! The fallback (`json_tool_call`) branch, where the partial `toolUse.input`
//! survives parsing, is covered in `bedrock_integration.rs`.
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use cognee_llm::adapters::bedrock::BedrockAdapter;
use cognee_llm::adapters::bedrock::aws::env::AwsInputs;
use cognee_llm::adapters::bedrock::converse::encode_model_id;
use cognee_llm::error::LlmError;
use cognee_llm::llm_trait::Llm;
use cognee_llm::types::{GenerationOptions, Message};
use httpmock::prelude::*;
use serde_json::{Value, json};

/// A bearer key short-circuits the §1.2 credential ladder before any lookup, so
/// these tests never touch AWS, `~/.aws` or IMDS.
const BEARER_KEY: &str = "test-bedrock-key";

/// Native structured output (caps row `anthropic.claude-sonnet-4-5-20250929-v1:0`,
/// model cap 64_000) — the branch on which a truncation loses its payload.
const SONNET: &str = "eu.anthropic.claude-sonnet-4-5-20250929-v1:0";

/// Build an adapter whose endpoint is the mock server, with the region supplied
/// explicitly so the chain stays hermetic on a machine with a populated
/// `~/.aws/config`.
async fn adapter(server: &MockServer, model: &str) -> BedrockAdapter {
    let aws = AwsInputs {
        region: Some("eu-central-1".to_string()),
        bedrock_runtime_endpoint: Some(server.base_url()),
        ..AwsInputs::default()
    };
    BedrockAdapter::new(model, Some(BEARER_KEY), None, &aws)
        .await
        .expect("adapter builds offline under bearer auth")
        .with_network_retries(0)
}

fn converse_path(model: &str) -> String {
    format!("/model/{}/converse", encode_model_id(model))
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "name": { "type": "string" } },
        "required": ["name"]
    })
}

/// The regression: a native answer cut off mid-JSON is a truncation, not an
/// unparseable answer.
///
/// The default ceiling (16_384) is also the effective cap here — `min(model cap
/// 64_000, ceiling)` — so the first request already goes out at the cap and
/// there is no headroom to raise into: the call must fail terminally on the
/// first response. `structured_output_retries(3)` is deliberate, so a loop that
/// re-asked at the same budget would show up as three calls.
#[tokio::test]
async fn a_truncated_native_answer_is_reported_as_truncation_not_as_unparseable_json() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(SONNET))
                .body_includes(r#""maxTokens":16384"#);
            then.status(200)
                .header("content-type", "application/json")
                // Cut off mid-value: exactly what a real `maxTokens` stop emits,
                // and unparseable as JSON.
                .body(
                    r#"{"output":{"message":{"content":[{"text":"{\"name\": \"Alexander Gra"}]}},
                        "stopReason":"max_tokens"}"#,
                );
        })
        .await;

    let error = adapter(&server, SONNET)
        .await
        .with_structured_output_retries(3)
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect_err("a truncation at the effective cap cannot be repaired");

    // Exactly one exchange: the loop stopped instead of re-asking at a budget
    // that would truncate at the same point.
    mock.assert_calls_async(1).await;
    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
    let message = error.to_string();
    assert!(
        message.contains("truncated") && message.contains("output budget"),
        "the error must name the cause and the budget that was hit: {message}",
    );
    assert!(
        !message.contains("parseable JSON"),
        "a truncation must not be misfiled as an unparseable answer: {message}",
    );
}

/// The same, in the other disguise: the budget was spent before any text was
/// emitted, so the answer is blank rather than cut off mid-value. Neither shape
/// is self-describing, which is why `stopReason` — not the payload — decides.
#[tokio::test]
async fn a_blank_truncated_native_answer_is_reported_as_truncation() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(SONNET))
                .body_includes(r#""maxTokens":16384"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":""}]}},
                        "stopReason":"max_tokens"}"#,
                );
        })
        .await;

    let error = adapter(&server, SONNET)
        .await
        .with_structured_output_retries(3)
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect_err("a blank truncation at the effective cap cannot be repaired");

    mock.assert_calls_async(1).await;
    let message = error.to_string();
    assert!(
        message.contains("truncated") && message.contains("output budget"),
        "the error must name the cause and the budget that was hit: {message}",
    );
    assert!(
        !message.contains("parseable JSON"),
        "a truncation must not be misfiled as an unparseable answer: {message}",
    );
}

/// A caller who set `max_tokens` themselves gets a terminal error, not a
/// silently larger (and larger-billed) re-ask. This is the rule the merged
/// OpenAI fix states explicitly — "An explicit max_tokens is not raised
/// automatically" — and the reason is the same on Bedrock: the HTTP
/// custom-prompt route forwards client-supplied budgets straight into
/// structured output, so a client asking for 1_000 tokens must not be re-issued
/// at the 16_384 ceiling. The single mock proves it: a raise would have to send
/// a second request, and there is none to match it.
#[tokio::test]
async fn a_truncated_answer_at_a_caller_supplied_budget_is_terminal() {
    let server = MockServer::start_async().await;
    let truncated = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(SONNET))
                .body_includes(r#""maxTokens":1000"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":"{\"name\": \"Alexander Gra"}]}},
                        "stopReason":"max_tokens"}"#,
                );
        })
        .await;

    let error = adapter(&server, SONNET)
        .await
        .with_structured_output_retries(3)
        .create_structured_output_with_messages_raw(
            vec![Message::user("who?")],
            &schema(),
            Some(GenerationOptions {
                max_tokens: Some(1_000),
                ..Default::default()
            }),
        )
        .await
        .expect_err("a caller-supplied budget must not be raised");

    let message = error.to_string();
    assert!(
        message.contains("1000") && message.contains("caller-requested"),
        "the error must name the caller's own budget: {message}",
    );
    assert!(
        !message.contains("parseable JSON"),
        "a truncation must not be misfiled as an unparseable answer: {message}",
    );
    // Exactly one call: the refusal is immediate, with no re-ask at any budget.
    truncated.assert_calls_async(1).await;
}
