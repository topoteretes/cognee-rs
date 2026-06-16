//! Mock Transcriber implementation for deterministic testing.
//!
//! Returns canned responses from a queue, enabling unit tests for audio
//! transcription pipeline stages without requiring a real Whisper API endpoint.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "mock infrastructure — panics are acceptable"
)]

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use cognee_llm::LlmResult;
use cognee_llm::transcriber::{Transcriber, TranscriptionOutput, validate_audio_format};

/// A test-only transcriber that pops pre-loaded responses from an internal queue.
///
/// # Usage
///
/// ```ignore
/// use cognee_test_utils::MockTranscriber;
/// use cognee_llm::TranscriptionOutput;
///
/// let mock = MockTranscriber::new(vec![
///     TranscriptionOutput { text: "Hello world".into(), language: Some("en".into()), duration: Some(1.5) },
/// ]);
/// let transcriber: Arc<dyn Transcriber> = Arc::new(mock);
/// ```
///
/// When the queue is exhausted, subsequent calls return an empty
/// `TranscriptionOutput`.
pub struct MockTranscriber {
    responses: Mutex<VecDeque<TranscriptionOutput>>,
    model: String,
}

impl MockTranscriber {
    /// Create a new `MockTranscriber` pre-loaded with the given responses.
    ///
    /// Responses are returned in FIFO order.
    pub fn new(model: &str, responses: Vec<TranscriptionOutput>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
            model: model.to_string(),
        }
    }

    /// Create a `MockTranscriber` that always returns an empty transcription.
    pub fn empty() -> Self {
        Self {
            responses: Mutex::new(VecDeque::new()),
            model: "mock-whisper".to_string(),
        }
    }
}

#[async_trait]
impl Transcriber for MockTranscriber {
    async fn transcribe_audio(
        &self,
        _audio: &[u8],
        format: &str,
        _language_hint: Option<&str>,
        _prompt_hint: Option<&str>,
    ) -> LlmResult<TranscriptionOutput> {
        // Validate format even in mock, so tests catch invalid-format errors.
        validate_audio_format(format)?;

        let mut queue = self
            .responses
            .lock()
            .expect("MockTranscriber lock poisoned"); // lock poison is unrecoverable
        Ok(queue.pop_front().unwrap_or(TranscriptionOutput {
            text: String::new(),
            language: None,
            duration: None,
        }))
    }

    fn transcription_model(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_llm::LlmError;

    #[tokio::test]
    async fn test_returns_queued_responses() {
        let mock = MockTranscriber::new(
            "mock-whisper",
            vec![
                TranscriptionOutput {
                    text: "first".to_string(),
                    language: Some("en".to_string()),
                    duration: Some(1.0),
                },
                TranscriptionOutput {
                    text: "second".to_string(),
                    language: None,
                    duration: None,
                },
            ],
        );

        let r1 = mock
            .transcribe_audio(b"fake", "mp3", None, None)
            .await
            .unwrap();
        assert_eq!(r1.text, "first");
        assert_eq!(r1.language.as_deref(), Some("en"));
        assert_eq!(r1.duration, Some(1.0));

        let r2 = mock
            .transcribe_audio(b"fake", "wav", None, None)
            .await
            .unwrap();
        assert_eq!(r2.text, "second");
        assert!(r2.language.is_none());
    }

    #[tokio::test]
    async fn test_returns_empty_when_exhausted() {
        let mock = MockTranscriber::empty();
        let result = mock
            .transcribe_audio(b"fake", "mp3", None, None)
            .await
            .unwrap();
        assert!(result.text.is_empty());
        assert!(result.language.is_none());
        assert!(result.duration.is_none());
    }

    #[tokio::test]
    async fn test_validates_format() {
        let mock = MockTranscriber::empty();
        let result = mock.transcribe_audio(b"fake", "mid", None, None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            LlmError::InvalidAudioFormat(_)
        ));
    }
}
