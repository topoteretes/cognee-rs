//! EPUB text extraction via `zip` + `scraper` + `quick-xml`.
//!
//! EPUBs are ZIP archives containing XHTML files. This module reads
//! `META-INF/container.xml` to find the OPF file, parses the OPF
//! spine for chapter order, reads each chapter's XHTML, and strips
//! HTML tags using `scraper`. Chapters are joined with `"\n\n"`.

use std::io::{Cursor, Read};

use scraper::{Html, Selector};

use super::super::LoaderError;

/// Extract text from an EPUB file.
pub(crate) fn extract(bytes: &[u8]) -> Result<String, LoaderError> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| LoaderError::ExtractionFailed(format!("Failed to open EPUB as ZIP: {e}")))?;

    // Step 1: Read META-INF/container.xml to find the OPF file path
    let opf_path = find_opf_path(&mut archive)?;

    // Determine the base directory of the OPF file for resolving relative paths
    let opf_dir = opf_path.rfind('/').map(|i| &opf_path[..=i]).unwrap_or("");
    let opf_dir = opf_dir.to_string();

    // Step 2: Read the OPF file and parse the spine/manifest
    let opf_content = read_zip_entry(&mut archive, &opf_path)?;
    let chapter_paths = parse_opf_spine(&opf_content, &opf_dir)?;

    // Step 3: Read each chapter and strip HTML
    let mut elements: Vec<String> = Vec::new();
    for path in &chapter_paths {
        match read_zip_entry(&mut archive, path) {
            Ok(html_content) => {
                let text = strip_html(&html_content);
                let trimmed = text.trim().to_string();
                if !trimmed.is_empty() {
                    elements.push(trimmed);
                }
            }
            Err(_) => {
                // Some spine items may reference non-existent files; skip them
                continue;
            }
        }
    }

    Ok(elements.join("\n\n"))
}

/// Read a file entry from the ZIP archive as a String.
fn read_zip_entry(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<String, LoaderError> {
    let mut content = String::new();
    let mut file = archive.by_name(name).map_err(|e| {
        LoaderError::ExtractionFailed(format!("Failed to find '{name}' in EPUB: {e}"))
    })?;
    file.read_to_string(&mut content).map_err(|e| {
        LoaderError::ExtractionFailed(format!("Failed to read '{name}' in EPUB: {e}"))
    })?;
    Ok(content)
}

/// Find the OPF file path from `META-INF/container.xml`.
fn find_opf_path(archive: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Result<String, LoaderError> {
    let container_xml = read_zip_entry(archive, "META-INF/container.xml")?;

    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(&container_xml);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e))
                if e.local_name().as_ref() == b"rootfile" =>
            {
                for attr in e.attributes().flatten() {
                    if attr.key.local_name().as_ref() == b"full-path" {
                        let path = String::from_utf8(attr.value.to_vec()).map_err(|e| {
                            LoaderError::ExtractionFailed(format!("Invalid UTF-8 in OPF path: {e}"))
                        })?;
                        return Ok(path);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(LoaderError::ExtractionFailed(format!(
                    "XML parse error in container.xml: {e}"
                )));
            }
            _ => {}
        }
    }

    Err(LoaderError::ExtractionFailed(
        "No rootfile found in META-INF/container.xml".to_string(),
    ))
}

/// Parse the OPF file to extract chapter paths in spine order.
fn parse_opf_spine(opf_xml: &str, opf_dir: &str) -> Result<Vec<String>, LoaderError> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    use std::collections::HashMap;

    let mut reader = Reader::from_str(opf_xml);

    // Collect manifest items (id -> href) and spine item refs (in order)
    let mut manifest: HashMap<String, String> = HashMap::new();
    let mut spine_idrefs: Vec<String> = Vec::new();
    let mut in_manifest = false;
    let mut in_spine = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let local = e.local_name();
                match local.as_ref() {
                    b"manifest" => in_manifest = true,
                    b"spine" => in_spine = true,
                    b"item" if in_manifest => {
                        parse_manifest_item(e, &mut manifest);
                    }
                    b"itemref" if in_spine => {
                        parse_spine_itemref(e, &mut spine_idrefs);
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = e.local_name();
                if local.as_ref() == b"item" && in_manifest {
                    parse_manifest_item(e, &mut manifest);
                } else if local.as_ref() == b"itemref" && in_spine {
                    parse_spine_itemref(e, &mut spine_idrefs);
                }
            }
            Ok(Event::End(ref e)) => {
                let local = e.local_name();
                if local.as_ref() == b"manifest" {
                    in_manifest = false;
                } else if local.as_ref() == b"spine" {
                    in_spine = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(LoaderError::ExtractionFailed(format!(
                    "XML parse error in OPF: {e}"
                )));
            }
            _ => {}
        }
    }

    // Resolve spine idrefs to file paths
    let paths: Vec<String> = spine_idrefs
        .iter()
        .filter_map(|idref| {
            manifest.get(idref).map(|href| {
                if href.starts_with('/') {
                    href.clone()
                } else {
                    format!("{opf_dir}{href}")
                }
            })
        })
        .collect();

    Ok(paths)
}

/// Extract id and href from a manifest `<item>` element.
fn parse_manifest_item(
    e: &quick_xml::events::BytesStart<'_>,
    manifest: &mut std::collections::HashMap<String, String>,
) {
    let mut id = None;
    let mut href = None;
    for attr in e.attributes().flatten() {
        match attr.key.local_name().as_ref() {
            b"id" => {
                id = String::from_utf8(attr.value.to_vec()).ok();
            }
            b"href" => {
                href = String::from_utf8(attr.value.to_vec()).ok();
            }
            _ => {}
        }
    }
    if let (Some(id), Some(href)) = (id, href) {
        manifest.insert(id, href);
    }
}

/// Extract idref from a spine `<itemref>` element.
fn parse_spine_itemref(e: &quick_xml::events::BytesStart<'_>, spine_idrefs: &mut Vec<String>) {
    for attr in e.attributes().flatten() {
        if attr.key.local_name().as_ref() == b"idref"
            && let Ok(idref) = String::from_utf8(attr.value.to_vec())
        {
            spine_idrefs.push(idref);
        }
    }
}

/// Strip HTML tags from content, returning only the text.
fn strip_html(html: &str) -> String {
    let document = Html::parse_document(html);
    // Try to select <body> first, fall back to root element
    let body_selector = Selector::parse("body");

    if let Ok(selector) = body_selector {
        let mut texts: Vec<String> = Vec::new();
        for element in document.select(&selector) {
            let text: String = element
                .text()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .collect::<Vec<&str>>()
                .join(" ");
            if !text.is_empty() {
                texts.push(text);
            }
        }
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }

    // Fallback: get all text from the document root
    document
        .root_element()
        .text()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<&str>>()
        .join(" ")
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
    fn strip_html_basic() {
        let html = "<html><body><p>Hello</p><p>World</p></body></html>";
        let result = strip_html(html);
        assert!(result.contains("Hello"));
        assert!(result.contains("World"));
    }

    #[test]
    fn strip_html_with_tags() {
        let html = "<html><body><p><b>Bold</b> and <i>italic</i></p></body></html>";
        let result = strip_html(html);
        assert!(result.contains("Bold"));
        assert!(result.contains("italic"));
        assert!(!result.contains("<b>"));
        assert!(!result.contains("<i>"));
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
