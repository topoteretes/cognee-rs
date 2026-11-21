//! Endpoint-specific request options must be explicit, regardless of model name.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]

use std::{sync::Mutex, time::Duration};

use cognee_llm::{
    DeltaSink, Llm, Message, MessageRole, OpenAIAdapter, StructuredOutputMode,
    build_openai_compatible_adapter,
};
use httpmock::{When, prelude::*};
use serde_json::{Value, json};

const MODEL: &str = "qwen3:8b";
const QUALIFIED_MODEL: &str = "Qwen/Qwen3-8B";
const STRUCTURED_MODES: [StructuredOutputMode; 4] = [
    StructuredOutputMode::JsonSchema,
    StructuredOutputMode::Tools,
    StructuredOutputMode::Functions,
    StructuredOutputMode::Json,
];

fn bounded(adapter: OpenAIAdapter) -> OpenAIAdapter {
    adapter
        .with_network_retries(0)
        .with_min_retry_elapsed(Duration::ZERO)
        .with_structured_output_retries(1)
        .with_http_timeouts(Duration::from_secs(5), Duration::from_secs(2))
}

fn adapter(server: &MockServer, model: &str, options: Option<&Value>) -> OpenAIAdapter {
    let adapter = bounded(OpenAIAdapter::new(model, "test-key", Some(server.base_url())).unwrap());
    configure_options(adapter, options)
}

fn configure_options(adapter: OpenAIAdapter, options: Option<&Value>) -> OpenAIAdapter {
    match options {
        Some(options) => adapter.with_extra_args(options.as_object().unwrap().clone()),
        None => adapter,
    }
}

fn request_options(when: When, model: &str, options: Option<&Value>) -> When {
    let when = when
        .method(POST)
        .json_body_includes(json!({"model": model}).to_string());
    match options {
        Some(options) => when.json_body_includes(options.to_string()),
        None => when
            .body_excludes("\"think\":")
            .body_excludes("\"reasoning\":"),
    }
}

fn messages() -> Vec<Message> {
    vec![Message {
        role: MessageRole::User,
        content: "hello".into(),
    }]
}

fn completion(message: Value) -> Value {
    json!({
        "id": "test", "object": "chat.completion", "created": 1, "model": MODEL,
        "choices": [{"index": 0, "message": message, "finish_reason": "stop"}]
    })
}

async fn assert_generate(model: &str, options: Option<&Value>) {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            request_options(when, model, options)
                .path("/chat/completions")
                .body_excludes("\"stream\":true");
            then.status(200)
                .json_body(completion(json!({"role": "assistant", "content": "Hello"})));
        })
        .await;
    let result = adapter(&server, model, options)
        .generate(messages(), None)
        .await;
    mock.assert_calls_async(1).await;
    assert_eq!(result.unwrap().content, "Hello");
}

#[derive(Default)]
struct CollectDeltas(Mutex<Vec<String>>);

impl DeltaSink for CollectDeltas {
    fn begin_attempt(&self) {
        self.0.lock().unwrap().clear();
    }

    fn delta(&self, text: &str) {
        self.0.lock().unwrap().push(text.into());
    }
}

async fn assert_streaming(options: Option<&Value>) {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            request_options(when, QUALIFIED_MODEL, options)
                .path("/chat/completions")
                .json_body_includes(r#"{"stream":true,"stream_options":{"include_usage":true}}"#);
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(concat!(
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n",
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n",
                    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2,\"total_tokens\":4}}\n\n",
                    "data: [DONE]\n\n",
                ));
        })
        .await;
    let sink = CollectDeltas::default();
    let result = adapter(&server, QUALIFIED_MODEL, options)
        .generate_streaming(messages(), None, &sink)
        .await;
    mock.assert_calls_async(1).await;
    let result = result.unwrap();
    assert_eq!(result.content, "Hello");
    assert_eq!(result.finish_reason.as_deref(), Some("stop"));
    assert_eq!(result.usage.unwrap().total_tokens, 4);
    assert_eq!(*sink.0.lock().unwrap(), ["Hel", "lo"]);
}

