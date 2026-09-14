//! PDF document loader with dual-backend support.
//!
//! Backend selection (compile-time):
//! - `pdf-pdfium` feature: high-fidelity extraction via PDFium (preferred)
//! - `pdf-pure-rust` feature: pure-Rust extraction via `pdf-extract`
//! - If both are enabled, pdfium takes priority
//! - If neither is enabled, this module is not compiled
//!
//! Both backends wrap their pages in the same `"Page N:\n{text}\n"` envelope,
//! which is what matches the Python `pypdf_loader.py:70-84` format — but the
//! page *text* they put inside it is not identical, and this module used to
//! claim it was. MEASURED over `tests/fixtures/pdf/sample.pdf` (PDFium builds
//! 7690 and 7961, `pdf-extract` as pinned):
//!
//! - PDFium separates lines within a page with `\r\n` and adds nothing else.
//! - `pdf-extract` separates them with `\n` and prefixes every page's text with
//!   two blank lines.
//!
//! So text ingested through a `pdf-pure-rust` build (the Python, TS and Java
//! bindings, `android-default`, and any container build that wants no native
//! library) does not byte-match the same document ingested through a
//! `pdf-pdfium` build. Chunk boundaries, and therefore embeddings and extracted
//! graph nodes, can differ between the two. `tests/pdf_fixture_extraction.rs`
//! and `tests/pdf_pure_rust_fixture_extraction.rs` pin each backend's output.

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
        let text = extract_impl(bytes).await?;
        Ok(LoaderOutput::Text(text))
    }

    fn engine_name(&self) -> &'static str {
        "pypdf_loader"
    }
}

/// Resolving the PDFium library is async because on a cold cache it downloads
/// libpdfium with a *blocking* HTTP client, which has to be driven from
/// `spawn_blocking` — doing it inline aborted the process. See
/// [`pdfium`] for the full story.
#[cfg(feature = "pdf-pdfium")]
async fn extract_impl(bytes: &[u8]) -> Result<String, LoaderError> {
    let lib_path = pdfium::ensure_library().await?;
    pdfium::extract_text(bytes, lib_path)
}

#[cfg(all(feature = "pdf-pure-rust", not(feature = "pdf-pdfium")))]
async fn extract_impl(bytes: &[u8]) -> Result<String, LoaderError> {
    pure_rust::extract_text(bytes)
}
