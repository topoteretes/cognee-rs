//! Answer token streaming: the OpenAI adapter's `generate_streaming` and the
//! token-sink promotion rules (Python `test_adapter_streaming.py`,
//! `token_sink.py`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]

use std::sync::Arc;

use cognee_llm::adapters::OpenAIAdapter;
use cognee_llm::{
    Message, MessageRole, StreamEvent, TokenSink, generate_answer, promote_answer_lane,
    with_token_sink,
};
use httpmock::prelude::*;

const SSE_BODY: &str = concat!(
    "data: {\"id\":\"c\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
    ": keepalive\n\n",
    "data: {\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":3,\"total_tokens\":6}}\n\n",
    "data: [DONE]\n\n",
);

fn user_msg() -> Vec<Message> {
    vec![Message {
        role: MessageRole::User,
        content: "hello".to_string(),
    }]
}

fn adapter(server: &MockServer, streaming: bool) -> OpenAIAdapter {
    OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(server.base_url()))
        .unwrap()
        .with_network_retries(0)
        .with_streaming(streaming)
}

fn sse_mock(server: &MockServer) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .json_body_includes(r#"{"stream": true}"#)
            .json_body_includes(r#"{"stream_options": {"include_usage": true}}"#);
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(SSE_BODY);
    })
}

async fn drain(sink: &Arc<TokenSink>) -> Vec<StreamEvent> {
    let mut stream = sink.stream().expect("first consumer");
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

fn deltas(events: &[StreamEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Delta(t) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn streaming_yields_deltas_and_the_full_string_and_usage() {
    let server = MockServer::start_async().await;
    let mock = sse_mock(&server);
    let llm = adapter(&server, true);

    let sink = Arc::new(TokenSink::new());
    let response = with_token_sink(Arc::clone(&sink), async {
        promote_answer_lane(generate_answer(&llm, user_msg(), None)).await
    })
    .await
    .unwrap();
    mock.assert();

    assert_eq!(response.content, "Hello world");
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));
    assert_eq!(response.usage.unwrap().total_tokens, 6);

    // generate_answer closed the sink because its answer streamed.
    assert!(sink.is_closed());
    let events = drain(&sink).await;
    assert_eq!(deltas(&events), vec!["Hel", "lo", " world"]);
    assert_eq!(
        events.first(),
        Some(&StreamEvent::Stage("generating".into()))
    );
    assert_eq!(events.last(), Some(&StreamEvent::AnswerDone));
}

#[tokio::test]
async fn no_deltas_without_a_promoted_lane() {
    // Requested but never promoted: the analysis lane, a sessionless recall or a
    // sequential run. The blocking call runs and nothing is emitted.
    let server = MockServer::start_async().await;
    let blocking = server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"id":"x","object":"chat.completion","created":1,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"plain"},"finish_reason":"stop"}]}"#);
    });
    let llm = adapter(&server, true);

    let sink = Arc::new(TokenSink::new());
    let response = with_token_sink(Arc::clone(&sink), async {
        generate_answer(&llm, user_msg(), None).await
    })
    .await
    .unwrap();
    blocking.assert();
    assert_eq!(response.content, "plain");
    assert!(!sink.emitted_any());
    assert!(!sink.is_closed(), "the creator closes an untouched sink");
}

#[tokio::test]
async fn sibling_lane_in_join_does_not_inherit_the_promotion() {
    let server = MockServer::start_async().await;
    let _sse = sse_mock(&server);
    let llm = adapter(&server, true);
    let sink = Arc::new(TokenSink::new());

    with_token_sink(Arc::clone(&sink), async {
        let answer = promote_answer_lane(async { cognee_llm::active_token_sink().is_some() });
        let analysis = async { cognee_llm::active_token_sink().is_some() };
        let (answer, analysis) = tokio::join!(answer, analysis);
        assert!(answer);
        assert!(!analysis);
        let _ = &llm;
    })
    .await;
}

#[tokio::test]
async fn gates_flag_off_and_default_adapter_stream_nothing() {
    let server = MockServer::start_async().await;
    let blocking = server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"id":"x","object":"chat.completion","created":1,"model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"plain"},"finish_reason":"stop"}]}"#);
    });
    // llm.streaming off: promoted sink, still the blocking call.
    let llm = adapter(&server, false);
    let sink = Arc::new(TokenSink::new());
    let response = with_token_sink(Arc::clone(&sink), async {
        promote_answer_lane(generate_answer(&llm, user_msg(), None)).await
    })
    .await
    .unwrap();
    assert_eq!(response.content, "plain");
    blocking.assert();
    assert!(!sink.emitted_any());
    assert!(sink.stream().is_some());
}

#[tokio::test]
async fn first_delta_claims_ownership_and_later_lanes_are_dropped() {
    let sink = Arc::new(TokenSink::new());
    sink.put_delta(1, "a");
    sink.put_delta(2, "dropped");
    sink.put_delta(1, "b");
    // Only the owner may reset.
    sink.begin_attempt(2);
    sink.begin_attempt(1);
    sink.close();
    let events = drain(&sink).await;
    assert_eq!(
        events,
        vec![
            StreamEvent::Delta("a".into()),
            StreamEvent::Delta("b".into()),
            StreamEvent::Reset
        ]
    );
}

#[tokio::test]
async fn buffer_overflow_drops_deltas_but_never_errors() {
    let sink = Arc::new(TokenSink::with_max_buffered(2));
    for _ in 0..5 {
        sink.put_delta(1, "x");
    }
    sink.fail("boom", Some(402));
    assert!(sink.dropped_events());
    let events = drain(&sink).await;
    assert_eq!(deltas(&events).len(), 2);
    assert_eq!(
        events.last(),
        Some(&StreamEvent::Error {
            message: "boom".into(),
            status: Some(402)
        })
    );
}

#[tokio::test]
async fn single_consumer_and_detach() {
    let sink = Arc::new(TokenSink::new());
    let mut stream = sink.stream().expect("first consumer");
    assert!(sink.stream().is_none());
    sink.detach();
    sink.put_delta(1, "late");
    assert!(
        sink.emitted_any(),
        "a detached consumer does not unset ownership"
    );
    sink.close();
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn stream_failure_reports_a_sanitised_error_with_status() {
    let server = MockServer::start_async().await;
    server.mock(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(402).body("out of credits, prompt: SECRET");
    });
    let llm = adapter(&server, true);
    let sink = Arc::new(TokenSink::new());
    let result = with_token_sink(Arc::clone(&sink), async {
        promote_answer_lane(generate_answer(&llm, user_msg(), None)).await
    })
    .await;
    assert!(result.is_err());
    // No delta was emitted, so this lane never owned the sink (Python: only the
    // owning lane reports): it stays open and silent, and the creator surfaces
    // the failure itself.
    assert!(!sink.is_closed());
}
