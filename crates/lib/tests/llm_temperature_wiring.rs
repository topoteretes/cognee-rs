#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `llm.temperature` (`LLM_TEMPERATURE`) reaches the wire on the
//! OpenAI-compatible and Anthropic adapters (SDK-1070). It used to reach only
//! Bedrock: the other factories ignored it.
//!
//! Each case sets the temperature through `ConfigManager::set`, builds the LLM
//! through `ComponentManager`, makes one call (an option-less `generate`, or a
//! structured-output call) against a mock server and asserts on the request
//! body it received.

use cognee::config::{ConfigManager, Settings};
use cognee::llm::{GenerationOptions, Message};
use cognee::{ComponentManager, PipelineContext};
use mockito::Matcher;
use serde_json::json;

fn config(values: &[(&str, serde_json::Value)]) -> ConfigManager {
    let cm = ConfigManager::new(Settings::default());
    // A mismatched request gets mockito's 501: fail it at once, unretried.
    cm.set("llm.min_retry_seconds", json!(0)).unwrap();
    cm.set("llm.network_retries", json!(1)).unwrap();
    for (key, value) in values {
        cm.set(key, value.clone()).unwrap();
    }
    cm
}

const OPENAI_REPLY: &str = r#"{
    "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "gpt-4o-mini",
    "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
}"#;

const ANTHROPIC_REPLY: &str = r#"{
    "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-test",
    "content": [{"type": "text", "text": "ok"}],
    "stop_reason": "end_turn",
    "usage": {"input_tokens": 1, "output_tokens": 1}
}"#;

async fn generate_once(cm: ConfigManager) {
    let components = ComponentManager::new(cm);
    let llm = components.llm().await.expect("llm builds");
    llm.generate(vec![Message::user("hello")], None)
        .await
        .expect("mock server answers");
}

#[tokio::test(flavor = "multi_thread")]
async fn openai_compatible_sends_the_configured_temperature() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_body(Matcher::PartialJson(json!({"temperature": 0.25})))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(OPENAI_REPLY)
        .expect(1)
        .create_async()
        .await;

    generate_once(config(&[
        ("llm_provider", json!("custom")),
        ("llm_model", json!("gpt-4o-mini")),
        ("llm_api_key", json!("sk-test")),
        ("llm_endpoint", json!(format!("{}/v1", server.url()))),
        ("llm_temperature", json!(0.25)),
    ]))
    .await;
    mock.assert_async().await;
}

/// A `temperature` given in `LLM_ARGS` wins over `llm.temperature`, as in
/// Python's fold.
#[tokio::test(flavor = "multi_thread")]
async fn llm_args_temperature_wins_over_the_setting() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_body(Matcher::PartialJson(json!({"temperature": 0.75})))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(OPENAI_REPLY)
        .expect(1)
        .create_async()
        .await;

    generate_once(config(&[
        ("llm_provider", json!("custom")),
        ("llm_model", json!("gpt-4o-mini")),
        ("llm_api_key", json!("sk-test")),
        ("llm_endpoint", json!(format!("{}/v1", server.url()))),
        ("llm_temperature", json!(0.25)),
        ("llm_args", json!({"temperature": 0.75})),
    ]))
    .await;
    mock.assert_async().await;
}

/// Unset, an option-less call keeps the built-in `0.0`, as before.
#[tokio::test(flavor = "multi_thread")]
async fn openai_compatible_keeps_the_builtin_temperature_when_unset() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/chat/completions")
        .match_body(Matcher::PartialJson(json!({"temperature": 0.0})))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(OPENAI_REPLY)
        .expect(1)
        .create_async()
        .await;

    generate_once(config(&[
        ("llm_provider", json!("custom")),
        ("llm_model", json!("gpt-4o-mini")),
        ("llm_api_key", json!("sk-test")),
        ("llm_endpoint", json!(format!("{}/v1", server.url()))),
    ]))
    .await;
    mock.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn anthropic_sends_the_configured_temperature() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/messages")
        .match_body(Matcher::PartialJson(json!({"temperature": 0.25})))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(ANTHROPIC_REPLY)
        .expect(1)
        .create_async()
        .await;

    generate_once(config(&[
        ("llm_provider", json!("anthropic")),
        ("llm_model", json!("claude-test")),
        ("llm_api_key", json!("sk-ant-test")),
        ("anthropic_base_url", json!(format!("{}/v1", server.url()))),
        ("llm_temperature", json!(0.25)),
    ]))
    .await;
    mock.assert_async().await;
}

