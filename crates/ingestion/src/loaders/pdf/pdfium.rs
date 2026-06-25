//! PDFium-based PDF text extraction backend.
//!
//! Uses `pdfium-render` (with `thread_safe` feature) and `pdfium-auto`
//! for high-fidelity text extraction from PDF documents. The PDFium
//! shared library is auto-downloaded and cached by `pdfium-auto` on
//! first use.
//!
//! Gated behind the `pdf-pdfium` feature.

use super::format::format_pages;
use crate::loaders::LoaderError;

/// Extract text from PDF bytes using the PDFium backend.
///
/// Returns the formatted text with page headers matching the Python
/// `pypdf_loader.py` output format. Per-page errors are logged and
/// skipped; the extraction continues with remaining pages.
pub fn extract_text(bytes: &[u8]) -> Result<String, LoaderError> {
    let pdfium = pdfium_auto::bind_pdfium_silent().map_err(|e| {
        LoaderError::ExtractionFailed(format!("Failed to load PDFium library: {e}"))
    })?;

    let document = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| LoaderError::ExtractionFailed(format!("Failed to parse PDF: {e}")))?;

    let mut pages: Vec<(usize, Result<String, String>)> = Vec::new();

    for (idx, page) in document.pages().iter().enumerate() {
        let page_num = idx + 1; // 1-indexed, matching Python
        match page.text() {
            Ok(text_page) => {
                pages.push((page_num, Ok(text_page.all())));
            }
            Err(e) => {
                pages.push((page_num, Err(format!("{e}"))));
            }
        }
    }

    Ok(format_pages(&pages))
}
