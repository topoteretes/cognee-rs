//! DOCX text extraction via `zip` + `quick-xml`.
//!
//! Opens the DOCX file as a ZIP archive, reads `word/document.xml`,
//! and extracts text from `<w:t>` elements within `<w:p>` paragraphs.
//! Paragraphs are joined with `"\n\n"`.

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use super::super::LoaderError;

const WORD_NS: &[u8] = b"http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Extract text from a DOCX file.
pub(crate) fn extract(bytes: &[u8]) -> Result<String, LoaderError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| LoaderError::ExtractionFailed(format!("Failed to open DOCX as ZIP: {e}")))?;

    let mut xml_content = String::new();
    {
        let mut file = archive.by_name("word/document.xml").map_err(|e| {
            LoaderError::ExtractionFailed(format!("Failed to find word/document.xml in DOCX: {e}"))
        })?;
        file.read_to_string(&mut xml_content).map_err(|e| {
            LoaderError::ExtractionFailed(format!("Failed to read word/document.xml: {e}"))
        })?;
    }

    parse_docx_xml(&xml_content)
}

fn parse_docx_xml(xml: &str) -> Result<String, LoaderError> {
    let mut reader = NsReader::from_str(xml);

    let mut paragraphs: Vec<String> = Vec::new();
    let mut current_paragraph = String::new();
    let mut in_paragraph = false;
    let mut in_text = false;

    loop {
        match reader.read_resolved_event() {
            Ok((resolved, Event::Start(ref e))) => {
                let local = e.local_name();
                let is_word =
                    matches!(resolved, ResolveResult::Bound(ns) if ns.as_ref() == WORD_NS);
                if local.as_ref() == b"p" && is_word {
                    in_paragraph = true;
                    current_paragraph.clear();
                } else if local.as_ref() == b"t" && is_word && in_paragraph {
                    in_text = true;
                }
            }
            Ok((_, Event::End(ref e))) => {
                let local = e.local_name();
                if local.as_ref() == b"t" {
                    in_text = false;
                } else if local.as_ref() == b"p" && in_paragraph {
                    in_paragraph = false;
                    let trimmed = current_paragraph.trim().to_string();
                    if !trimmed.is_empty() {
                        paragraphs.push(trimmed);
                    }
                }
            }
            Ok((_, Event::Text(ref e))) if in_text => {
                let raw = std::str::from_utf8(e.as_ref()).map_err(|err| {
                    LoaderError::ExtractionFailed(format!("Invalid UTF-8 in DOCX XML: {err}"))
                })?;
                current_paragraph.push_str(raw);
            }
            Ok((_, Event::GeneralRef(ref e))) if in_text => {
                current_paragraph.push_str(resolve_xml_entity(e.as_ref()));
            }
            Ok((_, Event::Eof)) => break,
            Err(e) => {
                return Err(LoaderError::ExtractionFailed(format!(
                    "XML parse error in DOCX: {e}"
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
mod tests {
    use super::*;

    #[test]
    fn parse_simple_docx_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>Hello world</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>Second paragraph</w:t></w:r>
    </w:p>
    <w:p>
      <w:r><w:t>  </w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let result = parse_docx_xml(xml).unwrap();
        assert_eq!(result, "Hello world\n\nSecond paragraph");
    }

    #[test]
    fn parse_multiple_runs_in_paragraph() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>Hello </w:t></w:r>
      <w:r><w:t>world</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let result = parse_docx_xml(xml).unwrap();
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn parse_entities_in_text() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p>
      <w:r><w:t>A &amp; B</w:t></w:r>
    </w:p>
  </w:body>
</w:document>"#;

        let result = parse_docx_xml(xml).unwrap();
        assert_eq!(result, "A & B");
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
