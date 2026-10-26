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
pub fn extract_text(bytes: &[u8]) -> Result<String, LoaderError> {
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
            pdf_extract::extract_text_from_mem_by_pages(&repair.bytes).map_err(|retry| {
                tracing::debug!(error = %retry, "repaired PDF still failed to parse");
                extraction_failed(original)
            })?
        }
    };

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

    #[test]
    fn a_tail_the_repair_does_not_recognise_still_fails_cleanly() {
        // Six bytes short lands inside `endobj`: not whitespace, so the repair
        // declines and the parser's own error is reported.
        let err = extract_text(&incremental_update_pdf(&[6])).expect_err("still unreadable");
        assert!(matches!(err, LoaderError::ExtractionFailed(_)), "{err}");
        assert!(err.to_string().contains("invalid file trailer"), "{err}");
    }
}
