//! End-to-end `BedrockAdapter` behaviour against an `httpmock` server, plus
//! `#[ignore]`d live tests (one per auth mode).
//!
//! The transport seam (`BedrockTransport`) is deliberately `pub(crate)`, so an
//! integration test cannot inject a fake transport — and it must not be widened
//! to `pub` just to be tested. Instead the adapter's **resolved endpoint** is
//! pointed at the mock server through `AwsInputs::bedrock_runtime_endpoint`
//! (rung 2 of the §1.3 endpoint chain), which exercises the real chain rather
//! than bypassing it.
//!
//! Every mock declares its matchers on `when`, so a wrong URL, header or body
//! shape shows up as a `404` from the mock server (and a failing
//! `assert_calls_async(1)`) rather than as a silently passing assertion.
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

const SONNET: &str = "eu.anthropic.claude-sonnet-4-5-20250929-v1:0";
const NOVA_LITE: &str = "eu.amazon.nova-lite-v1:0";

/// Build an adapter whose endpoint is the mock server.
///
/// `region` is supplied explicitly (rung 1 of the §1.3 region chain) so the
/// chain never falls through to `aws-config`'s default region provider — this
/// stays hermetic on a developer machine with a populated `~/.aws/config`.
async fn adapter(server: &MockServer, model: &str) -> BedrockAdapter {
    let aws = AwsInputs {
        region: Some("eu-central-1".to_string()),
        bedrock_runtime_endpoint: Some(server.base_url()),
        ..AwsInputs::default()
    };
    BedrockAdapter::new(model, Some(BEARER_KEY), None, &aws)
        .await
        .expect("adapter builds offline under bearer auth")
        // One mocked exchange per test.
        .with_network_retries(0)
        .with_structured_output_retries(1)
}

/// The Converse path for `model`, with the id percent-encoded as one segment.
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

#[tokio::test]
async fn generate_posts_converse_with_the_un_normalised_id_and_a_clamped_budget() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                // The exact, *un-normalised* path: `eu.` survives and `:` is
                // percent-encoded. A normalised id here would 404.
                .path(converse_path(NOVA_LITE))
                // Bearer auth: a plain header, no SigV4 (plan §1.2).
                .header("authorization", format!("Bearer {BEARER_KEY}"))
                .header_excludes("authorization", "AWS4-HMAC-SHA256")
                // nova-lite caps output at 10_000, below the 16_384 default
                // ceiling — so the budget is clamped, not forwarded.
                .body_includes(r#""maxTokens":10000"#)
                // System messages are hoisted out of `messages` into `system`.
                .body_includes(r#""system":[{"text":"be terse"}]"#)
                .body_includes(r#""content":[{"text":"hello"}]"#)
                .body_excludes(r#""role":"system""#);
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "output": { "message": { "role": "assistant", "content": [
                            { "text": "Hello " }, { "text": "world" }
                        ]}},
                        "stopReason": "end_turn",
                        "usage": { "inputTokens": 11, "outputTokens": 3, "totalTokens": 14 }
                    }"#,
                );
        })
        .await;

    let response = adapter(&server, NOVA_LITE)
        .await
        .generate(
            vec![Message::system("be terse"), Message::user("hello")],
            None,
        )
        .await
        .expect("generate should succeed");

    assert_eq!(response.content, "Hello world");
    assert_eq!(response.model, NOVA_LITE);
    assert_eq!(response.finish_reason.as_deref(), Some("end_turn"));
    let usage = response.usage.expect("usage is reported");
    assert_eq!(usage.prompt_tokens, 11);
    assert_eq!(usage.completion_tokens, 3);
    assert_eq!(usage.total_tokens, 14);
    mock.assert_calls_async(1).await;
}

#[tokio::test]
async fn structured_output_uses_the_native_output_config_for_the_shipped_anthropic_model() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(SONNET))
                .body_includes(r#""outputConfig""#)
                .body_includes(r#""type":"json_schema""#)
                // The native branch injects no synthetic tool.
                .body_excludes("toolConfig")
                .body_excludes("json_tool_call");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "output": { "message": { "role": "assistant", "content": [
                            { "text": "{\"name\":\"Ada\"}" }
                        ]}},
                        "stopReason": "end_turn"
                    }"#,
                );
        })
        .await;

    let value = adapter(&server, SONNET)
        .await
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect("structured output should succeed");

    assert_eq!(value["name"], "Ada");
    mock.assert_calls_async(1).await;
}

