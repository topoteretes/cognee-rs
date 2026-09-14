#![cfg(feature = "pdf-pdfium")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Drives the PDF loader over a *real* PDF and pins the extracted text.
//!
//! Until this file, no test anywhere in the repository fed real PDF bytes to a
//! real extractor. Every PDF in the suite was a synthesized `%PDF-1.x` header:
//! two in `pipeline.rs` that only ever reach MIME classification, and
//! `pdf_pdfium_cold_cache.rs`'s, which is deliberately barely-a-PDF because
//! that test is about the process aborting *before* extraction is reached.
//!
//! So `PdfLoader::extract` returning correct text had no coverage at all, and
//! neither did the page-header format that matches Python's `pypdf_loader.py` —
//! `format_pages` was tested only against hand-written `Vec`s, never against
//! what a PDF engine actually hands it.
//!
//! # Why this is gated on `pdf-pdfium`
//!
//! `pdf-pdfium` is in `crates/lib`'s default feature list, so a workspace test
//! run unifies it on and this file executes — and because `loaders::pdf` gives
//! pdfium priority when both backends are compiled in, pdfium is what a
//! workspace lane actually exercises.
//!
//! Note that `pdf-pure-rust` is *also* on in a workspace build, contrary to
//! what one might assume from `cognee-ingestion`'s own `default = []`: the
//! `python` crate is a root workspace member (`Cargo.toml`) and ships
//! `pdf-pure-rust` in its defaults, so feature unification turns it on for
//! every `--workspace` command. VERIFIED with `cargo metadata`. That is why
//! `pdf_pure_rust_fixture_extraction.rs` needs a lane of its own to be worth
//! anything — see its module docs.
//!
//! # When this test skips — and when it must not
//!
//! The pdfium backend needs `libpdfium`, which `pdfium-auto` downloads on first
//! use. On a developer machine that cannot obtain it — no network, a blocked
//! proxy, an unsupported target triple — this test returns instead of failing.
//!
//! **In CI it fails instead.** A skip here is not visible: cargo captures the
//! output of a *passing* test, so a bare `test ... ok` is all a normal
//! `cargo test` prints, and a lane could go green having never parsed a PDF —
//! which is precisely the green-zero failure this file and its `[[test]]`
//! entry exist to close. Keying the hard failure on `CI` (set by GitHub
//! Actions) keeps the local convenience without letting the guarantee evaporate
//! where it is load-bearing. The repo's own runner passes `--no-capture`
//! (`scripts/run_tests_with_openai.sh`), so the notice is legible there too.
//!
//! If a CI environment ever genuinely cannot download it, provision the library
//! and point `PDFIUM_LIB_PATH` at it rather than relaxing this.
//!
//! The escape hatch is otherwise deliberately narrow: it fires *only* on the
//! library-resolution error, and it loses no coverage, because that path is
//! itself what `pdf_pdfium_cold_cache.rs` pins. A parse failure, a bind
//! failure, or the wrong text still fail the test.

use cognee_ingestion::loaders::{DocumentLoader, LoaderOutput, pdf::PdfLoader};
use cognee_models::{DataPoint, Document};
use uuid::Uuid;

/// A two-page, uncompressed, base-14-font PDF authored in this repository.
/// See `fixtures/pdf/README.md` for provenance and how to regenerate it.
const SAMPLE_PDF: &[u8] = include_bytes!("fixtures/pdf/sample.pdf");

/// The loader's expected output, CRLF-normalised — see [`normalise`].
const EXPECTED: &str = include_str!("fixtures/pdf/sample.expected.txt");

/// The *resolution* error's own prefix (`loaders::pdf::pdfium::ensure_library`),
/// and deliberately not the word "PDFium": `extract_text`'s bind failure reads
/// "Failed to load PDFium library: …", and that one is a genuine failure that
/// must not be swallowed as an environment skip.
const LIBRARY_UNOBTAINABLE: &str = "Failed to obtain the PDFium library";

/// Whether this is an automated run that must not skip. GitHub Actions sets
/// `CI=true`; some local tooling exports `CI=false` or an empty `CI`, and
/// treating merely-present as true would hard-fail those developers for no
/// reason, so the value is read rather than just its existence.
fn in_ci() -> bool {
    match std::env::var("CI") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false"),
        Err(_) => false,
    }
}

/// PDFium reports intra-page line breaks as `\r\n`, while the `Page N:`
/// separators come from `format_pages` as `\n`. Committing that mixture as a
/// text fixture invites an editor or a `.gitattributes` rule to rewrite it, so
/// the fixture is stored LF-only and both sides are normalised before
/// comparison. Verified byte-identical under PDFium builds 7690 and 7961.
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

    let output = match PdfLoader.extract(SAMPLE_PDF, &doc).await {
        Ok(output) => output,
        Err(e) if e.to_string().contains(LIBRARY_UNOBTAINABLE) => {
            assert!(
                !in_ci(),
                "libpdfium could not be obtained, so real-PDF extraction never ran — and a CI \
                 lane must not report green on that. Provision the library and set \
                 PDFIUM_LIB_PATH. Cause: {e}"
            );
            eprintln!(
                "SKIP extracts_text_from_a_real_pdf: libpdfium could not be obtained on this \
                 machine, so real-PDF extraction was NOT verified. Set PDFIUM_LIB_PATH to an \
                 existing libpdfium shared library to run it. Cause: {e}"
            );
            return;
        }
        Err(e) => panic!("extraction of a valid PDF failed: {e}"),
    };

    let LoaderOutput::Text(text) = output else {
        panic!("the PDF loader must yield text, got {output:?}");
    };

    // `loaders::pdf` documents, as MEASURED, that PDFium separates intra-page
    // lines with CRLF. `normalise` below deliberately erases exactly that, so
    // without this assertion the divergence the module docs describe — and
    // that `sample.expected.pure-rust.txt` exists to contrast with — would be
    // unpinned, and a pdfium upgrade that switched to LF would slip through
    // green while the docs went stale.
    assert!(
        text.contains("\r\n"),
        "PDFium is documented to emit CRLF within a page; if that changed, \
         update the `loaders::pdf` module docs and both expected files. Got: {text:?}"
    );

    assert_eq!(
        normalise(&text),
        normalise(EXPECTED),
        "extracted text does not match fixtures/pdf/sample.expected.txt"
    );
}
