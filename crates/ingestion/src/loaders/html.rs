//! HTML file loader -- BeautifulSoup-equivalent text extraction.
//!
//! Handles `document_type = "html"`. Wraps the rule-driven
//! [`extract_html`](crate::url_crawler::extract_html) extractor (a port of
//! Python's `BeautifulSoupLoader`, used by both the URL crawler and this
//! loader) and applies Python's plain-text fallback for inputs that contain
//! no HTML markup.

use async_trait::async_trait;
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput};
use crate::url_crawler::extract_html;

/// Loader for HTML documents.
///
/// Decodes raw bytes as UTF-8 (stripping the BOM), runs the 39-rule
/// extraction set, and returns [`LoaderOutput::Text`] for downstream
/// paragraph chunking. If extraction yields nothing and the input has no HTML
/// tags, the raw text is returned instead -- mirroring Python's
/// `beautiful_soup_loader.py:213-216`, which treats pre-extracted plain text
/// (e.g. content fetched with `format="text"`) as the content itself.
pub struct HtmlLoader;

#[async_trait]
impl DocumentLoader for HtmlLoader {
    async fn extract(&self, bytes: &[u8], _doc: &Document) -> Result<LoaderOutput, LoaderError> {
        // Strip UTF-8 BOM if present (0xEF 0xBB 0xBF).
        let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &bytes[3..]
        } else {
            bytes
        };

        let raw = String::from_utf8(bytes.to_vec())
            .map_err(|e| LoaderError::InvalidUtf8(e.to_string()))?;

        let extracted = extract_html(&raw);

        // Plain-text fallback: if the rules extracted nothing and the input
        // doesn't look like HTML, treat the raw content as plain text.
        let text = if extracted.is_empty() && !looks_like_html(&raw) {
            raw.trim().to_string()
        } else {
            extracted
        };

        Ok(LoaderOutput::Text(text))
    }

    fn engine_name(&self) -> &'static str {
        "beautiful_soup_loader"
    }
}

/// Heuristic for whether `input` contains HTML markup.
///
/// Approximates Python's `soup.find()` check: an angle bracket immediately
/// followed by a tag-name letter or a closing-tag slash indicates a tag.
fn looks_like_html(input: &str) -> bool {
    let bytes = input.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'<'
            && let Some(&next) = bytes.get(i + 1)
            && (next.is_ascii_alphabetic() || next == b'/')
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use cognee_models::DataPoint;
    use uuid::Uuid;

    fn test_doc() -> Document {
        let mut base = DataPoint::new("TextDocument", None);
        base.id = Uuid::new_v4();
        Document {
            base,
            document_type: "html".to_string(),
            name: "test.html".to_string(),
            raw_data_location: "file:///test.html".to_string(),
            mime_type: "text/html".to_string(),
            extension: "html".to_string(),
            data_id: Uuid::new_v4(),
            external_metadata: None,
        }
    }

    async fn extract_text(loader: &HtmlLoader, input: &[u8]) -> String {
        match loader.extract(input, &test_doc()).await.expect("ok") {
            LoaderOutput::Text(s) => s,
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn extracts_body_text() {
        let html = b"<html><head><title>T</title></head><body><p>Hello world</p></body></html>";
        let text = extract_text(&HtmlLoader, html).await;
        assert!(text.contains("Hello world"), "got: {text}");
    }

    #[tokio::test]
    async fn strips_script_and_style() {
        let html = b"<html><body><script>var x = 1;</script><style>p{color:red}</style><p>Visible</p></body></html>";
        let text = extract_text(&HtmlLoader, html).await;
        assert!(text.contains("Visible"), "got: {text}");
        assert!(!text.contains("var x"), "script leaked: {text}");
        assert!(!text.contains("color:red"), "style leaked: {text}");
    }

    #[tokio::test]
    async fn plain_text_fallback_when_no_tags() {
        // No HTML markup -> treated as plain text.
        let input = b"just some plain text, no tags here";
        let text = extract_text(&HtmlLoader, input).await;
        assert_eq!(text, "just some plain text, no tags here");
    }

    #[tokio::test]
    async fn bom_is_stripped() {
        let mut input = vec![0xEF, 0xBB, 0xBF];
        input.extend_from_slice(b"plain bom text");
        let text = extract_text(&HtmlLoader, &input).await;
        assert_eq!(text, "plain bom text");
    }

    #[tokio::test]
    async fn empty_input_returns_empty() {
        let text = extract_text(&HtmlLoader, b"").await;
        assert_eq!(text, "");
    }

    #[tokio::test]
    async fn invalid_utf8_errors() {
        let result = HtmlLoader.extract(&[0xFF, 0xFE], &test_doc()).await;
        assert!(matches!(result, Err(LoaderError::InvalidUtf8(_))));
    }

    #[tokio::test]
    async fn engine_name_matches() {
        assert_eq!(HtmlLoader.engine_name(), "beautiful_soup_loader");
    }
}
