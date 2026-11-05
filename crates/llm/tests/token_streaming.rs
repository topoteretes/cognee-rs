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
async fn a_failure_before_any_delta_leaves_the_sink_to_its_creator() {
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

#[tokio::test]
async fn llm_args_stream_options_are_merged_and_include_usage_is_forced() {
    // Python `test_include_usage_survives_caller_supplied_stream_options` and
    // `test_sampling_parameters_are_not_dropped`: a caller's `stream_options` is
    // merged, never replaced, `include_usage` is forced on, and the rest of
    // `LLM_ARGS` reaches the streamed request as it reaches the blocking one.
    let server = MockServer::start_async().await;
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/chat/completions")
            .json_body_includes(r#"{"stream": true}"#)
            .json_body_includes(
                r#"{"stream_options": {"include_usage": true, "other_key": "value"}}"#,
            )
            .json_body_includes(r#"{"seed": 7}"#);
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(SSE_BODY);
    });
    let mut args = serde_json::Map::new();
    args.insert(
        "stream_options".into(),
        serde_json::json!({"other_key": "value", "include_usage": false}),
    );
    args.insert("stream".into(), serde_json::json!(false));
    args.insert("seed".into(), serde_json::json!(7));
    let llm = adapter(&server, true).with_extra_args(args);

    let sink = Arc::new(TokenSink::new());
    let response = with_token_sink(Arc::clone(&sink), async {
        promote_answer_lane(generate_answer(&llm, user_msg(), None)).await
    })
    .await
    .unwrap();
    mock.assert();
    assert_eq!(response.content, "Hello world");
}

/// A raw HTTP/1.1 server: the first connection streams one delta and then dies
/// mid-body; every later one serves the full [`SSE_BODY`].
async fn flaky_sse_server() -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut connection = 0;
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            connection += 1;
            // Read the whole request (headers + Content-Length body).
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let body = if connection == 1 {
                "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n"
            } else {
                SSE_BODY
            };
            let mut out = format!("{head}{:x}\r\n{body}\r\n", body.len());
            if connection > 1 {
                out.push_str("0\r\n\r\n");
            }
            socket.write_all(out.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            // Dropping the socket without the terminating chunk truncates the
            // first response mid-body.
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_mid_stream_failure_retries_and_tells_the_consumer_to_reset() {
    // Python `test_a_retry_tells_the_consumer_to_discard_the_partial_answer`,
    // driven through the real adapter: the retry ladder re-streams from the
    // beginning, so the owning lane emits `reset` before the second attempt.
    let base = flaky_sse_server().await;
    let llm = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(base))
        .unwrap()
        .with_network_retries(2)
        .with_min_retry_elapsed(std::time::Duration::ZERO)
        .with_streaming(true);

    let sink = Arc::new(TokenSink::new());
    let response = with_token_sink(Arc::clone(&sink), async {
        promote_answer_lane(generate_answer(&llm, user_msg(), None)).await
    })
    .await
    .unwrap();
    assert_eq!(response.content, "Hello world", "only the retry's text");

    let events = drain(&sink).await;
    assert_eq!(
        events,
        vec![
            StreamEvent::Stage("generating".into()),
            StreamEvent::Delta("partial".into()),
            StreamEvent::Reset,
            StreamEvent::Delta("Hel".into()),
            StreamEvent::Delta("lo".into()),
            StreamEvent::Delta(" world".into()),
            StreamEvent::AnswerDone,
        ]
    );
}

#[tokio::test]
async fn a_failure_after_deltas_ends_the_stream_with_one_sanitised_error() {
    // The owning lane failed after it streamed: the consumer already has a 200,
    // so the failure reaches it as one `error` event with no provider detail.
    let base = flaky_sse_server().await;
    let llm = OpenAIAdapter::new("gpt-4o-mini", "test-key", Some(base))
        .unwrap()
        .with_network_retries(0)
        .with_min_retry_elapsed(std::time::Duration::ZERO)
        .with_streaming(true);

    let sink = Arc::new(TokenSink::new());
    let result = with_token_sink(Arc::clone(&sink), async {
        promote_answer_lane(generate_answer(&llm, user_msg(), None)).await
    })
    .await;
    assert!(result.is_err());
    assert!(sink.is_closed());
    let events = drain(&sink).await;
    let errors: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Error { message, status } => Some((message.clone(), *status)),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1, "{events:?}");
    let (message, status) = &errors[0];
    assert!(message.ends_with("during answer generation"), "{message}");
    assert!(!message.contains("127.0.0.1"), "{message}");
    assert_eq!(*status, None, "a network failure has no HTTP status");
}