#[tokio::test]
async fn structured_output_falls_back_to_json_tool_call_without_forcing_tool_choice_on_nova() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(NOVA_LITE))
                .body_includes(r#""json_tool_call""#)
                .body_includes(r#""toolConfig""#)
                // nova-lite does not advertise `supports_tool_choice` and is
                // documented to 400 on a specific `toolChoice`.
                .body_excludes("toolChoice")
                .body_excludes("outputConfig");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "output": { "message": { "role": "assistant", "content": [
                            { "toolUse": {
                                "toolUseId": "tu_1",
                                "name": "json_tool_call",
                                "input": { "name": "Grace" }
                            }}
                        ]}},
                        "stopReason": "tool_use"
                    }"#,
                );
        })
        .await;

    let value = adapter(&server, NOVA_LITE)
        .await
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect("structured output should succeed");

    assert_eq!(value["name"], "Grace");
    mock.assert_calls_async(1).await;
}

#[tokio::test]
async fn llm_args_reach_additional_model_request_fields_on_the_wire() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(NOVA_LITE))
                .body_includes(r#""additionalModelRequestFields""#)
                .body_includes(r#""top_k":40"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":"ok"}]}},"stopReason":"end_turn"}"#,
                );
        })
        .await;

    let extra = json!({ "top_k": 40 }).as_object().unwrap().clone();
    let response = adapter(&server, NOVA_LITE)
        .await
        .with_extra_args(extra)
        .generate(vec![Message::user("hi")], None)
        .await
        .expect("generate should succeed");

    assert_eq!(response.content, "ok");
    mock.assert_calls_async(1).await;
}

/// A 400 `ThrottlingException` must engage the retry layer, not be reported as
/// a terminal bad request (plan §4 R3 step 7).
#[tokio::test]
async fn a_400_throttling_exception_is_retried_and_surfaces_as_a_rate_limit() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path(converse_path(NOVA_LITE));
            then.status(400)
                .header("content-type", "application/json")
                .body(r#"{"__type":"ThrottlingException","message":"Too many requests"}"#);
        })
        .await;

    // Two attempts: the initial one plus one retry, which only happens because
    // the throttle was classified as retryable.
    let error = adapter(&server, NOVA_LITE)
        .await
        .with_network_retries(1)
        .generate(vec![Message::user("hi")], None)
        .await
        .expect_err("a throttled request fails");

    assert!(
        matches!(error, LlmError::MaxRetriesExceeded(_)),
        "the transport ladder must exhaust rather than return terminally: {error:?}",
    );
    assert!(
        error.to_string().contains("Rate limit exceeded"),
        "the underlying error must be a rate limit, not a generic bad request: {error}",
    );
    mock.assert_calls_async(2).await;
}

/// The counterpart: a 400 `ValidationException` is terminal, so it must *not*
/// burn the retry budget.
#[tokio::test]
async fn a_400_validation_exception_is_terminal() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path(converse_path(NOVA_LITE));
            then.status(400)
                .header("content-type", "application/json")
                .body(r#"{"__type":"ValidationException","message":"bad maxTokens"}"#);
        })
        .await;

    let error = adapter(&server, NOVA_LITE)
        .await
        .with_network_retries(3)
        .generate(vec![Message::user("hi")], None)
        .await
        .expect_err("a validation failure fails");

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
    mock.assert_calls_async(1).await;
}

#[tokio::test]
async fn transcribe_image_sends_a_converse_image_block() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(NOVA_LITE))
                .body_includes(r#""image":{"format":"png""#)
                // 1x1-ish payload, base64 of "PNGDATA".
                .body_includes(r#""bytes":"UE5HREFUQQ==""#)
                .body_includes(r#""text":"What's in this image?""#)
                .body_includes(r#""maxTokens":300"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":"a cat"}]}},"stopReason":"end_turn"}"#,
                );
        })
        .await;

    let description = adapter(&server, NOVA_LITE)
        .await
        .transcribe_image(b"PNGDATA", "image/png", None)
        .await
        .expect("vision should succeed");

    assert_eq!(description, "a cat");
    mock.assert_calls_async(1).await;
}

/// The vision path is bounded by the same effective budget as the chat path:
/// the lesser of the model cap and the configured `llm_max_completion_tokens`
/// ceiling. A caller-supplied `max_tokens` must not slip past that ceiling.
#[tokio::test]
async fn transcribe_image_clamps_to_the_configured_ceiling() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(NOVA_LITE))
                .body_includes(r#""maxTokens":64"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":"a cat"}]}},"stopReason":"end_turn"}"#,
                );
        })
        .await;

    let description = adapter(&server, NOVA_LITE)
        .await
        .with_max_completion_tokens(64)
        .transcribe_image(
            b"PNGDATA",
            "image/png",
            Some(GenerationOptions {
                // Well above both the ceiling and nova-lite's 10_000 cap.
                max_tokens: Some(16_384),
                ..Default::default()
            }),
        )
        .await
        .expect("vision should succeed");

    assert_eq!(description, "a cat");
    mock.assert_calls_async(1).await;
}

