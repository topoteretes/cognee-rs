//! Text file loader -- strict UTF-8 decode with universal newlines; a BOM is
//! kept, as Python does.
//!
//! Handles `document_type = "text"`. Relocates the logic previously
//! hard-coded at `crates/cognify/src/tasks.rs:166-167`.

use async_trait::async_trait;
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput, universal_newlines};

/// Loader for plain-text documents.
///
/// Decodes raw bytes as UTF-8. A BOM (byte-order mark) is kept as a leading
/// U+FEFF: Python's `text_loader` opens the file with `encoding="utf-8"`, not
/// `utf-8-sig` (`text_loader.py:82`), which does not drop it. Line endings are
/// translated as that text-mode read does (universal newlines: `\r\n` and a
/// lone `\r` become `\n`), so the stored text, `raw_content_hash` and chunks
/// match Python's. Returns [`LoaderOutput::Text`] for downstream paragraph
/// chunking.
pub struct TextLoader;

#[async_trait]
impl DocumentLoader for TextLoader {
    async fn extract(&self, bytes: &[u8], _doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let text =
            std::str::from_utf8(bytes).map_err(|e| LoaderError::InvalidUtf8(e.to_string()))?;

        Ok(LoaderOutput::Text(universal_newlines(text)))
    }

    fn engine_name(&self) -> &'static str {
        "text_loader"
    }
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

    fn make_test_doc() -> Document {
        let mut base = DataPoint::new("TextDocument", None);
        base.id = Uuid::new_v4();
        Document {
            base,
            document_type: "text".to_string(),
            name: "test.txt".to_string(),
            raw_data_location: "file:///test.txt".to_string(),
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: Uuid::new_v4(),
            external_metadata: None,
        }
    }

    #[tokio::test]
    async fn valid_utf8_returns_text() {
        let loader = TextLoader;
        let doc = make_test_doc();
        let result = loader.extract(b"Hello world", &doc).await.unwrap();
        match result {
            LoaderOutput::Text(s) => assert_eq!(s, "Hello world"),
            _ => panic!("expected LoaderOutput::Text"),
        }
    }

    #[tokio::test]
    async fn bom_is_kept() {
        let loader = TextLoader;
        let doc = make_test_doc();
        let mut input = vec![0xEF, 0xBB, 0xBF];
        input.extend_from_slice(b"hello");
        let result = loader.extract(&input, &doc).await.unwrap();
        match result {
            LoaderOutput::Text(s) => assert_eq!(s, "\u{feff}hello"),
            _ => panic!("expected LoaderOutput::Text"),
        }
    }

    #[tokio::test]
    async fn crlf_and_lone_cr_become_lf() {
        let result = TextLoader
            .extract(b"a\r\nb\rc\nd\r", &make_test_doc())
            .await;
        match result.unwrap() {
            LoaderOutput::Text(s) => assert_eq!(s, "a\nb\nc\nd\n"),
            _ => panic!("expected LoaderOutput::Text"),
        }
    }

    /// Python 3 `open(path, encoding="utf-8").read()` of these bytes returns
    /// `'﻿a\nb\nc'`: the BOM stays and the newlines are translated.
    #[tokio::test]
    async fn bom_is_kept_while_newlines_are_translated() {
        let result = TextLoader
            .extract(b"\xEF\xBB\xBFa\r\nb\rc", &make_test_doc())
            .await;
        match result.unwrap() {
            LoaderOutput::Text(s) => assert_eq!(s, "\u{feff}a\nb\nc"),
            _ => panic!("expected LoaderOutput::Text"),
        }
    }

    #[tokio::test]
    async fn invalid_utf8_returns_error() {
        let loader = TextLoader;
        let doc = make_test_doc();
        let result = loader.extract(&[0xFF, 0xFE], &doc).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, LoaderError::InvalidUtf8(_)));
    }

    #[tokio::test]
    async fn engine_name_is_text_loader() {
        assert_eq!(TextLoader.engine_name(), "text_loader");
    }

    #[tokio::test]
    async fn empty_input_returns_empty_text() {
        let loader = TextLoader;
        let doc = make_test_doc();
        let result = loader.extract(b"", &doc).await.unwrap();
        match result {
            LoaderOutput::Text(s) => assert_eq!(s, ""),
            _ => panic!("expected LoaderOutput::Text"),
        }
    }
}
