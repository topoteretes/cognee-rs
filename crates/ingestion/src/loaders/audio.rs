//! Audio document loader — Whisper-API transcription.
//!
//! Extracts text from audio documents by delegating to a [`Transcriber`]
//! (Whisper API call). The resulting transcript is returned as
//! [`LoaderOutput::Text`] so it is subsequently processed by the normal
//! paragraph chunker, matching Python SDK behaviour.
//!
//! Engine name `"audio_loader"` matches the Python `loader_engine` metadata
//! column for cross-SDK parity.
//!
//! **Format validation (D4):** only the formats accepted by the OpenAI Whisper
//! API are supported: `mp3`, `mp4`, `mpeg`, `mpga`, `m4a`, `wav`, `webm`.
//! Any other format produces [`LoaderError::UnsupportedFormat`] immediately,
//! before attempting the API call.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_llm::{Transcriber, validate_audio_format};
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput};

/// Loader for audio documents.
///
/// Holds a reference to a [`Transcriber`]. On `extract`, validates the audio
/// format, then sends the raw audio bytes to
/// [`Transcriber::transcribe_audio`] and returns the transcript as plain text.
///
/// **Fail-fast on unsupported format (D4):** formats outside the Whisper
/// whitelist produce `LoaderError::UnsupportedFormat` immediately.
pub struct AudioLoader {
    transcriber: Arc<dyn Transcriber>,
}

impl AudioLoader {
    /// Create a new `AudioLoader` backed by the given transcriber.
    pub fn new(transcriber: Arc<dyn Transcriber>) -> Self {
        Self { transcriber }
    }
}

#[async_trait]
impl DocumentLoader for AudioLoader {
    async fn extract(&self, bytes: &[u8], doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let format = audio_format(doc);
        validate_audio_format(&format)
            .map_err(|_| LoaderError::UnsupportedFormat(format!("audio format '{format}'")))?;
        let out = self
            .transcriber
            .transcribe_audio(bytes, &format, None, None)
            .await
            .map_err(|e| LoaderError::ExtractionFailed(e.to_string()))?;
        Ok(LoaderOutput::Text(out.text))
    }

    fn engine_name(&self) -> &'static str {
        "audio_loader"
    }
}