// ---------------------------------------------------------------------------
// Live tests — one per auth mode. `#[ignore]`d; run by hand against a real
// account with `cargo test -p cognee-llm --features bedrock --test
// bedrock_integration -- --ignored --nocapture`.
// ---------------------------------------------------------------------------

/// Model id for the live tests, e.g. `eu.amazon.nova-lite-v1:0`.
const LIVE_MODEL_ENV: &str = "COGNEE_BEDROCK_LIVE_MODEL";

fn live_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Resolve the live model id, or explain the skip.
fn live_model(auth_mode: &str, required: &[&str]) -> Option<String> {
    let Some(model) = live_env(LIVE_MODEL_ENV) else {
        eprintln!("skipping live {auth_mode} test: {LIVE_MODEL_ENV} is unset");
        return None;
    };
    for name in required {
        if live_env(name).is_none() {
            eprintln!("skipping live {auth_mode} test: {name} is unset");
            return None;
        }
    }
    Some(model)
}

async fn live_round_trip(adapter: BedrockAdapter, auth_mode: &str) {
    let response = adapter
        .generate(
            vec![Message::user("Reply with the single word: pong")],
            Some(GenerationOptions {
                max_tokens: Some(16),
                ..Default::default()
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("live {auth_mode} request failed: {error}"));
    assert!(
        !response.content.trim().is_empty(),
        "live {auth_mode} request returned empty content",
    );
}

#[tokio::test]
#[ignore = "live AWS Bedrock: needs a real account and AWS_BEARER_TOKEN_BEDROCK"]
async fn live_bearer_token_auth() {
    let Some(model) = live_model("bearer", &["AWS_BEARER_TOKEN_BEDROCK"]) else {
        return;
    };
    let token = live_env("AWS_BEARER_TOKEN_BEDROCK").expect("checked above");
    let adapter = BedrockAdapter::new(model, Some(&token), None, &AwsInputs::default())
        .await
        .expect("adapter should build");
    live_round_trip(adapter, "bearer").await;
}

#[tokio::test]
#[ignore = "live AWS Bedrock: needs a real account and AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY"]
async fn live_static_key_auth() {
    let Some(model) = live_model(
        "static keys",
        &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"],
    ) else {
        return;
    };
    // Left empty on purpose: `AwsInputs::resolve` backfills the static keys (and
    // the region) from the uppercase environment, which is the §1.2 ladder this
    // test is here to exercise. `api_key: None` keeps the bearer rungs out of it
    // so SigV4 actually runs.
    let adapter = BedrockAdapter::new(model, None, None, &AwsInputs::default())
        .await
        .expect("adapter should build");
    live_round_trip(adapter, "static keys").await;
}

#[tokio::test]
#[ignore = "live AWS Bedrock: needs a real account and AWS_PROFILE_NAME"]
async fn live_profile_auth() {
    let Some(model) = live_model("profile", &["AWS_PROFILE_NAME"]) else {
        return;
    };
    // Note the spelling: litellm reads `AWS_PROFILE_NAME`, not `AWS_PROFILE`.
    let adapter = BedrockAdapter::new(model, None, None, &AwsInputs::default())
        .await
        .expect("adapter should build");
    live_round_trip(adapter, "profile").await;
}

/// The repair loop: an unusable first answer drives a corrective re-ask inside
/// the same retry budget, and the correction is actually on the second request.
///
/// The two mocks are disambiguated by the correction itself — the first only
/// matches a body *without* it, the second only a body *with* it — so a loop
/// that re-POSTed an identical body would never reach the second mock.
#[tokio::test]
async fn an_unparseable_answer_drives_a_corrective_re_ask_that_then_succeeds() {
    let server = MockServer::start_async().await;
    let first = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(SONNET))
                .body_excludes("could not be parsed");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":"sorry, no JSON here"}]}},
                        "stopReason":"end_turn"}"#,
                );
        })
        .await;
    let repaired = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(SONNET))
                .body_includes("could not be parsed")
                // The native branch must not be told to call a tool.
                .body_excludes("json_tool_call");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":"{\"name\":\"Ada\"}"}]}},
                        "stopReason":"end_turn"}"#,
                );
        })
        .await;

    let value = adapter(&server, SONNET)
        .await
        .with_structured_output_retries(3)
        .create_structured_output_with_messages_raw(vec![Message::user("who?")], &schema(), None)
        .await
        .expect("the repair loop should recover");

    assert_eq!(value["name"], "Ada");
    first.assert_calls_async(1).await;
    repaired.assert_calls_async(1).await;
}

