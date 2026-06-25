//! PDF document loader with dual-backend support.
//!
//! Backend selection (compile-time):
//! - `pdf-pdfium` feature: high-fidelity extraction via PDFium (preferred)
//! - `pdf-pure-rust` feature: pure-Rust extraction via `pdf-extract`
//! - If both are enabled, pdfium takes priority
//! - If neither is enabled, this module is not compiled
//!
//! Both backends produce identical output matching the Python
//! `pypdf_loader.py:70-84` format.

mod format;

#[cfg(feature = "pdf-pdfium")]
mod pdfium;

#[cfg(all(feature = "pdf-pure-rust", not(feature = "pdf-pdfium")))]
mod pure_rust;

use async_trait::async_trait;
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput};

/// PDF document loader.
///
/// Extracts text page-by-page from PDF files, producing output in the
/// Python-compatible format: `"Page 1:\n{text}\n\nPage 2:\n{text}\n"`.
///
/// The extraction backend is selected at compile time based on enabled
/// features. See the module-level documentation for details.
pub struct PdfLoader;

#[async_trait]
impl DocumentLoader for PdfLoader {
    async fn extract(&self, bytes: &[u8], _doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let text = extract_impl(bytes)?;
        Ok(LoaderOutput::Text(text))
    }

    fn engine_name(&self) -> &'static str {
        "pypdf_loader"
    }
}

#[cfg(feature = "pdf-pdfium")]
fn extract_impl(bytes: &[u8]) -> Result<String, LoaderError> {
    pdfium::extract_text(bytes)
}

#[cfg(all(feature = "pdf-pure-rust", not(feature = "pdf-pdfium")))]
fn extract_impl(bytes: &[u8]) -> Result<String, LoaderError> {
    pure_rust::extract_text(bytes)
}
