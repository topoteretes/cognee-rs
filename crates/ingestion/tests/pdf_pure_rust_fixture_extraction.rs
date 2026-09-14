#![cfg(feature = "pdf-pure-rust")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The `pdf-pure-rust` half of [`pdf_fixture_extraction`], over the same PDF.
//!
//! It exists because `pdf-pure-rust` is what a container build wants — no
//! native library and no runtime download — and it is what the Python, TS and
//! Java bindings already ship, so it should not be the one backend that nothing
//! ever reads a real PDF with.
//!
//! # ⚠️ A `--workspace` run does NOT exercise the pure backend
//!
//! It would be easy to assume otherwise, so: `pdf-pure-rust` *is* enabled in a
//! workspace build. `cognee-ingestion` declares `default = []`, but the
//! `python` crate is a root workspace member and ships `pdf-pure-rust` in its
//! defaults, so unification turns it on for every `--workspace` command
//! (VERIFIED with `cargo metadata`: both PDF features resolve on).
//!
//! But `loaders::pdf` gives pdfium priority whenever both are compiled in, so
//! in that configuration this test asserts the *pdfium* output and
//! `pdf-extract` is never called. Worse, `loaders::pdf::pure_rust` is itself
//! `cfg(not(feature = "pdf-pdfium"))`, so a root `cargo check --all-targets`
//! does not even type-check it.
//!
//! Exercising this backend therefore requires a dedicated
//! `--no-default-features --features pdf-pure-rust` invocation. `check_all.sh`
//! and `ci.yml` run one; without it this file, and
//! `sample.expected.pure-rust.txt`, would be decoration.
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

/// Duplicated from `pdf_fixture_extraction.rs` — integration tests are separate
/// crates, so there is nothing to share short of a `mod common` file, which for
/// four lines would cost more than it saves. See that copy for the reasoning.
fn in_ci() -> bool {
    match std::env::var("CI") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false"),
        Err(_) => false,
    }
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
        // As in `pdf_fixture_extraction.rs`: a skip is invisible in a passing
        // test, so CI must not be allowed to go green having never pinned the
        // selection rule below.
        assert!(
            !in_ci(),
            "libpdfium could not be obtained, so the pdfium-priority rule was never checked — \
             and a CI lane must not report green on that. Provision the library and set \
             PDFIUM_LIB_PATH. Cause: {e}"
        );
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
        // The other half of the MEASURED claim in the `loaders::pdf` docs, and
        // the half `normalise` would otherwise erase: `pdf-extract` uses LF
        // where PDFium uses CRLF. Pinned here so the documented divergence
        // cannot rot in either direction.
        assert!(
            !text.contains('\r'),
            "pdf-extract is documented to emit LF within a page; if that changed, \
             update the `loaders::pdf` module docs and both expected files. Got: {text:?}"
        );
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
