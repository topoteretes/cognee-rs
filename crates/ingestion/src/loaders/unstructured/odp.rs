//! ODP (OpenDocument Presentation) text extraction via `zip` + `quick-xml`.
//!
//! Opens the ODP file as a ZIP archive, reads `content.xml`, and
//! extracts text from `<text:p>` elements (same parser as ODT).
//! Paragraphs are joined with `"\n\n"`.

use std::io::{Cursor, Read};

use super::super::LoaderError;

/// Extract text from an ODP file.
pub(crate) fn extract(bytes: &[u8]) -> Result<String, LoaderError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| LoaderError::ExtractionFailed(format!("Failed to open ODP as ZIP: {e}")))?;

    let mut xml_content = String::new();
    {
        let mut file = archive.by_name("content.xml").map_err(|e| {
            LoaderError::ExtractionFailed(format!("Failed to find content.xml in ODP: {e}"))
        })?;
        file.read_to_string(&mut xml_content).map_err(|e| {
            LoaderError::ExtractionFailed(format!("Failed to read content.xml: {e}"))
        })?;
    }

    // Reuse the ODF content parser from the ODT module
    super::odt::parse_odf_content(&xml_content)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn invalid_zip_returns_error() {
        let result = extract(b"not a zip file");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, LoaderError::ExtractionFailed(_)),
            "expected ExtractionFailed, got {err:?}"
        );
    }

    #[test]
    fn empty_bytes_returns_error() {
        let result = extract(b"");
        assert!(result.is_err());
    }
}
