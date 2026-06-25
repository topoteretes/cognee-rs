//! Pure-Rust PDF text extraction backend.
//!
//! Uses `pdf-extract` for text extraction without any native
//! dependencies. Works on all targets including Android, but may
//! produce lower-fidelity text for complex layouts, multi-column
//! documents, and some CJK/RTL encodings.
//!
//! Gated behind the `pdf-pure-rust` feature.

use super::format::format_pages;
use crate::loaders::LoaderError;

/// Extract text from PDF bytes using the pure-Rust backend.
///
/// Returns the formatted text with page headers matching the Python
/// `pypdf_loader.py` output format. Unlike the pdfium backend, this
/// backend does not provide per-page error granularity -- if the
/// document-level parse fails, the entire extraction fails.
pub fn extract_text(bytes: &[u8]) -> Result<String, LoaderError> {
    let page_texts = pdf_extract::extract_text_from_mem_by_pages(bytes)
        .map_err(|e| LoaderError::ExtractionFailed(format!("Failed to extract PDF text: {e}")))?;

    let pages: Vec<(usize, Result<String, String>)> = page_texts
        .into_iter()
        .enumerate()
        .map(|(idx, text)| (idx + 1, Ok(text)))
        .collect();

    Ok(format_pages(&pages))
}
