//! ODT (OpenDocument Text) extraction via `zip` + `quick-xml`.
//!
//! Opens the ODT file as a ZIP archive, reads `content.xml`, and
//! extracts text from `<text:p>` elements. Paragraphs are joined
//! with `"\n\n"`.

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use super::super::LoaderError;

const ODF_TEXT_NS: &[u8] = b"urn:oasis:names:tc:opendocument:xmlns:text:1.0";

/// Extract text from an ODT file.
pub(crate) fn extract(bytes: &[u8]) -> Result<String, LoaderError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| LoaderError::ExtractionFailed(format!("Failed to open ODT as ZIP: {e}")))?;

    let mut xml_content = String::new();
    {
        let mut file = archive.by_name("content.xml").map_err(|e| {
            LoaderError::ExtractionFailed(format!("Failed to find content.xml in ODT: {e}"))
        })?;
        file.read_to_string(&mut xml_content).map_err(|e| {
            LoaderError::ExtractionFailed(format!("Failed to read content.xml: {e}"))
        })?;
    }

    parse_odf_content(&xml_content)
}

/// Parse ODF content.xml extracting text from `<text:p>` elements.
///
/// Shared between ODT and ODP extraction since both use the same
/// ODF text namespace and paragraph structure.
pub(crate) fn parse_odf_content(xml: &str) -> Result<String, LoaderError> {
    let mut reader = NsReader::from_str(xml);

    let mut paragraphs: Vec<String> = Vec::new();
    let mut current_paragraph = String::new();
    let mut in_paragraph = false;
    let mut depth: u32 = 0;

    loop {
        match reader.read_resolved_event() {
            Ok((resolved, Event::Start(ref e))) => {
                let local = e.local_name();
                let is_text_ns =
                    matches!(resolved, ResolveResult::Bound(ns) if ns.as_ref() == ODF_TEXT_NS);
                if local.as_ref() == b"p" && is_text_ns {
                    in_paragraph = true;
                    depth = 1;
                    current_paragraph.clear();
                } else if in_paragraph {
                    depth += 1;
                }
            }
            Ok((_, Event::End(ref e))) if in_paragraph => {
                let local = e.local_name();
                if local.as_ref() == b"p" && depth == 1 {
                    in_paragraph = false;
                    let trimmed = current_paragraph.trim().to_string();
                    if !trimmed.is_empty() {
                        paragraphs.push(trimmed);
                    }
                } else {
                    depth = depth.saturating_sub(1);
                }
            }
            Ok((_, Event::Text(ref e))) if in_paragraph => {
                let raw = std::str::from_utf8(e.as_ref()).map_err(|err| {
                    LoaderError::ExtractionFailed(format!("Invalid UTF-8 in ODF XML: {err}"))
                })?;
                current_paragraph.push_str(raw);
            }
            Ok((_, Event::GeneralRef(ref e))) if in_paragraph => {
                current_paragraph.push_str(resolve_xml_entity(e.as_ref()));
            }
            Ok((_, Event::Eof)) => break,
            Err(e) => {
                return Err(LoaderError::ExtractionFailed(format!(
                    "XML parse error in ODF: {e}"
                )));
            }
            _ => {}
        }
    }

    Ok(paragraphs.join("\n\n"))
}

/// Resolve standard XML entities.
fn resolve_xml_entity(name: &[u8]) -> &'static str {
    match name {
        b"amp" => "&",
        b"lt" => "<",
        b"gt" => ">",
        b"apos" => "'",
        b"quot" => "\"",
        _ => "",
    }
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
    fn parse_simple_odf_content() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p>First paragraph</text:p>
      <text:p>Second paragraph</text:p>
      <text:p>  </text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let result = parse_odf_content(xml).unwrap();
        assert_eq!(result, "First paragraph\n\nSecond paragraph");
    }

    #[test]
    fn parse_odf_with_spans() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content
    xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
    xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
  <office:body>
    <office:text>
      <text:p><text:span>Hello </text:span><text:span>world</text:span></text:p>
    </office:text>
  </office:body>
</office:document-content>"#;

        let result = parse_odf_content(xml).unwrap();
        assert_eq!(result, "Hello world");
    }

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
