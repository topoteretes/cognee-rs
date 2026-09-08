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

    let text = format_pages(&pages);
    // `format_pages` returns "" when every page extracted empty, and nothing
    // downstream treats that as a failure: ingest stores a zero-byte document
    // and reports success, the empty text embeds to a zero-norm vector, and
    // cosine KNN drops that row (see `cognee_vector::zero_norm`). The whole
    // pipeline then completes green with nothing retrievable. That is worse
    // than an error, so at minimum it must not be silent.
    if text.trim().is_empty() {
        tracing::warn!(
            backend = "pdf-pure-rust",
            page_count = pages.len(),
            "PDF extraction produced no text — this backend returns empty for some \
             real PDFs (Type3 or otherwise undecodable font encodings) as well as for \
             scanned/image-only documents. The document will be stored empty and will \
             not be retrievable by search. Build with the `pdf-pdfium` feature to read \
             these files."
        );
    }
    Ok(text)
}
