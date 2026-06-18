//! Audio transcription trait and types.
//!
//! Provides an async trait for converting audio bytes to text, with a default
//! implementation targeting the OpenAI Whisper API (`POST /v1/audio/transcriptions`).

use async_trait::async_trait;

use crate::error::{LlmError, LlmResult};

/// Audio formats accepted by the OpenAI Whisper API.
const SUPPORTED_AUDIO_FORMATS: &[&str] = &["mp3", "mp4", "mpeg", "mpga", "m4a", "wav", "webm"];

/// Validate that `format` is a supported audio format.
///
/// Returns `Ok(())` if the format (case-insensitive) is in the whitelist,
/// or `Err(LlmError::InvalidAudioFormat)` otherwise.
pub fn validate_audio_format(format: &str) -> LlmResult<()> {
    let lower = format.to_ascii_lowercase();
    if SUPPORTED_AUDIO_FORMATS.contains(&lower.as_str()) {
        Ok(())
    } else {
        Err(LlmError::InvalidAudioFormat(format.to_string()))
    }
}

/// Output of an audio transcription request.
#[derive(Debug, Clone)]
pub struct TranscriptionOutput {
    /// The transcribed text.
    pub text: String,
    /// The detected or specified language (e.g. `"english"`).
    pub language: Option<String>,
    /// Audio duration in seconds.
    pub duration: Option<f32>,
}

/// Trait for audio transcription backends.
///
/// Separate from [`crate::Llm`] because the Whisper endpoint uses a different
/// request shape (multipart upload), response shape, and error semantics.
#[async_trait]
pub trait Transcriber: Send + Sync {
    /// Transcribe audio bytes to text.
    ///
    /// # Arguments
    /// * `audio` - Raw audio file bytes (must be < 25 MB for OpenAI Whisper).
    /// * `format` - File extension without the dot: `"mp3"`, `"wav"`, etc.
    /// * `language_hint` - Optional ISO-639-1 language code (e.g. `"en"`).
    /// * `prompt_hint` - Optional vocabulary/context hint for the model.
    async fn transcribe_audio(
        &self,
        audio: &[u8],
        format: &str,
        language_hint: Option<&str>,
        prompt_hint: Option<&str>,
    ) -> LlmResult<TranscriptionOutput>;

    /// Return the name of the transcription model in use.
    fn transcription_model(&self) -> &str;
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;

    #[test]
    fn test_valid_formats() {
        for fmt in &["mp3", "mp4", "mpeg", "mpga", "m4a", "wav", "webm"] {
            assert!(
                validate_audio_format(fmt).is_ok(),
                "Expected {fmt} to be valid"
            );
        }
    }

    #[test]
    fn test_invalid_formats() {
        for fmt in &["mid", "aiff", "amr", "ogg", "flac", "aac", "wma"] {
            let result = validate_audio_format(fmt);
            assert!(result.is_err(), "Expected {fmt} to be invalid");
            assert!(
                matches!(result.unwrap_err(), LlmError::InvalidAudioFormat(_)),
                "Expected InvalidAudioFormat for {fmt}"
            );
        }
    }

    #[test]
    fn test_format_case_insensitive() {
        assert!(validate_audio_format("MP3").is_ok());
        assert!(validate_audio_format("Mp3").is_ok());
        assert!(validate_audio_format("WAV").is_ok());
        assert!(validate_audio_format("WebM").is_ok());
    }
}
