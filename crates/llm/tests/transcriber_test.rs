//! Integration tests for the `Transcriber` trait and `OpenAIAdapter` implementation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration test code — panics are acceptable"
)]

use cognee_llm::LlmError;
use cognee_llm::adapters::OpenAIAdapter;
use cognee_llm::transcriber::Transcriber;

use httpmock::prelude::*;

#[tokio::test]
async fn test_transcription_request_shape() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/audio/transcriptions")
            .header("Authorization", "Bearer test-key")
            .header_exists("Content-Type"); // multipart/form-data with boundary
        then.status(200).json_body(serde_json::json!({
            "text": "Hello world",
            "language": "english",
            "duration": 1.5
        }));
    });

    let base_url = server.base_url();
    let adapter = OpenAIAdapter::new("gpt-4", "test-key", Some(base_url))
        .unwrap()
        .with_network_retries(0);

    let result = adapter
        .transcribe_audio(b"fake-audio-bytes", "mp3", None, None)
        .await
        .unwrap();

    assert_eq!(result.text, "Hello world");
    assert_eq!(result.language.as_deref(), Some("english"));
    assert_eq!(result.duration, Some(1.5));
    mock.assert_calls(1);
}

#[tokio::test]
async fn test_transcription_with_optional_fields() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/audio/transcriptions");
        then.status(200).json_body(serde_json::json!({
            "text": "Technical transcription",
            "language": "en",
            "duration": 3.2
        }));
    });

    let base_url = server.base_url();
    let adapter = OpenAIAdapter::new("gpt-4", "test-key", Some(base_url))
        .unwrap()
        .with_network_retries(0);

    let result = adapter
        .transcribe_audio(b"fake-audio", "wav", Some("en"), Some("technical terms"))
        .await
        .unwrap();

    assert_eq!(result.text, "Technical transcription");
    mock.assert_calls(1);
}

#[tokio::test]
async fn test_invalid_format_no_http_call() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/audio/transcriptions");
        then.status(200).json_body(serde_json::json!({
            "text": "should not reach here"
        }));
    });

    let base_url = server.base_url();
    let adapter = OpenAIAdapter::new("gpt-4", "test-key", Some(base_url))
        .unwrap()
        .with_network_retries(0);

    let result = adapter.transcribe_audio(b"fake", "mid", None, None).await;

    assert!(result.is_err());
    assert!(
        matches!(result.unwrap_err(), LlmError::InvalidAudioFormat(_)),
        "Expected InvalidAudioFormat error"
    );
    mock.assert_calls(0);
}

#[tokio::test]
#[ignore]
async fn test_live_openai_transcription() {
    let token = match std::env::var("OPENAI_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            eprintln!("Skipping live transcription test: OPENAI_TOKEN not set");
            return;
        }
    };

    let adapter = OpenAIAdapter::new("gpt-4", &token, None).unwrap();

    // Generate a minimal valid WAV file (1 second of silence, 8kHz, 16-bit mono).
    let wav_bytes = generate_silent_wav(8000, 1);

    let result = adapter
        .transcribe_audio(&wav_bytes, "wav", Some("en"), None)
        .await;

    // A silent WAV may return empty text or a short string; we just verify
    // the request succeeded and returned a TranscriptionOutput.
    match result {
        Ok(output) => {
            eprintln!("Transcription output: {output:?}");
            // Duration should be approximately 1 second
            if let Some(dur) = output.duration {
                assert!(dur > 0.5 && dur < 2.0, "Unexpected duration: {dur}");
            }
        }
        Err(e) => {
            panic!("Live transcription failed: {e}");
        }
    }
}

/// Generate a minimal WAV file containing silence.
fn generate_silent_wav(sample_rate: u32, duration_secs: u32) -> Vec<u8> {
    let num_samples = sample_rate * duration_secs;
    let data_size = num_samples * 2; // 16-bit = 2 bytes per sample
    let file_size = 36 + data_size;

    let mut buf = Vec::with_capacity(file_size as usize + 8);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt sub-chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data sub-chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    buf.resize(buf.len() + data_size as usize, 0); // silence = zeros

    buf
}

