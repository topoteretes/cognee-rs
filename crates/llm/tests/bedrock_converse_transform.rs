//! Converse request/response transforms and the Bedrock error taxonomy
//! (plan §1.4.2 / §4 R3 steps 3, 5, 6, 7).
#![cfg(feature = "bedrock")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code: panics are acceptable"
)]

use cognee_llm::adapters::bedrock::converse::{
    ConverseResponse, inference_config, is_retryable, map_error,
    merge_additional_model_request_fields, penalty_model_fields, split_messages,
};
use cognee_llm::error::LlmError;
use cognee_llm::types::{GenerationOptions, Message, TokenUsage};
use serde_json::{Map, Value, json};

#[test]
fn system_messages_are_hoisted_into_the_top_level_system_blocks() {
    // Converse has no `system` role inside `messages`.
    let (system, turns) = split_messages(&[
        Message::system("be terse"),
        Message::user("hello"),
        Message::assistant("hi"),
        Message::system("and helpful"),
    ]);

    assert_eq!(
        system,
        vec![
            json!({ "text": "be terse" }),
            json!({ "text": "and helpful" })
        ],
        "each system message becomes its own top-level block",
    );
    assert_eq!(turns.len(), 2, "no system turn survives in `messages`");
    assert_eq!(turns[0]["role"], "user");
    assert_eq!(
        turns[0]["content"],
        json!([{ "text": "hello" }]),
        "content is a block array, not a bare string",
    );
    assert_eq!(turns[1]["role"], "assistant");
    assert!(
        turns.iter().all(|turn| turn["role"] != "system"),
        "a system role inside `messages` would be rejected by Converse",
    );
}

#[test]
fn inference_config_carries_the_clamped_budget_and_the_sampling_params() {
    let opts = GenerationOptions {
        temperature: Some(0.25),
        max_tokens: Some(99_999),
        top_p: Some(0.9),
        stop: Some(vec!["STOP".to_string()]),
        ..Default::default()
    };
    // The adapter clamps before calling this; the transform reports what it is
    // handed, under Converse's camelCase names.
    let config = inference_config(&opts, 10_000);

    assert_eq!(config["maxTokens"], json!(10_000));
    assert_eq!(config["temperature"], json!(0.25));
    // `GenerationOptions` carries `f32`, so the JSON number is the widened
    // `f32` — the same thing the OpenAI and Anthropic adapters put on the wire.
    assert_eq!(config["topP"], json!(0.9_f32));
    assert_eq!(config["stopSequences"], json!(["STOP"]));
    // `max_tokens` is never echoed under its OpenAI name.
    assert!(config.get("max_tokens").is_none());
}

#[test]
fn unset_and_empty_sampling_params_are_omitted_rather_than_sent_as_null() {
    let opts = GenerationOptions {
        temperature: None,
        max_tokens: None,
        top_p: None,
        stop: Some(Vec::new()),
        ..Default::default()
    };
    let config = inference_config(&opts, 512);

    assert_eq!(config["maxTokens"], json!(512));
    for omitted in ["temperature", "topP", "stopSequences"] {
        assert!(
            config.get(omitted).is_none(),
            "{omitted} must be omitted, not null: {config}",
        );
    }
}

/// Documented decision: Converse's `inferenceConfig` has no frequency/presence
/// penalty, so they are routed through `additionalModelRequestFields` — the
/// destination litellm's own transform uses for inference params it does not
/// recognise.
#[test]
fn frequency_and_presence_penalties_go_to_additional_model_request_fields() {
    let none = penalty_model_fields(&GenerationOptions::default());
    assert!(
        none.is_empty(),
        "the default options set neither penalty, so nothing is added",
    );

    let opts = GenerationOptions {
        frequency_penalty: Some(0.25),
        presence_penalty: Some(0.5),
        ..Default::default()
    };
    let fields = penalty_model_fields(&opts);
    assert_eq!(fields["frequency_penalty"], json!(0.25));
    assert_eq!(fields["presence_penalty"], json!(0.5));

    // They never leak into `inferenceConfig`, which would 400.
    let config = inference_config(&opts, 512);
    assert!(config.get("frequency_penalty").is_none());
    assert!(config.get("presence_penalty").is_none());
}

