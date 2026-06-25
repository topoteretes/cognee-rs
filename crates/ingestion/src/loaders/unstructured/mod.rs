//! Unstructured document loader dispatching to format-specific extractors.
//!
//! Handles `document_type = "unstructured"` documents by dispatching
//! based on file extension to the appropriate format extractor.
//! Each format is behind its own feature flag.

#[cfg(feature = "unstructured-docx")]
mod docx;
#[cfg(feature = "unstructured-eml")]
mod eml;
#[cfg(feature = "unstructured-epub")]
mod epub;
#[cfg(feature = "unstructured-odp")]
mod odp;
#[cfg(feature = "unstructured-odt")]
mod odt;
#[cfg(feature = "unstructured-pptx")]
mod pptx;
#[cfg(feature = "unstructured-xlsx")]
mod xlsx;

use async_trait::async_trait;
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput};

/// Loader for unstructured document formats (XLSX, DOCX, PPTX, EPUB, EML, ODT, ODP).
///
/// Dispatches to format-specific extractors based on the document's file extension.
/// Each format extractor is behind a feature flag to keep the dependency tree minimal.
pub struct UnstructuredLoader;

#[async_trait]
impl DocumentLoader for UnstructuredLoader {
    async fn extract(&self, bytes: &[u8], doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let ext = doc.extension.to_lowercase();
        let text = dispatch_by_extension(&ext, bytes)?;
        Ok(LoaderOutput::Text(text))
    }

    fn engine_name(&self) -> &'static str {
        "unstructured_loader"
    }
}

fn dispatch_by_extension(ext: &str, bytes: &[u8]) -> Result<String, LoaderError> {
    match ext {
        #[cfg(feature = "unstructured-xlsx")]
        "xlsx" | "xls" | "ods" => xlsx::extract(bytes),

        #[cfg(not(feature = "unstructured-xlsx"))]
        "xlsx" | "xls" | "ods" => Err(LoaderError::UnsupportedFormat(format!(
            "Spreadsheet format '.{ext}' requires the 'unstructured-xlsx' feature"
        ))),

        #[cfg(feature = "unstructured-docx")]
        "docx" => docx::extract(bytes),

        #[cfg(not(feature = "unstructured-docx"))]
        "docx" => Err(LoaderError::UnsupportedFormat(
            "DOCX format requires the 'unstructured-docx' feature".to_string(),
        )),

        "doc" => Err(LoaderError::UnsupportedFormat(
            "Format '.doc' is not supported in the Rust SDK; use the Python SDK or convert to .docx".to_string(),
        )),

        #[cfg(feature = "unstructured-pptx")]
        "pptx" => pptx::extract(bytes),

        #[cfg(not(feature = "unstructured-pptx"))]
        "pptx" => Err(LoaderError::UnsupportedFormat(
            "PPTX format requires the 'unstructured-pptx' feature".to_string(),
        )),

        "ppt" => Err(LoaderError::UnsupportedFormat(
            "Format '.ppt' is not supported in the Rust SDK; use the Python SDK or convert to .pptx".to_string(),
        )),

        #[cfg(feature = "unstructured-epub")]
        "epub" => epub::extract(bytes),

        #[cfg(not(feature = "unstructured-epub"))]
        "epub" => Err(LoaderError::UnsupportedFormat(
            "EPUB format requires the 'unstructured-epub' feature".to_string(),
        )),

        #[cfg(feature = "unstructured-eml")]
        "eml" => eml::extract(bytes),

        #[cfg(not(feature = "unstructured-eml"))]
        "eml" => Err(LoaderError::UnsupportedFormat(
            "EML format requires the 'unstructured-eml' feature".to_string(),
        )),

        #[cfg(feature = "unstructured-odt")]
        "odt" => odt::extract(bytes),

        #[cfg(not(feature = "unstructured-odt"))]
        "odt" => Err(LoaderError::UnsupportedFormat(
            "ODT format requires the 'unstructured-odt' feature".to_string(),
        )),

        #[cfg(feature = "unstructured-odp")]
        "odp" => odp::extract(bytes),

        #[cfg(not(feature = "unstructured-odp"))]
        "odp" => Err(LoaderError::UnsupportedFormat(
            "ODP format requires the 'unstructured-odp' feature".to_string(),
        )),

        "rtf" => Err(LoaderError::UnsupportedFormat(
            "Format '.rtf' is not supported in the Rust SDK; use the Python SDK or convert to a supported format (.docx, .txt)".to_string(),
        )),

        "msg" => Err(LoaderError::UnsupportedFormat(
            "Format '.msg' is not supported in the Rust SDK; use the Python SDK or convert to .eml".to_string(),
        )),

        _ => Err(LoaderError::UnsupportedFormat(format!(
            "Unstructured format '.{ext}' is not supported"
        ))),
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

    fn make_doc(ext: &str) -> Document {
        Document {
            base: DataPoint::new("UnstructuredDocument", None),
            document_type: "unstructured".to_string(),
            name: format!("test.{ext}"),
            raw_data_location: format!("file:///tmp/test.{ext}"),
            mime_type: "application/octet-stream".to_string(),
            extension: ext.to_string(),
            data_id: Uuid::new_v4(),
            external_metadata: None,
        }
    }

    #[tokio::test]
    async fn engine_name_is_unstructured_loader() {
        assert_eq!(UnstructuredLoader.engine_name(), "unstructured_loader");
    }

    #[tokio::test]
    async fn unsupported_doc_format() {
        let loader = UnstructuredLoader;
        let doc = make_doc("doc");
        let result = loader.extract(b"data", &doc).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, LoaderError::UnsupportedFormat(_)));
        assert!(err.to_string().contains(".doc"));
    }

    #[tokio::test]
    async fn unsupported_ppt_format() {
        let loader = UnstructuredLoader;
        let doc = make_doc("ppt");
        let result = loader.extract(b"data", &doc).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, LoaderError::UnsupportedFormat(_)));
        assert!(err.to_string().contains(".ppt"));
    }

    #[tokio::test]
    async fn unsupported_rtf_format() {
        let loader = UnstructuredLoader;
        let doc = make_doc("rtf");
        let result = loader.extract(b"data", &doc).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, LoaderError::UnsupportedFormat(_)));
        assert!(err.to_string().contains(".rtf"));
    }

    #[tokio::test]
    async fn unsupported_msg_format() {
        let loader = UnstructuredLoader;
        let doc = make_doc("msg");
        let result = loader.extract(b"data", &doc).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, LoaderError::UnsupportedFormat(_)));
        assert!(err.to_string().contains(".msg"));
    }

    #[tokio::test]
    async fn unknown_extension_returns_error() {
        let loader = UnstructuredLoader;
        let doc = make_doc("xyz");
        let result = loader.extract(b"data", &doc).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, LoaderError::UnsupportedFormat(_)));
    }
}
