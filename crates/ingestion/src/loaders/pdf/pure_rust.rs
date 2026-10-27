//! Pure-Rust PDF text extraction backend.
//!
//! Uses `pdf-extract` for text extraction without any native
//! dependencies. Works on all targets including Android, but may
//! produce lower-fidelity text for complex layouts, multi-column
//! documents, and some CJK/RTL encodings.
//!
//! Gated behind the `pdf-pure-rust` feature.

use super::format::format_pages;
use super::startxref::repair_xref_offsets;
use crate::loaders::LoaderError;

/// Extract text from PDF bytes using the pure-Rust backend.
///
/// Returns the formatted text with page headers matching the Python
/// `pypdf_loader.py` output format. Unlike the pdfium backend, this
/// backend does not provide per-page error granularity -- if the
/// document-level parse fails, the entire extraction fails.
///
/// When the parse fails, xref offsets that point at whitespace just before
/// `xref` are corrected in a copy and the parse is retried once (see
/// [`super::startxref`]). A file that parses is never rewritten, and the
/// stored bytes are untouched either way.
///
/// A parse that "succeeds" with no pages at all is an error. A PDF has at
/// least one page, and `pdf-extract` stops at the first page number it cannot
/// find, so zero pages means the page tree, or page 1, was unreadable. One
/// known cause is an inline `/Encrypt` dictionary, which `lopdf` does not
/// decrypt (see [`no_pages_error`]).
///
/// A document with pages but no text (scanned or image-only, encrypted or
/// not) still returns `""` with a warning, as PDFium and Python's pypdf do.
pub fn extract_text(bytes: &[u8]) -> Result<String, LoaderError> {
    // The bytes that were actually parsed: the input, or its repaired copy.
    let mut repaired = None;
    let page_texts = match pdf_extract::extract_text_from_mem_by_pages(bytes) {
        Ok(pages) => pages,
        Err(original) => {
            let Some(repair) = repair_xref_offsets(bytes) else {
                return Err(extraction_failed(original));
            };
            let corrections: Vec<String> = repair
                .corrections
                .iter()
                .map(|c| format!("{:?} {} -> {}", c.field, c.recorded, c.corrected))
                .collect();
            tracing::warn!(
                backend = "pdf-pure-rust",
                corrections = ?corrections,
                error = %original,
                "PDF xref offsets point at whitespace before the xref keyword; \
                 retrying with them moved onto the keyword"
            );
            // On a second failure the file's own error is the one to report:
            // the repaired copy is an internal attempt, not the document.
            let pages =
                pdf_extract::extract_text_from_mem_by_pages(&repair.bytes).map_err(|retry| {
                    tracing::debug!(error = %retry, "repaired PDF still failed to parse");
                    extraction_failed(original)
                })?;
            repaired = Some(repair.bytes);
            pages
        }
    };
    let parsed = repaired.as_deref().unwrap_or(bytes);

    if page_texts.is_empty() {
        return Err(no_pages_error(parsed));
    }

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

/// The error for a parse that produced no pages, naming the cause when it is
/// encryption. Parses `bytes` again, so it is only reached on that path, never
/// on a successful extraction.
fn no_pages_error(bytes: &[u8]) -> LoaderError {
    const UNREADABLE: &str = "PDF parsed but no pages could be read";
    let doc = pdf_extract::Document::load_mem(bytes).ok();
    let message = match &doc {
        // `lopdf` removes `/Encrypt` from the trailer once it has decrypted,
        // so one still there is an encryption it did not act on.
        Some(doc) if doc.trailer.get(b"Encrypt").is_ok() => format!(
            "{UNREADABLE}: the document is encrypted in a form the pdf-pure-rust backend \
             cannot decrypt (for example an inline /Encrypt dictionary). The pdf-pdfium \
             backend reads such files."
        ),
        Some(doc) if doc.was_encrypted() => format!(
            "{UNREADABLE}: the document was decrypted, but its page tree or first page is \
             unreadable."
        ),
        _ => format!("{UNREADABLE} (its page tree or first page is unreadable)."),
    };
    LoaderError::ExtractionFailed(message)
}

fn extraction_failed(e: pdf_extract::OutputError) -> LoaderError {
    LoaderError::ExtractionFailed(format!("Failed to extract PDF text: {e}"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    /// A one-page PDF plus one incremental update per entry of `shorts`, each
    /// replacing the page's content stream, laid out like the SDK-1193 data
    /// sheets: classic xref tables, `/Prev`, and a blank line between the last
    /// `endobj` and each update's `xref`. Update `i` records its `xref` offset
    /// `shorts[i]` bytes short, in its `startxref` and in the next update's
    /// `/Prev`, as a writer that reuses its recorded value does.
    fn incremental_update_pdf(shorts: &[usize]) -> Vec<u8> {
        fn object(pdf: &mut Vec<u8>, offsets: &mut Vec<(u32, usize)>, id: u32, body: &str) {
            offsets.push((id, pdf.len()));
            pdf.extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        fn stream(text: &str) -> String {
            let content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            )
        }
        fn page(contents: u32) -> String {
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 4 0 R >> >> /Contents {contents} 0 R >>"
            )
        }
        fn entry(offset: usize) -> String {
            format!("{offset:010} 00000 n \n")
        }

        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        object(
            &mut pdf,
            &mut offsets,
            1,
            "<< /Type /Catalog /Pages 2 0 R >>",
        );
        object(
            &mut pdf,
            &mut offsets,
            2,
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        );
        object(&mut pdf, &mut offsets, 3, &page(5));
        object(
            &mut pdf,
            &mut offsets,
            4,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        );
        object(&mut pdf, &mut offsets, 5, &stream("Original revision"));
        let first_xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for (_, offset) in &offsets {
            pdf.extend_from_slice(entry(*offset).as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{first_xref}\n%%EOF\n")
                .as_bytes(),
        );

        let mut prev = first_xref;
        for (i, &short) in shorts.iter().enumerate() {
            let contents = 6 + i as u32;
            let mut update = Vec::new();
            let text = format!("Revision {} SDK-1193", i + 1);
            object(&mut pdf, &mut update, contents, &stream(&text));
            object(&mut pdf, &mut update, 3, &page(contents));
            pdf.push(b'\n');
            let xref = pdf.len();
            pdf.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
            update.sort();
            for (id, offset) in &update {
                pdf.extend_from_slice(format!("{id} 1\n{}", entry(*offset)).as_bytes());
            }
            let recorded = xref - short;
            let size = contents + 1;
            pdf.extend_from_slice(
                format!(
                    "trailer\n<< /Size {size} /Root 1 0 R /Prev {prev} >>\n\
                     startxref\n{recorded}\n%%EOF\n"
                )
                .as_bytes(),
            );
            prev = recorded;
        }
        pdf
    }

    #[test]
    fn the_fixtures_reproduce_the_unpatched_failure() {
        // Guards the tests below: if `pdf-extract` starts accepting these files
        // on its own, they would pass without exercising the repair.
        for shorts in [&[1][..], &[1, 1], &[1, 0]] {
            let err = pdf_extract::extract_text_from_mem_by_pages(&incremental_update_pdf(shorts))
                .expect_err("pdf-extract 0.12 rejects an off-by-one xref offset");
            assert!(
                err.to_string().contains("invalid file trailer"),
                "{shorts:?}: {err}"
            );
        }
    }

    #[test]
    fn reads_an_incremental_update_whose_final_startxref_is_one_byte_short() {
        let text = extract_text(&incremental_update_pdf(&[1])).expect("repaired file parses");
        assert!(text.contains("Revision 1 SDK-1193"), "{text:?}");
        assert!(!text.contains("Original revision"), "{text:?}");
        assert_eq!(text, extract_text(&incremental_update_pdf(&[0])).unwrap());
    }

    #[test]
    fn reads_a_file_updated_twice_with_short_offsets() {
        // The second update's `/Prev` carries the first one's short offset.
        let text = extract_text(&incremental_update_pdf(&[1, 1])).expect("repaired file parses");
        assert!(text.contains("Revision 2 SDK-1193"), "{text:?}");
        assert!(!text.contains("Revision 1 SDK-1193"), "{text:?}");
        assert_eq!(
            text,
            extract_text(&incremental_update_pdf(&[0, 0])).unwrap()
        );

        let text = extract_text(&incremental_update_pdf(&[1, 0])).expect("short /Prev only");
        assert!(text.contains("Revision 2 SDK-1193"), "{text:?}");
    }

    #[test]
    fn well_formed_incremental_updates_extract_as_before() {
        for shorts in [&[0][..], &[0, 0]] {
            let pdf = incremental_update_pdf(shorts);
            let direct = pdf_extract::extract_text_from_mem_by_pages(&pdf).unwrap();
            let pages: Vec<_> = direct
                .into_iter()
                .enumerate()
                .map(|(i, t)| (i + 1, Ok(t)))
                .collect();
            assert_eq!(extract_text(&pdf).unwrap(), format_pages(&pages));
        }
    }

    /// A classic-xref PDF whose objects are `objects`, numbered from 1, with
    /// object 1 as the catalog.
    fn pdf_of(objects: &[&str]) -> Vec<u8> {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    /// One page with an empty content stream: what a scanned page looks like
    /// to a text extractor, minus the image.
    fn blank_page_pdf() -> Vec<u8> {
        pdf_of(&[
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            "<< /Length 0 >>\nstream\n\nendstream",
        ])
    }

    /// `sample.pdf` encrypted by `mutool clean -E rc4-128` and left as mutool
    /// wrote it, with `/Encrypt` inline in the trailer. `lopdf` 0.42 only
    /// decrypts an indirect `/Encrypt`; see `tests/fixtures/pdf/README.md`.
    const INLINE_ENCRYPT_PDF: &[u8] =
        include_bytes!("../../../tests/fixtures/pdf/sample.encrypted-inline-rc4-128.pdf");

    /// No pages at all: an empty page tree.
    fn no_pages_pdf() -> Vec<u8> {
        pdf_of(&[
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [] /Count 0 >>",
        ])
    }

    /// [`ENCRYPTED_BLANK_PDF`] with its (cleartext) page tree emptied in place:
    /// `/Count 1/Kids[4 0 R]` becomes the same-length `/Count 0/Kids[     ]`,
    /// so every xref offset stays valid. It decrypts, then has no pages.
    fn encrypted_no_pages_pdf() -> Vec<u8> {
        const FROM: &[u8] = b"/Count 1/Kids[4 0 R]";
        const TO: &[u8] = b"/Count 0/Kids[     ]";
        let mut pdf = ENCRYPTED_BLANK_PDF.to_vec();
        let at = pdf
            .windows(FROM.len())
            .position(|w| w == FROM)
            .expect("the fixture's page tree is written as `/Count 1/Kids[4 0 R]`");
        pdf[at..at + FROM.len()].copy_from_slice(TO);
        pdf
    }

    /// A blank one-page PDF encrypted with an empty user password, with an
    /// indirect `/Encrypt` that `lopdf` does decrypt.
    const ENCRYPTED_BLANK_PDF: &[u8] =
        include_bytes!("../../../tests/fixtures/pdf/blank.encrypted-rc4-128.pdf");

    #[test]
    fn the_empty_extraction_fixtures_reproduce_what_pdf_extract_does() {
        // Guards the tests below: each input must reach `extract_text` as an
        // Ok with no text, either zero pages (which this module turns into an
        // error) or one empty page (which it keeps). If `pdf-extract` starts
        // failing (or reading) these on its own, the tests below would pass
        // without exercising anything.
        let pages = pdf_extract::extract_text_from_mem_by_pages(INLINE_ENCRYPT_PDF).unwrap();
        assert!(pages.is_empty(), "{pages:?}");

        for pdf in [no_pages_pdf(), encrypted_no_pages_pdf()] {
            let pages = pdf_extract::extract_text_from_mem_by_pages(&pdf).unwrap();
            assert!(pages.is_empty(), "{pages:?}");
        }

        for pdf in [ENCRYPTED_BLANK_PDF, &blank_page_pdf()] {
            let pages = pdf_extract::extract_text_from_mem_by_pages(pdf).unwrap();
            assert_eq!(pages.len(), 1);
            assert!(pages[0].trim().is_empty(), "{pages:?}");
        }
    }

    #[test]
    fn a_pdf_with_no_pages_is_an_error() {
        let err = extract_text(&no_pages_pdf()).expect_err("zero pages is never valid");
        assert!(matches!(err, LoaderError::ExtractionFailed(_)), "{err}");
        let msg = err.to_string();
        assert!(
            msg.contains("page tree or first page is unreadable"),
            "{msg}"
        );
        assert!(!msg.contains("encrypt"), "{msg}");
    }

    #[test]
    fn an_inline_encrypt_dictionary_is_an_error_not_an_empty_document() {
        let err = extract_text(INLINE_ENCRYPT_PDF).expect_err("unreadable, so it must fail");
        assert!(matches!(err, LoaderError::ExtractionFailed(_)), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("cannot decrypt"), "{msg}");
        assert!(msg.contains("pdf-pdfium"), "{msg}");
    }

    #[test]
    fn a_decrypted_pdf_with_no_pages_is_an_error_that_does_not_blame_decryption() {
        let err = extract_text(&encrypted_no_pages_pdf()).expect_err("zero pages");
        assert!(matches!(err, LoaderError::ExtractionFailed(_)), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("was decrypted"), "{msg}");
        assert!(!msg.contains("cannot decrypt"), "{msg}");
    }

    #[test]
    fn an_encrypted_pdf_with_blank_pages_still_extracts_to_empty_text() {
        // As PDFium and pypdf do: encryption alone is no reason to reject a
        // document whose pages decrypt fine but carry no text.
        assert_eq!(extract_text(ENCRYPTED_BLANK_PDF).unwrap(), "");
    }

    #[test]
    fn an_unencrypted_pdf_with_blank_pages_still_extracts_to_empty_text() {
        // Image-only and scanned documents land here. Failing them would
        // reject files Python's pypdf loader accepts; they keep the warning.
        assert_eq!(extract_text(&blank_page_pdf()).unwrap(), "");
    }

    #[test]
    fn a_tail_the_repair_does_not_recognise_still_fails_cleanly() {
        // Six bytes short lands inside `endobj`: not whitespace, so the repair
        // declines and the parser's own error is reported.
        let err = extract_text(&incremental_update_pdf(&[6])).expect_err("still unreadable");
        assert!(matches!(err, LoaderError::ExtractionFailed(_)), "{err}");
        assert!(err.to_string().contains("invalid file trailer"), "{err}");
    }
}
