//! Wire-level regression for application identity on OpenAI-compatible calls.

#![allow(
    clippy::expect_used,
    reason = "integration test code — panics are acceptable failures"
)]

use cognee_llm::{Llm, Message, MessageRole, OpenAIAdapter};
use httpmock::prelude::*;

#[tokio::test]
async fn configured_user_agent_is_sent_on_chat_requests() {
    let server = MockServer::start_async().await;
    let request = server
        .mock_async(|when, then| {
            when.method(POST)
                .path("/chat/completions")
                .header("user-agent", "Apex/test");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id":"chatcmpl-test",
                        "object":"chat.completion",
                        "created":1,
                        "model":"gpt-5.4-nano",
                        "choices":[{
                            "index":0,
                            "message":{"role":"assistant","content":"ok"},
                            "finish_reason":"stop"
                        }]
                    }"#,
                );
        })
        .await;

    let adapter = OpenAIAdapter::new("gpt-5.4-nano", "test-key", Some(server.base_url()))
        .expect("adapter")
        .with_user_agent(Some("Apex/test".to_string()))
        .with_network_retries(0);

    let response = adapter
        .generate(
            vec![Message {
                role: MessageRole::User,
                content: "ping".to_string(),
            }],
            None,
        )
        .await
        .expect("mocked request succeeds");

    assert_eq!(response.content, "ok");
    request.assert_async().await;
}
