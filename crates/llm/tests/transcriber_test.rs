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
