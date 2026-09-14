#![cfg(feature = "pdf-pure-rust")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The `pdf-pure-rust` half of [`pdf_fixture_extraction`], over the same PDF.
//!
//! This backend is default-nowhere in this workspace — only the non-default
//! `android-default` composite turns it on — so this file runs only under an
//! explicit `--features pdf-pure-rust`. Its `[[test]]` entry carries the
//! matching `required-features`, so every other lane skips it and says so
//! instead of compiling it to a green `running 0 tests`.
//!
//! It exists because `pdf-pure-rust` is what a container build wants — no
//! native library and no runtime download — and it is what the Python, TS and
//! Java bindings already ship, so it should not be the one backend that nothing
//! ever reads a real PDF with.
//!
//! # Why this file is not `not(feature = "pdf-pdfium")`
//!
//! That would have been the natural gate — `loaders::pdf` compiles the pure
//! backend only when pdfium is absent — but `required-features` cannot express
//! a negation, so under `--features pdf-pdfium,pdf-pure-rust` the target would
//! still build and yield zero tests: the precise hole this file's `[[test]]`
//! entry exists to close. So it stays compiled in both cases and asserts the
//! *selection rule* instead, which nothing else covers: with both features on,
//! `loaders::pdf` documents that pdfium takes priority, and this pins it.
//!
//! # Why the backends have separate expected files
//!
//! `loaders::pdf` used to document the two as producing identical output. They
//! do not: over `fixtures/pdf/sample.pdf`, `pdf-extract` emits two blank lines
//! at the head of every page's text and separates lines with `\n`, where PDFium
//! emits neither the blank lines nor `\n` (it uses `\r\n`). The two expected
//! files record that measured difference rather than a claim, and the
//! divergence is now described where it matters, in the `loaders::pdf` docs.

use cognee_ingestion::loaders::{DocumentLoader, LoaderOutput, pdf::PdfLoader};
use cognee_models::{DataPoint, Document};
use uuid::Uuid;

/// The same fixture the PDFium test reads. See `fixtures/pdf/README.md`.
const SAMPLE_PDF: &[u8] = include_bytes!("fixtures/pdf/sample.pdf");

const EXPECTED_PURE_RUST: &str = include_str!("fixtures/pdf/sample.expected.pure-rust.txt");
const EXPECTED_PDFIUM: &str = include_str!("fixtures/pdf/sample.expected.txt");

/// See the identically named constant in `pdf_fixture_extraction.rs`.
const LIBRARY_UNOBTAINABLE: &str = "Failed to obtain the PDFium library";

/// See the note on `pdf_fixture_extraction::normalise`: PDFium reports
/// intra-page line breaks as `\r\n`, and the fixtures are stored LF-only.
fn normalise(s: &str) -> String {
    s.replace("\r\n", "\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extracts_text_from_a_real_pdf() {
    let doc = Document {
        base: DataPoint::new("PdfDocument", None),
        document_type: "pdf".to_string(),
        name: "sample.pdf".to_string(),
        raw_data_location: "file:///sample.pdf".to_string(),
        mime_type: "application/pdf".to_string(),
        extension: "pdf".to_string(),
        data_id: Uuid::new_v4(),
        external_metadata: None,
    };

    let result = PdfLoader.extract(SAMPLE_PDF, &doc).await;

    // With both features on, the loader binds PDFium, which has to be resolved
    // at runtime and can legitimately be unobtainable. See the skip note in
    // `pdf_fixture_extraction.rs` for why this branch is kept this narrow.
    if cfg!(feature = "pdf-pdfium")
        && let Err(e) = &result
        && e.to_string().contains(LIBRARY_UNOBTAINABLE)
    {
        eprintln!(
            "SKIP extracts_text_from_a_real_pdf: both PDF backends are enabled, so the loader \
             selected PDFium, and libpdfium could not be obtained on this machine. Set \
             PDFIUM_LIB_PATH to an existing libpdfium shared library to run it. Cause: {e}"
        );
        return;
    }

    let output = result.expect("extraction of a valid PDF must succeed");
    let LoaderOutput::Text(text) = output else {
        panic!("the PDF loader must yield text, got {output:?}");
    };

    // `loaders::pdf` documents that pdfium wins when both features are on.
    // Asserting the pdfium output here is what holds it to that.
    let (expected, fixture) = if cfg!(feature = "pdf-pdfium") {
        (EXPECTED_PDFIUM, "fixtures/pdf/sample.expected.txt")
    } else {
        (
            EXPECTED_PURE_RUST,
            "fixtures/pdf/sample.expected.pure-rust.txt",
        )
    };

    assert_eq!(
        normalise(&text),
        normalise(expected),
        "extracted text does not match {fixture}"
    );
}