// ── Retry classification parity with `call_api` ─────────────────────────────
//
// The Whisper path runs its own retry loop, so it needs the same terminal set as
// `OpenAIAdapter::call_api`: HTTP 400..=402 and 404 are terminal, as is a 429
// whose body reports an exhausted quota rather than a per-minute limit. A plain
// 429 or a 5xx stays transient. These assert on *exact* request counts, since
// the point is that a terminal failure costs one request rather than the full
// ladder.
//
// The transient cases deliberately use `with_network_retries(1)`: the
// transcription path does not honour `Retry-After`, so every retry pays a real
// 4-8s backoff and one is enough to prove the ladder still runs.

/// Build an adapter pointed at the mock server with an explicit retry budget.
fn retry_adapter(server: &MockServer, retries: u32) -> OpenAIAdapter {
    OpenAIAdapter::new("gpt-4", "test-key", Some(server.base_url()))
        .expect("adapter builds from a mock base URL")
        .with_network_retries(retries)
}

#[tokio::test]
async fn transcription_402_is_terminal_after_one_request() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/audio/transcriptions");
            then.status(402)
                .body(r#"{"error":{"message":"Payment required"}}"#);
        })
        .await;

    let err = retry_adapter(&server, 3)
        .transcribe_audio(b"fake-audio", "mp3", None, None)
        .await
        .expect_err("a 402 must surface as an error");

    assert!(
        matches!(err, LlmError::PaymentRequired(_)),
        "expected PaymentRequired, got {err:?}"
    );
    assert_eq!(
        mock.calls_async().await,
        1,
        "a 402 is terminal: it must cost exactly one request"
    );
}

#[tokio::test]
async fn transcription_404_is_terminal_after_one_request() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/audio/transcriptions");
            then.status(404)
                .body(r#"{"error":{"message":"The model `whisper-9` does not exist"}}"#);
        })
        .await;

    let err = retry_adapter(&server, 3)
        .transcribe_audio(b"fake-audio", "mp3", None, None)
        .await
        .expect_err("a 404 must surface as an error");

    assert!(
        matches!(err, LlmError::ModelNotFound(_)),
        "expected ModelNotFound, got {err:?}"
    );
    assert_eq!(
        mock.calls_async().await,
        1,
        "an unknown transcription model is terminal: exactly one request"
    );
}

#[tokio::test]
async fn transcription_quota_exhausted_429_is_terminal_after_one_request() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/audio/transcriptions");
            then.status(429).body(
                r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota"}}"#,
            );
        })
        .await;

    let err = retry_adapter(&server, 3)
        .transcribe_audio(b"fake-audio", "mp3", None, None)
        .await
        .expect_err("an exhausted quota must surface as an error");

    assert!(
        matches!(err, LlmError::PaymentRequired(_)),
        "an `insufficient_quota` 429 is a billing failure, not a rate limit; got {err:?}"
    );
    assert_eq!(
        mock.calls_async().await,
        1,
        "no wait clears an exhausted quota: exactly one request"
    );
}

#[tokio::test]
async fn transcription_plain_429_still_retries() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/audio/transcriptions");
            then.status(429)
                .body(r#"{"error":{"message":"Rate limit reached for whisper-1"}}"#);
        })
        .await;

    let err = retry_adapter(&server, 1)
        .transcribe_audio(b"fake-audio", "mp3", None, None)
        .await
        .expect_err("an exhausted retry budget must surface an error");

    assert!(
        matches!(err, LlmError::MaxRetriesExceeded(_)),
        "expected MaxRetriesExceeded, got {err:?}"
    );
    assert_eq!(
        mock.calls_async().await,
        2,
        "a plain rate limit is transient and must use the whole budget"
    );
}

#[tokio::test]
async fn transcription_5xx_still_retries() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/audio/transcriptions");
            then.status(503).body("service unavailable");
        })
        .await;

    let err = retry_adapter(&server, 1)
        .transcribe_audio(b"fake-audio", "mp3", None, None)
        .await
        .expect_err("an exhausted retry budget must surface an error");

    assert!(
        matches!(err, LlmError::MaxRetriesExceeded(_)),
        "expected MaxRetriesExceeded, got {err:?}"
    );
    assert_eq!(
        mock.calls_async().await,
        2,
        "a 5xx is transient and must use the whole budget"
    );
}
