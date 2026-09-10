#![cfg(feature = "pdf-pdfium")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression test: ingesting a PDF with a cold `pdf2md` cache must not abort.
//!
//! `pdfium_auto::ensure_pdfium_library` downloads libpdfium with
//! `reqwest::blocking`, and building a blocking reqwest client constructs and
//! drops a tokio runtime. Dropped from inside an async context that aborts with
//!
//! > Cannot drop a runtime in a context where blocking is not allowed.
//!
//! which made the first PDF ingested on any machine without a warm
//! `~/.cache/pdf2md/pdfium-<ver>/` a hard `exit 101` — every fresh container, CI
//! runner and new developer checkout. The fix drives the download from
//! `spawn_blocking`.
//!
//! # Why this test needs no reachable network
//!
//! The panic happened in `reqwest::blocking::ClientBuilder::build()`, *before*
//! any request was sent, so merely *reaching* the download is enough to trigger
//! it. The test therefore points `PDFIUM_AUTO_CACHE_DIR` at an empty temp dir
//! (guaranteeing a cache miss) and `https_proxy` at a dead loopback port, so the
//! download attempt fails immediately instead of pulling ~6 MB from GitHub.
//!
//! The assertion is deliberately "it returned at all": on a runner where the
//! proxy is bypassed the real download may succeed, and that is equally fine.
//! What must never happen is the process dying. Before the fix this test aborts
//! the whole test binary; after it, it returns `Err` (or `Ok`).
//!
//! `PDFIUM_AUTO_CACHE_DIR` is only honoured on the *first* resolution in a
//! process (`pdfium-auto` memoizes into a `OnceLock`, and so does the loader),
//! so this file deliberately contains exactly one test.

use cognee_ingestion::loaders::DocumentLoader;
use cognee_ingestion::loaders::pdf::PdfLoader;
use cognee_models::{DataPoint, Document};
use uuid::Uuid;

/// The smallest thing that is structurally a PDF. Extraction never gets this
/// far on a cold cache, and if it does the parse error is still an `Err`, not a
/// panic — which is exactly what is being pinned.
const MINIMAL_PDF: &[u8] =
    b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";

/// Multi-threaded on purpose: it is the flavour the CLI and HTTP server run,
/// and the one the original `exit 101` was reported on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_pdfium_cache_errors_instead_of_aborting() {
    let cache = tempfile::tempdir().expect("tempdir");

    // SAFETY (edition 2024): this test binary contains a single test, so no
    // other thread is reading the environment concurrently.
    unsafe {
        std::env::set_var("PDFIUM_AUTO_CACHE_DIR", cache.path());
        // Force a cache miss even if the developer has a warm real cache.
        std::env::remove_var("PDFIUM_LIB_PATH");
        // `PDFIUM_NO_AUTO_DOWNLOAD` would short-circuit *before* the blocking
        // client is built, so it would pass even against the unfixed code.
        std::env::remove_var("PDFIUM_NO_AUTO_DOWNLOAD");
        // Fail the download fast rather than fetching ~6 MB.
        std::env::set_var("https_proxy", "http://127.0.0.1:9");
        std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:9");
        std::env::remove_var("no_proxy");
        std::env::remove_var("NO_PROXY");
    }

    let doc = Document {
        base: DataPoint::new("PdfDocument", None),
        document_type: "pdf".to_string(),
        name: "cold-cache.pdf".to_string(),
        raw_data_location: "file:///cold-cache.pdf".to_string(),
        mime_type: "application/pdf".to_string(),
        extension: "pdf".to_string(),
        data_id: Uuid::new_v4(),
        external_metadata: None,
    };

    // Reaching the line after this one at all is the assertion: the unfixed
    // code aborts the process inside `extract`.
    let result = PdfLoader.extract(MINIMAL_PDF, &doc).await;

    // The proxy is a best-effort way to keep the download from succeeding; a
    // machine with NO_PROXY set, a proxy-exempt route, or a warm pdfium cache
    // can still resolve the library. In that case `MINIMAL_PDF` — deliberately
    // the smallest thing that is structurally a PDF — may fail in the PARSER
    // instead, which is a different error and must not be held to the
    // resolution hint. Assert the hint only on the error this test is about.
    if let Err(e) = result {
        let msg = e.to_string();
        // Keyed on the resolution error's own prefix, not the word "PDFium".
        // `extract_text`'s *bind* failure is "Failed to load PDFium library: …"
        // (pdfium.rs:80) — it contains "PDFium" but cannot contain the hint, so
        // a looser predicate turns a truncated download or an arch mismatch
        // into a spurious failure of a test that is about the process abort.
        let is_resolution_failure = msg.contains("Failed to obtain the PDFium library");
        if is_resolution_failure {
            assert!(
                msg.contains("PDFIUM_LIB_PATH"),
                "a failure to obtain libpdfium must tell the operator how to supply \
                 one; got: {msg}"
            );
        }
    }
}