/// Derive the audio format string for a document.
///
/// Resolution order:
/// 1. `doc.extension` — lowercased and stripped of a leading `.` if present.
/// 2. The last `.`-delimited component of `doc.name` (e.g. `speech.mp3` → `"mp3"`).
/// 3. `"unknown"` as a fallback (will be rejected by `validate_audio_format`).
pub fn audio_format(doc: &Document) -> String {
    // Prefer the structured extension field.
    let ext = doc.extension.trim_start_matches('.').to_ascii_lowercase();
    if !ext.is_empty() {
        return ext;
    }

    // Fall back to the extension embedded in the document name.
    let from_name = doc
        .name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if !from_name.is_empty() && from_name != doc.name.to_ascii_lowercase() {
        return from_name;
    }

    "unknown".to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cognee_llm::TranscriptionOutput;
    use cognee_models::DataPoint;
    use cognee_test_utils::MockTranscriber;
    use uuid::Uuid;

    use super::*;

    fn make_audio_doc(name: &str, extension: &str) -> Document {
        let mut base = DataPoint::new("AudioDocument", None);
        base.id = Uuid::new_v4();
        Document {
            base,
            document_type: "audio".to_string(),
            name: name.to_string(),
            raw_data_location: format!("file:///storage/{name}"),
            mime_type: "audio/mpeg".to_string(),
            extension: extension.to_string(),
            data_id: Uuid::new_v4(),
            external_metadata: None,
        }
    }

    // --- DocumentLoader trait tests ---

    #[tokio::test]
    async fn extract_returns_text_with_transcript() {
        let mock = Arc::new(MockTranscriber::new(
            "mock-whisper",
            vec![TranscriptionOutput {
                text: "Hello world".to_string(),
                language: Some("en".to_string()),
                duration: Some(2.5),
            }],
        ));
        let loader = AudioLoader::new(mock);
        let doc = make_audio_doc("speech.mp3", "mp3");

        let result = loader
            .extract(b"fake-mp3-bytes", &doc)
            .await
            .expect("extract should succeed for supported format");
        match result {
            LoaderOutput::Text(text) => assert_eq!(text, "Hello world"),
            other => panic!("expected LoaderOutput::Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn engine_name_is_audio_loader() {
        let mock = Arc::new(MockTranscriber::empty());
        let loader = AudioLoader::new(mock);
        assert_eq!(loader.engine_name(), "audio_loader");
    }

    #[tokio::test]
    async fn extract_returns_unsupported_format_for_flac() {
        let mock = Arc::new(MockTranscriber::empty());
        let loader = AudioLoader::new(mock);
        let doc = make_audio_doc("audio.flac", "flac");

        let result = loader.extract(b"fake-flac-bytes", &doc).await;
        assert!(result.is_err(), "flac should be rejected");
        assert!(
            matches!(result.unwrap_err(), LoaderError::UnsupportedFormat(_)),
            "error should be UnsupportedFormat for flac"
        );
    }

    #[tokio::test]
    async fn extract_returns_unsupported_format_for_ogg() {
        let mock = Arc::new(MockTranscriber::empty());
        let loader = AudioLoader::new(mock);
        let doc = make_audio_doc("audio.ogg", "ogg");

        let result = loader.extract(b"fake-ogg-bytes", &doc).await;
        assert!(result.is_err(), "ogg should be rejected");
        assert!(
            matches!(result.unwrap_err(), LoaderError::UnsupportedFormat(_)),
            "error should be UnsupportedFormat for ogg"
        );
    }

    #[tokio::test]
    async fn extract_returns_unsupported_format_for_aac() {
        let mock = Arc::new(MockTranscriber::empty());
        let loader = AudioLoader::new(mock);
        let doc = make_audio_doc("audio.aac", "aac");

        let result = loader.extract(b"fake-aac-bytes", &doc).await;
        assert!(
            matches!(result.unwrap_err(), LoaderError::UnsupportedFormat(_)),
            "error should be UnsupportedFormat for aac"
        );
    }

    #[tokio::test]
    async fn extract_returns_unsupported_format_for_mid() {
        let mock = Arc::new(MockTranscriber::empty());
        let loader = AudioLoader::new(mock);
        let doc = make_audio_doc("audio.mid", "mid");

        let result = loader.extract(b"fake-mid-bytes", &doc).await;
        assert!(
            matches!(result.unwrap_err(), LoaderError::UnsupportedFormat(_)),
            "error should be UnsupportedFormat for mid"
        );
    }

    #[tokio::test]
    async fn extract_returns_unsupported_format_for_amr() {
        let mock = Arc::new(MockTranscriber::empty());
        let loader = AudioLoader::new(mock);
        let doc = make_audio_doc("audio.amr", "amr");

        let result = loader.extract(b"fake-amr-bytes", &doc).await;
        assert!(
            matches!(result.unwrap_err(), LoaderError::UnsupportedFormat(_)),
            "error should be UnsupportedFormat for amr"
        );
    }

    #[tokio::test]
    async fn extract_returns_unsupported_format_for_aiff() {
        let mock = Arc::new(MockTranscriber::empty());
        let loader = AudioLoader::new(mock);
        let doc = make_audio_doc("audio.aiff", "aiff");

        let result = loader.extract(b"fake-aiff-bytes", &doc).await;
        assert!(
            matches!(result.unwrap_err(), LoaderError::UnsupportedFormat(_)),
            "error should be UnsupportedFormat for aiff"
        );
    }

    // --- audio_format helper tests ---

    #[test]
    fn audio_format_prefers_extension_field() {
        let doc = make_audio_doc("audio.wav", "mp3");
        // extension field is "mp3", name says "wav" — extension wins
        assert_eq!(audio_format(&doc), "mp3");
    }

    #[test]
    fn audio_format_falls_back_to_name() {
        let doc = make_audio_doc("speech.wav", "");
        assert_eq!(audio_format(&doc), "wav");
    }

    #[test]
    fn audio_format_strips_leading_dot() {
        let doc = make_audio_doc("audio.mp3", ".mp3");
        assert_eq!(audio_format(&doc), "mp3");
    }

    #[test]
    fn audio_format_lowercases() {
        let doc = make_audio_doc("audio.MP3", "MP3");
        assert_eq!(audio_format(&doc), "mp3");
    }

    #[test]
    fn audio_format_returns_unknown_when_no_extension() {
        let doc = make_audio_doc("audiofile", "");
        assert_eq!(audio_format(&doc), "unknown");
    }
}