/// The `_validated` override is a **defaulted** trait method that ignores its
/// validator unless an adapter overrides it — so this proves the override is
/// real: a well-formed JSON object that the caller's validator rejects must
/// drive a corrective re-ask carrying the validator's own reason, not be
/// returned as `Ok` (which would abort the caller at deserialization).
#[tokio::test]
async fn a_validator_rejection_drives_a_corrective_re_ask_carrying_its_reason() {
    let server = MockServer::start_async().await;
    let rejected = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(SONNET))
                .body_excludes("name must not be empty");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":"{\"name\":\"\"}"}]}},
                        "stopReason":"end_turn"}"#,
                );
        })
        .await;
    let accepted = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(SONNET))
                // The validator's reason is quoted back to the model verbatim.
                .body_includes("name must not be empty");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"text":"{\"name\":\"Ada\"}"}]}},
                        "stopReason":"end_turn"}"#,
                );
        })
        .await;

    // Passes `schema_required_validator`, so only this closure can reject an
    // object that already carries every required field.
    let validate = |value: &Value| -> Result<(), String> {
        if value["name"].as_str().is_some_and(|name| name.is_empty()) {
            return Err("name must not be empty".to_string());
        }
        Ok(())
    };

    let value = adapter(&server, SONNET)
        .await
        .with_structured_output_retries(3)
        .create_structured_output_with_messages_raw_validated(
            vec![Message::user("who?")],
            &schema(),
            None,
            &validate,
        )
        .await
        .expect("the validator-driven repair loop should recover");

    assert_eq!(value["name"], "Ada");
    rejected.assert_calls_async(1).await;
    accepted.assert_calls_async(1).await;
}

/// A `stopReason: max_tokens` answer is incomplete even though it parses, so it
/// is re-asked with a raised budget rather than returned as a partial object.
#[tokio::test]
async fn a_truncated_answer_is_re_asked_with_a_raised_budget() {
    let server = MockServer::start_async().await;
    let truncated = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(NOVA_LITE))
                .body_includes(r#""maxTokens":1000"#)
                .body_excludes("cut off at maxTokens");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"toolUse":{
                        "toolUseId":"tu_1","name":"json_tool_call","input":{"name":"Ad"}}}]}},
                        "stopReason":"max_tokens"}"#,
                );
        })
        .await;
    // The raised budget is the *effective* cap: min(model cap 10_000, ceiling
    // 16_384). It must never exceed the configured ceiling.
    let retried = server
        .mock_async(|when, then| {
            when.method(POST)
                .path(converse_path(NOVA_LITE))
                .body_includes(r#""maxTokens":10000"#)
                .body_includes("cut off at maxTokens");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"toolUse":{
                        "toolUseId":"tu_2","name":"json_tool_call","input":{"name":"Ada"}}}]}},
                        "stopReason":"tool_use"}"#,
                );
        })
        .await;

    let value = adapter(&server, NOVA_LITE)
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
        .expect("the raised budget should complete the object");

    assert_eq!(value["name"], "Ada");
    truncated.assert_calls_async(1).await;
    retried.assert_calls_async(1).await;
}

/// ...but truncation *at* the effective cap is unrecoverable by design: raising
/// the budget further would breach the `llm_max_completion_tokens` ceiling, so
/// it fails terminally instead of looping until the retry budget is gone.
#[tokio::test]
async fn truncation_at_the_effective_cap_fails_terminally() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path(converse_path(NOVA_LITE));
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"output":{"message":{"content":[{"toolUse":{
                        "toolUseId":"tu_1","name":"json_tool_call","input":{"name":"Ad"}}}]}},
                        "stopReason":"max_tokens"}"#,
                );
        })
        .await;

    let error = adapter(&server, NOVA_LITE)
        .await
        .with_structured_output_retries(5)
        .create_structured_output_with_messages_raw(
            // nova-lite's 10_000 cap is already the effective budget.
            vec![Message::user("who?")],
            &schema(),
            None,
        )
        .await
        .expect_err("a truncation at the cap cannot be repaired");

    assert!(matches!(error, LlmError::InvalidResponse(_)), "{error:?}");
    assert!(
        error.to_string().contains("truncated"),
        "the error must name the cause: {error}",
    );
    mock.assert_calls_async(1).await;
}
