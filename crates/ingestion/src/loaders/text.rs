//! Text file loader -- UTF-8 decode with BOM stripping.
//!
//! Handles `document_type = "text"`. Relocates the logic previously
//! hard-coded at `crates/cognify/src/tasks.rs:166-167`.

use async_trait::async_trait;
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput};

/// Loader for plain-text documents.
///
/// Decodes raw bytes as UTF-8, stripping the BOM (byte-order mark) if
/// present. Returns [`LoaderOutput::Text`] for downstream paragraph
/// chunking.
pub struct TextLoader;

#[async_trait]
impl DocumentLoader for TextLoader {
    async fn extract(&self, bytes: &[u8], _doc: &Document) -> Result<LoaderOutput, LoaderError> {
        // Strip UTF-8 BOM if present (0xEF 0xBB 0xBF)
        let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &bytes[3..]
        } else {
            bytes
        };

        let text = String::from_utf8(bytes.to_vec())
            .map_err(|e| LoaderError::InvalidUtf8(e.to_string()))?;

        Ok(LoaderOutput::Text(text))
    }

    fn engine_name(&self) -> &'static str {
        "text_loader"
    }
}

#[cfg(test)]
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
    async fn bom_is_stripped() {
        let loader = TextLoader;
        let doc = make_test_doc();
        let mut input = vec![0xEF, 0xBB, 0xBF];
        input.extend_from_slice(b"hello");
        let result = loader.extract(&input, &doc).await.unwrap();
        match result {
            LoaderOutput::Text(s) => assert_eq!(s, "hello"),
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