#[test]
fn llm_args_fill_additional_model_request_fields_but_explicit_keys_win() {
    let llm_args: Map<String, Value> = json!({ "top_k": 40, "frequency_penalty": 0.9 })
        .as_object()
        .unwrap()
        .clone();
    let explicit: Map<String, Value> = json!({ "frequency_penalty": 0.1 })
        .as_object()
        .unwrap()
        .clone();

    let mut body = json!({ "messages": [] });
    merge_additional_model_request_fields(&mut body, &llm_args, &explicit);

    let fields = &body["additionalModelRequestFields"];
    // LLM_ARGS fills a gap the adapter never sets.
    assert_eq!(fields["top_k"], json!(40));
    // ...but an explicit key wins (litellm's `{**llm_args, **kwargs}`).
    assert_eq!(
        fields["frequency_penalty"],
        json!(0.1),
        "the explicit value must beat LLM_ARGS",
    );
}

#[test]
fn additional_model_request_fields_is_omitted_when_there_is_nothing_to_send() {
    let mut body = json!({ "messages": [] });
    merge_additional_model_request_fields(&mut body, &Map::new(), &Map::new());
    assert!(
        body.get("additionalModelRequestFields").is_none(),
        "an empty object is rejected by some models, so the key is dropped",
    );
}

#[test]
fn the_response_maps_text_and_usage() {
    let response: ConverseResponse = serde_json::from_value(json!({
        "output": { "message": { "role": "assistant", "content": [
            { "text": "Hello " },
            { "text": "world" }
        ]}},
        "stopReason": "end_turn",
        "usage": { "inputTokens": 11, "outputTokens": 3, "totalTokens": 14 }
    }))
    .unwrap();

    assert_eq!(response.text(), "Hello world");
    assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
    let usage = TokenUsage::from(response.usage.unwrap());
    assert_eq!(usage.prompt_tokens, 11);
    assert_eq!(usage.completion_tokens, 3);
    assert_eq!(usage.total_tokens, 14);
}

/// The mapping the plan singles out: **Bedrock signals throttling as HTTP 400
/// `ThrottlingException` as well as 429.** Mapping the 400 to a generic
/// bad-request error strands the retry layer.
#[test]
fn throttling_at_http_400_maps_to_rate_limit_exceeded() {
    let error = map_error(
        400,
        r#"{"__type":"ThrottlingException","message":"Too many requests, please wait before trying again."}"#,
    );
    assert!(
        matches!(error, LlmError::RateLimitExceeded(_)),
        "a 400 ThrottlingException must be a rate limit, not a generic request error: {error:?}",
    );
    assert!(
        is_retryable(&error),
        "the retry layer must engage on a throttle",
    );

    // The plain 429 form maps the same way...
    assert!(matches!(
        map_error(429, r#"{"message":"Too many requests"}"#),
        LlmError::RateLimitExceeded(_)
    ));
    // ...as does a namespaced `__type`, which is how the AWS-JSON envelope
    // usually spells it.
    assert!(matches!(
        map_error(
            400,
            r#"{"__type":"com.amazon.coral.availability#ThrottlingException","message":"slow down"}"#
        ),
        LlmError::RateLimitExceeded(_)
    ));
    // ModelNotReadyException is the other transient 400/429 shape.
    assert!(matches!(
        map_error(
            400,
            r#"{"__type":"ModelNotReadyException","message":"warming up"}"#
        ),
        LlmError::RateLimitExceeded(_)
    ));
}

#[test]
fn the_remaining_bedrock_exceptions_map_to_their_taxonomy_slots() {
    let validation = map_error(
        400,
        r#"{"__type":"ValidationException","message":"maxTokens exceeds the model limit"}"#,
    );
    assert!(
        matches!(validation, LlmError::InvalidResponse(_)),
        "{validation:?}"
    );
    assert!(
        !is_retryable(&validation),
        "re-POSTing an identical rejected body cannot start working",
    );

    let denied = map_error(
        403,
        r#"{"__type":"AccessDeniedException","message":"not authorized to invoke this model"}"#,
    );
    assert!(
        matches!(denied, LlmError::AuthenticationError(_)),
        "{denied:?}"
    );
    assert!(!is_retryable(&denied));

    let unavailable = map_error(
        503,
        r#"{"__type":"ServiceUnavailableException","message":"try again"}"#,
    );
    assert!(
        matches!(unavailable, LlmError::ApiError(_)),
        "{unavailable:?}"
    );
    assert!(is_retryable(&unavailable), "a 503 is transient");

    let missing = map_error(
        404,
        r#"{"__type":"ResourceNotFoundException","message":"no such model"}"#,
    );
    assert!(matches!(missing, LlmError::ModelNotFound(_)), "{missing:?}");
    assert!(!is_retryable(&missing));

    // Bodies with no `__type` still map off the status code.
    assert!(matches!(
        map_error(500, "internal failure"),
        LlmError::ApiError(_)
    ));
    assert!(matches!(
        map_error(401, "missing credentials"),
        LlmError::AuthenticationError(_)
    ));
}