/// A structured-output reply on the OpenAI tools path (the first mode the
/// `auto` cascade sends).
const OPENAI_TOOL_REPLY: &str = r#"{
    "id": "chatcmpl-2", "object": "chat.completion", "created": 0, "model": "gpt-4o-mini",
    "choices": [{"index": 0, "message": {"role": "assistant", "tool_calls": [
        {"id": "c1", "type": "function", "function": {
            "name": "extract_structured_data", "arguments": "{\"name\": \"ok\"}"
        }}
    ]}, "finish_reason": "tool_calls"}],
    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
}"#;

async fn structured_once(cm: ConfigManager, options: Option<GenerationOptions>) {
    let components = ComponentManager::new(cm);
    let llm = components.llm().await.expect("llm builds");
    let schema = json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"],
    });
    llm.create_structured_output_with_messages_raw(vec![Message::user("hello")], &schema, options)
        .await
        .expect("mock server answers");
}

async fn structured_mock(
    server: &mut mockito::ServerGuard,
    path: &str,
    temperature: f64,
) -> mockito::Mock {
    server
        .mock("POST", path)
        // Azure appends `?api-version=`.
        .match_query(Matcher::Any)
        .match_body(Matcher::PartialJson(json!({"temperature": temperature})))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(OPENAI_TOOL_REPLY)
        .expect(1)
        .create_async()
        .await
}

fn openai_compatible(server: &mockito::ServerGuard) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("llm_provider", json!("custom")),
        ("llm_model", json!("gpt-4o-mini")),
        ("llm_api_key", json!("sk-test")),
        ("llm_endpoint", json!(format!("{}/v1", server.url()))),
        ("llm_temperature", json!(0.25)),
    ]
}

/// An option-less structured call (search completions with a response
/// schema, feedback detection) sends the configured temperature, not the
/// built-in `0.0`, on the OpenAI-compatible provider.
#[tokio::test(flavor = "multi_thread")]
async fn openai_compatible_structured_output_sends_the_configured_temperature() {
    let mut server = mockito::Server::new_async().await;
    let mock = structured_mock(&mut server, "/v1/chat/completions", 0.25).await;
    structured_once(config(&openai_compatible(&server)), None).await;
    mock.assert_async().await;
}

/// A temperature the structured call picks itself still wins.
#[tokio::test(flavor = "multi_thread")]
async fn structured_output_options_temperature_wins() {
    let mut server = mockito::Server::new_async().await;
    let mock = structured_mock(&mut server, "/v1/chat/completions", 0.5).await;
    let options = GenerationOptions {
        temperature: Some(0.5),
        ..GenerationOptions::default()
    };
    structured_once(config(&openai_compatible(&server)), Some(options)).await;
    mock.assert_async().await;
}

/// The same on Azure, which builds the same adapter through its own factory.
#[tokio::test(flavor = "multi_thread")]
async fn azure_structured_output_sends_the_configured_temperature() {
    let mut server = mockito::Server::new_async().await;
    let mock = structured_mock(
        &mut server,
        "/openai/deployments/test/chat/completions",
        0.25,
    )
    .await;
    structured_once(
        config(&[
            ("llm_provider", json!("azure")),
            ("llm_model", json!("gpt-4o-mini")),
            ("llm_api_key", json!("azure-test")),
            (
                "llm_endpoint",
                json!(format!("{}/openai/deployments/test", server.url())),
            ),
            ("llm_api_version", json!("2024-12-01-preview")),
            ("llm_temperature", json!(0.25)),
        ]),
        None,
    )
    .await;
    mock.assert_async().await;
}
