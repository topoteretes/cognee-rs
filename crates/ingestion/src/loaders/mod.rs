//! Document loader dispatch architecture.
//!
//! Defines the [`DocumentLoader`] trait, [`LoaderOutput`] enum,
//! [`LoaderRegistry`] struct, and [`LoaderError`] type for routing
//! document content through type-specific extraction logic.

#[cfg(feature = "csv-loader")]
pub mod csv_loader;
#[cfg(any(feature = "pdf-pdfium", feature = "pdf-pure-rust"))]
pub mod pdf;
pub mod text;
#[cfg(feature = "unstructured")]
pub mod unstructured;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_models::Document;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can occur during document content extraction.
#[derive(Debug, Error)]
pub enum LoaderError {
    #[error("Invalid UTF-8 in document content: {0}")]
    InvalidUtf8(String),

    #[error("Unsupported document format: {0}")]
    UnsupportedFormat(String),

    #[error("IO error during extraction: {0}")]
    IoError(String),

    #[error("Extraction failed: {0}")]
    ExtractionFailed(String),
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Async trait for extracting content from raw document bytes.
///
/// Implementations handle a specific document type (text, PDF, CSV, etc.).
/// The trait is `Send + Sync` for use in async pipelines with `Arc`.
#[async_trait]
pub trait DocumentLoader: Send + Sync {
    /// Extract text content from raw document bytes.
    ///
    /// `bytes` is the raw content retrieved from storage via
    /// `StorageTrait::retrieve`. `doc` provides metadata (extension,
    /// mime_type, etc.) that loaders may use for format decisions.
    async fn extract(&self, bytes: &[u8], doc: &Document) -> Result<LoaderOutput, LoaderError>;

    /// Python-compatible engine name for cross-SDK metadata parity.
    ///
    /// Must match the Python loader's `loader_name` property so that
    /// the `loader_engine` column in the metadata DB is comparable
    /// across SDKs.
    fn engine_name(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// The result of a [`DocumentLoader::extract`] call, determining how
/// the extracted content is chunked downstream.
#[derive(Debug)]
pub enum LoaderOutput {
    /// Text content to be chunked via `chunk_text` (paragraph strategy).
    /// Used by: text, PDF, unstructured, image, audio, HTML loaders.
    Text(String),

    /// Pre-split rows to be chunked via `chunk_by_row`.
    /// Each string is one row (e.g., "col: val, col: val" for CSV).
    /// The rows are joined with `"\n\n"` before passing to `chunk_by_row`,
    /// matching the Python input format.
    /// Used by: CSV loader.
    Rows(Vec<String>),

    /// A single pre-formed chunk. No further chunking applied.
    /// Used by: DLT short-circuit (though DLT is handled before loader
    /// dispatch, this variant exists for any future loader that needs
    /// to emit exactly one chunk).
    SingleChunk {
        text: String,
        cut_type: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Maps document type strings to their corresponding [`DocumentLoader`]
/// implementations.
///
/// The registry is constructed once per cognify pipeline run and passed
/// to `extract_chunks_from_documents`.
pub struct LoaderRegistry {
    loaders: HashMap<String, Arc<dyn DocumentLoader>>,
}

impl LoaderRegistry {
    pub fn new() -> Self {
        Self {
            loaders: HashMap::new(),
        }
    }

    /// Register a loader for a document type.
    ///
    /// `document_type` values match `Document.document_type`:
    /// "text", "pdf", "csv", "image", "audio", "unstructured".
    pub fn register(&mut self, document_type: &str, loader: Arc<dyn DocumentLoader>) {
        self.loaders.insert(document_type.to_string(), loader);
    }

    /// Look up the loader for a document type.
    pub fn get(&self, document_type: &str) -> Option<&Arc<dyn DocumentLoader>> {
        self.loaders.get(document_type)
    }

    /// Build a registry with all currently-available loaders.
    ///
    /// Feature-gated loaders are only registered when their feature
    /// is enabled. If a feature is disabled, the corresponding
    /// document type simply has no registered loader and will produce
    /// an `UnsupportedDocumentType` error at dispatch time.
    pub fn default_registry() -> Self {
        let mut registry = Self::new();
        registry.register("text", Arc::new(text::TextLoader));

        #[cfg(any(feature = "pdf-pdfium", feature = "pdf-pure-rust"))]
        registry.register("pdf", Arc::new(pdf::PdfLoader));

        #[cfg(feature = "csv-loader")]
        registry.register("csv", Arc::new(csv_loader::CsvLoader));

        #[cfg(feature = "unstructured")]
        registry.register("unstructured", Arc::new(unstructured::UnstructuredLoader));

        registry
    }
}

impl Default for LoaderRegistry {
    fn default() -> Self {
        Self::default_registry()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_text_loader() {
        let registry = LoaderRegistry::default();
        let loader = registry.get("text");
        assert!(loader.is_some());
        assert_eq!(
            loader.expect("just checked is_some").engine_name(),
            "text_loader"
        );
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let registry = LoaderRegistry::default();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn custom_registration_works() {
        let mut registry = LoaderRegistry::new();
        registry.register("custom", Arc::new(text::TextLoader));
        let loader = registry.get("custom");
        assert!(loader.is_some());
        assert_eq!(
            loader.expect("just checked is_some").engine_name(),
            "text_loader"
        );
    }

    #[test]
    fn register_replaces_existing() {
        let mut registry = LoaderRegistry::default();
        // Re-register "text" with the same loader — should not panic
        registry.register("text", Arc::new(text::TextLoader));
        let loader = registry.get("text");
        assert!(loader.is_some());
    }
}
