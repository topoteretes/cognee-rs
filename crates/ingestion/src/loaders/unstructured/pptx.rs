//! PPTX text extraction via `zip` + `quick-xml`.
//!
//! Opens the PPTX file as a ZIP archive, finds `ppt/slides/slide*.xml`
//! files, and extracts text from `<a:t>` elements. Slides are joined
//! with `"\n\n"`.

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;

use super::super::LoaderError;

const DRAWINGML_NS: &[u8] = b"http://schemas.openxmlformats.org/drawingml/2006/main";

/// Extract text from a PPTX file.
pub(crate) fn extract(bytes: &[u8]) -> Result<String, LoaderError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| LoaderError::ExtractionFailed(format!("Failed to open PPTX as ZIP: {e}")))?;

    // Collect slide file names and sort them for consistent ordering
    let mut slide_names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let file = archive.by_index(i).ok()?;
            let name = file.name().to_string();
            if name.starts_with("ppt/slides/slide") && name.ends_with(".xml") {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    // Sort by slide number (slide1.xml, slide2.xml, ...)
    slide_names.sort_by(|a, b| {
        let num_a = extract_slide_number(a);
        let num_b = extract_slide_number(b);
        num_a.cmp(&num_b)
    });

    let mut elements: Vec<String> = Vec::new();

    for slide_name in &slide_names {
        let mut xml_content = String::new();
        {
            let mut file = archive.by_name(slide_name).map_err(|e| {
                LoaderError::ExtractionFailed(format!("Failed to read {slide_name}: {e}"))
            })?;
            file.read_to_string(&mut xml_content).map_err(|e| {
                LoaderError::ExtractionFailed(format!("Failed to read {slide_name}: {e}"))
            })?;
        }

        let slide_text = parse_slide_xml(&xml_content)?;
        let trimmed = slide_text.trim().to_string();
        if !trimmed.is_empty() {
            elements.push(trimmed);
        }
    }

    Ok(elements.join("\n\n"))
}

/// Extract the slide number from a path like `ppt/slides/slide3.xml`.
fn extract_slide_number(name: &str) -> u32 {
    name.trim_start_matches("ppt/slides/slide")
        .trim_end_matches(".xml")
        .parse()
        .unwrap_or(0)
}

/// Parse a single slide's XML and extract text from `<a:t>` elements.
fn parse_slide_xml(xml: &str) -> Result<String, LoaderError> {
    let mut reader = NsReader::from_str(xml);

    let mut texts: Vec<String> = Vec::new();
    let mut in_text = false;
    let mut current_text = String::new();

    loop {
        match reader.read_resolved_event() {
            Ok((resolved, Event::Start(ref e))) => {
                let local = e.local_name();
                let is_drawingml =
                    matches!(resolved, ResolveResult::Bound(ns) if ns.as_ref() == DRAWINGML_NS);
                if local.as_ref() == b"t" && is_drawingml {
                    in_text = true;
                    current_text.clear();
                }
            }
            Ok((_, Event::End(ref e))) => {
                let local = e.local_name();
                if local.as_ref() == b"t" && in_text {
                    in_text = false;
                    let trimmed = current_text.trim().to_string();
                    if !trimmed.is_empty() {
                        texts.push(trimmed);
                    }
                }
            }
            Ok((_, Event::Text(ref e))) if in_text => {
                let raw = std::str::from_utf8(e.as_ref()).map_err(|err| {
                    LoaderError::ExtractionFailed(format!("Invalid UTF-8 in slide XML: {err}"))
                })?;
                current_text.push_str(raw);
            }
            Ok((_, Event::GeneralRef(ref e))) if in_text => {
                current_text.push_str(resolve_xml_entity(e.as_ref()));
            }
            Ok((_, Event::Eof)) => break,
            Err(e) => {
                return Err(LoaderError::ExtractionFailed(format!(
                    "XML parse error in slide: {e}"
                )));
            }
            _ => {}
        }
    }

    Ok(texts.join(" "))
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
    fn parse_simple_slide_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
       xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld>
    <p:spTree>
      <p:sp>
        <p:txBody>
          <a:p><a:r><a:t>Hello</a:t></a:r></a:p>
          <a:p><a:r><a:t>World</a:t></a:r></a:p>
        </p:txBody>
      </p:sp>
    </p:spTree>
  </p:cSld>
</p:sld>"#;

        let result = parse_slide_xml(xml).unwrap();
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn slide_number_extraction() {
        assert_eq!(extract_slide_number("ppt/slides/slide1.xml"), 1);
        assert_eq!(extract_slide_number("ppt/slides/slide10.xml"), 10);
        assert_eq!(extract_slide_number("ppt/slides/slide2.xml"), 2);
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