async fn assert_structured(mode: StructuredOutputMode, options: Option<&Value>) {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            let when = request_options(when, MODEL, options).path("/chat/completions");
            let message = match mode {
                StructuredOutputMode::JsonSchema => {
                    when.json_body_includes(r#"{"response_format":{"type":"json_schema","json_schema":{"strict":true}}}"#)
                        .body_excludes("\"tools\":")
                        .body_excludes("\"functions\":");
                    json!({"role":"assistant", "content":"{\"foo\":\"bar\"}"})
                }
                StructuredOutputMode::Tools => {
                    when.body_includes("\"tools\":")
                        .body_excludes("\"functions\":")
                        .body_excludes("\"response_format\":");
                    json!({"role":"assistant", "tool_calls":[{
                        "id":"call_1", "type":"function", "function":{
                            "name":"extract_structured_data", "arguments":"{\"foo\":\"bar\"}"
                        }
                    }]})
                }
                StructuredOutputMode::Functions => {
                    when.body_includes("\"functions\":")
                        .body_excludes("\"tools\":")
                        .body_excludes("\"response_format\":");
                    json!({"role":"assistant", "function_call":{
                        "name":"extract_structured_data", "arguments":"{\"foo\":\"bar\"}"
                    }})
                }
                StructuredOutputMode::Json => {
                    when.json_body_includes(r#"{"response_format":{"type":"json_object"}}"#)
                        .body_excludes("\"tools\":")
                        .body_excludes("\"functions\":");
                    json!({"role":"assistant", "content":"{\"foo\":\"bar\"}"})
                }
                StructuredOutputMode::Auto => unreachable!("these tests pin each request mode"),
            };
            then.status(200).json_body(completion(message));
        })
        .await;
    let schema = json!({
        "type":"object", "properties":{"foo":{"type":"string"}}, "required":["foo"]
    });
    let result = adapter(&server, MODEL, options)
        .with_structured_output_mode(mode)
        .create_structured_output_raw("input", "Extract data", &schema, None)
        .await;
    mock.assert_calls_async(1).await;
    assert_eq!(result.unwrap(), json!({"foo":"bar"}));
}

#[tokio::test]
async fn generate_omits_implicit_endpoint_options() {
    assert_generate(MODEL, None).await;
    assert_generate(QUALIFIED_MODEL, None).await;
    assert_generate("gpt-4o-mini", None).await;
}

#[tokio::test]
async fn streaming_omits_implicit_endpoint_options() {
    assert_streaming(None).await;
}

#[tokio::test]
async fn structured_modes_omit_implicit_endpoint_options() {
    for mode in STRUCTURED_MODES {
        assert_structured(mode, None).await;
    }
}

#[tokio::test]
async fn generate_preserves_explicit_endpoint_options() {
    assert_generate(
        MODEL,
        Some(&json!({"think":true, "reasoning":{"effort":"high"}})),
    )
    .await;
}

#[tokio::test]
async fn streaming_preserves_explicit_endpoint_options() {
    assert_streaming(Some(&json!({"think":true, "reasoning":{"effort":"high"}}))).await;
}

#[tokio::test]
async fn structured_modes_preserve_explicit_endpoint_options() {
    let options = json!({"think":true, "reasoning":{"effort":"high"}});
    for mode in STRUCTURED_MODES {
        assert_structured(mode, Some(&options)).await;
    }
}

#[tokio::test]
async fn former_defaults_remain_explicitly_configurable() {
    assert_generate(
        MODEL,
        Some(&json!({"think":false, "reasoning":{"effort":"none"}})),
    )
    .await;
}

#[tokio::test]
async fn ollama_factory_uses_compatible_route_and_explicit_options() {
    for options in [
        None,
        Some(json!({"think":true, "reasoning":{"effort":"high"}})),
    ] {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                request_options(when, MODEL, options.as_ref()).path("/v1/chat/completions");
                then.status(200)
                    .json_body(completion(json!({"role":"assistant", "content":"Hello"})));
            })
            .await;
        let adapter = build_openai_compatible_adapter(
            "ollama",
            "ollama/qwen3:8b",
            "test-key",
            &server.url("/v1"),
            1,
        )
        .unwrap();
        let result = configure_options(bounded(adapter), options.as_ref())
            .generate(messages(), None)
            .await;
        mock.assert_calls_async(1).await;
        assert_eq!(result.unwrap().content, "Hello");
    }
}
