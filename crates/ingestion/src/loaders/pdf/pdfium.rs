//! PDFium-based PDF text extraction backend.
//!
//! Uses `pdfium-render` (with `thread_safe` feature) and `pdfium-auto`
//! for high-fidelity text extraction from PDF documents. The PDFium
//! shared library is auto-downloaded and cached by `pdfium-auto` on
//! first use.
//!
//! Gated behind the `pdf-pdfium` feature.
//!
//! # Why library resolution is split out of extraction
//!
//! `pdfium_auto::ensure_pdfium_library` downloads libpdfium with
//! `reqwest::blocking`, and building a blocking reqwest client constructs and
//! then drops a tokio runtime. Dropping a runtime from inside an async context
//! aborts the process with
//!
//! > Cannot drop a runtime in a context where blocking is not allowed.
//!
//! so calling it directly from `DocumentLoader::extract` made the *first* PDF
//! ingested on any machine with a cold `~/.cache/pdf2md/` a hard `exit 101` —
//! every fresh container, CI runner and new developer checkout. The download is
//! therefore driven from [`tokio::task::spawn_blocking`], where blocking (and
//! hence a runtime drop) is permitted. The fix has to live here: `pdfium-auto`
//! 0.3.1 is the latest published version and exposes no async entry point.

use std::path::{Path, PathBuf};

use tokio::sync::OnceCell;

use super::format::format_pages;
use crate::loaders::LoaderError;

/// Process-wide memo for the resolved `libpdfium` path.
///
/// `pdfium_auto::ensure_pdfium_library` keeps its own `OnceLock`, so this is
/// not about avoiding repeated filesystem work — it is about not letting N
/// concurrent first-time ingests each occupy a blocking-pool thread queued on
/// pdfium-auto's advisory extract lock. Failures are not memoized
/// (`get_or_try_init` only stores on success), so a transient network error
/// does not poison the process.
static PDFIUM_LIB: OnceCell<PathBuf> = OnceCell::const_new();

/// Resolves the `libpdfium` shared library, downloading it on first use.
///
/// Runs the (blocking, network-touching) resolution on the blocking pool. See
/// the module docs for why that is load-bearing rather than merely tidy.
pub(super) async fn ensure_library() -> Result<&'static Path, LoaderError> {
    let path = PDFIUM_LIB
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| pdfium_auto::ensure_pdfium_library(None))
                .await
                .map_err(|e| {
                    LoaderError::ExtractionFailed(format!(
                        "PDFium library resolution task failed: {e}"
                    ))
                })?
                .map_err(|e| {
                    LoaderError::ExtractionFailed(format!(
                        "Failed to obtain the PDFium library: {e}. Set PDFIUM_LIB_PATH to an \
                         existing libpdfium shared library to skip the download."
                    ))
                })
        })
        .await?;

    Ok(path.as_path())
}

/// Extract text from PDF bytes using the PDFium backend.
///
/// `lib_path` comes from [`ensure_library`]; binding to an already-resolved
/// path is pure `dlopen`, with no network and no runtime construction, so it is
/// safe to run inline on the async worker.
///
/// Returns the formatted text with page headers matching the Python
/// `pypdf_loader.py` output format. Per-page errors are logged and
/// skipped; the extraction continues with remaining pages.
pub fn extract_text(bytes: &[u8], lib_path: &Path) -> Result<String, LoaderError> {
    let pdfium = pdfium_auto::bind_pdfium_from_path(lib_path).map_err(|e| {
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
