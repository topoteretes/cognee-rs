//! Image document loader — vision-LLM description.
//!
//! Extracts text from image documents by delegating to an LLM's
//! `transcribe_image` method (vision API call). The resulting description is
//! returned as [`LoaderOutput::Text`] so it is subsequently processed by the
//! normal paragraph chunker, matching Python SDK behaviour.
//!
//! Engine name `"image_loader"` matches the Python `loader_engine` metadata
//! column for cross-SDK parity.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_llm::Llm;
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput};

/// Loader for image documents.
///
/// Holds a reference to a vision-capable LLM. On `extract`, sends the raw
/// image bytes together with a best-effort MIME type to
/// [`Llm::transcribe_image`] and returns the description as plain text.
///
/// **Fail-fast on no-vision (D6):** if the configured model does not support
/// vision, `transcribe_image` returns `LlmError::FeatureNotSupported` which is
/// mapped to `LoaderError::ExtractionFailed`. The error propagates up and
/// aborts the cognify run — there is no skip / partial-success path.
pub struct ImageLoader {
    llm: Arc<dyn Llm>,
}

impl ImageLoader {
    /// Create a new `ImageLoader` backed by the given LLM.
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }
}

#[async_trait]
impl DocumentLoader for ImageLoader {
    async fn extract(&self, bytes: &[u8], doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let mime = image_mime_type(doc);
        let description = self
            .llm
            .transcribe_image(bytes, &mime, None)
            .await
            .map_err(|e| LoaderError::ExtractionFailed(e.to_string()))?;
        Ok(LoaderOutput::Text(description))
    }

    fn engine_name(&self) -> &'static str {
        "image_loader"
    }
}

/// Derive the MIME type for an image document.
///
/// Resolution order:
/// 1. `doc.mime_type` if it already starts with `"image/"`.
/// 2. `mime_guess` from the file extension in `doc.name` (e.g. `photo.jpg` → `image/jpeg`).
/// 3. `mime_guess` from the path in `doc.raw_data_location` as a last resort.
/// 4. `"image/jpeg"` as a universal fallback.
pub fn image_mime_type(doc: &Document) -> String {
    // Prefer the stored mime_type if it is already an image MIME.
    if doc.mime_type.starts_with("image/") {
        return doc.mime_type.clone();
    }

    // Try to infer from the document name (typically has the original extension).
    if let Some(mime) = mime_guess::from_path(&doc.name).first()
        && mime.type_() == mime_guess::mime::IMAGE
    {
        return mime.to_string();
    }

    // Fall back to the raw_data_location path (file:// URI or plain path).
    let location = doc.raw_data_location.trim_start_matches("file://");
    if let Some(mime) = mime_guess::from_path(location).first()
        && mime.type_() == mime_guess::mime::IMAGE
    {
        return mime.to_string();
    }

    // Universal fallback — JPEG is the most common image format and is
    // accepted by all vision APIs this codebase targets.
    "image/jpeg".to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::Arc;

    use cognee_models::DataPoint;
    use cognee_test_utils::MockLlm;
    use uuid::Uuid;

    use super::*;

    fn make_image_doc(name: &str, mime_type: &str) -> Document {
        let mut base = DataPoint::new("ImageDocument", None);
        base.id = Uuid::new_v4();
        Document {
            base,
            document_type: "image".to_string(),
            name: name.to_string(),
            raw_data_location: format!("file:///storage/{name}"),
            mime_type: mime_type.to_string(),
            extension: name.rsplit('.').next().unwrap_or("").to_string(),
            data_id: Uuid::new_v4(),
            external_metadata: None,
        }
    }

    // --- DocumentLoader trait tests ---

    #[tokio::test]
    async fn extract_returns_text_with_vision_response() {
        let llm =
            Arc::new(MockLlm::new(vec![]).with_vision_responses(vec!["A red square.".to_string()]));
        let loader = ImageLoader::new(llm);
        let doc = make_image_doc("photo.png", "image/png");

        let result = loader.extract(b"fake-png-bytes", &doc).await;
        let result = result.expect("extract should succeed when vision response is available");
        match result {
            LoaderOutput::Text(text) => assert_eq!(text, "A red square."),
            other => panic!("expected LoaderOutput::Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn engine_name_is_image_loader() {
        let llm = Arc::new(MockLlm::empty());
        let loader = ImageLoader::new(llm);
        assert_eq!(loader.engine_name(), "image_loader");
    }

    #[tokio::test]
    async fn extract_fails_when_no_vision_responses_queued() {
        // MockLlm with no vision responses returns FeatureNotSupported,
        // which the loader maps to ExtractionFailed (D6 fail-fast).
        let llm = Arc::new(MockLlm::empty());
        let loader = ImageLoader::new(llm);
        let doc = make_image_doc("photo.jpg", "image/jpeg");

        let result = loader.extract(b"fake-jpg-bytes", &doc).await;
        assert!(
            result.is_err(),
            "should fail when no vision responses queued"
        );
        assert!(
            matches!(result.unwrap_err(), LoaderError::ExtractionFailed(_)),
            "error should be ExtractionFailed"
        );
    }

    // --- image_mime_type helper tests ---

    #[test]
    fn mime_type_from_doc_mime_type_field() {
        let doc = make_image_doc("photo.jpg", "image/jpeg");
        assert_eq!(image_mime_type(&doc), "image/jpeg");
    }

    #[test]
    fn mime_type_falls_back_to_name_extension() {
        // doc.mime_type is not an image/ MIME; derive from doc.name.
        let doc = make_image_doc("photo.png", "application/octet-stream");
        assert_eq!(image_mime_type(&doc), "image/png");
    }

    #[test]
    fn mime_type_falls_back_to_location_extension() {
        // doc.mime_type and doc.name are both unhelpful; derive from raw_data_location.
        let mut doc = make_image_doc("unknownfile", "application/octet-stream");
        doc.raw_data_location = "file:///storage/archive.gif".to_string();
        assert_eq!(image_mime_type(&doc), "image/gif");
    }

    #[test]
    fn mime_type_defaults_to_jpeg_when_all_else_fails() {
        let mut doc = make_image_doc("noext", "text/plain");
        doc.raw_data_location = "file:///storage/noext".to_string();
        assert_eq!(image_mime_type(&doc), "image/jpeg");
    }
}
