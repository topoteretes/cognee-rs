//! Content-type sniffing and add-time loader routing, ported from Python.
//!
//! Python's `add` does not trust a file's name alone. `guess_file_type`
//! (`cognee/infrastructure/files/utils/guess_file_type.py`) reads the bytes
//! with the `filetype` package, and `LoaderEngine.get_loader`
//! (`cognee/infrastructure/loaders/LoaderEngine.py`) picks the first loader, in
//! priority order, whose `can_handle(extension, mime)` accepts either the path
//! extension or the sniffed extension together with the sniffed MIME type. The
//! sniffed values, not the path's, are what the `data` row stores as
//! `extension` and `mime_type`.
//!
//! This module is the byte-level half of that: [`guess_file_type`] and
//! [`route_loader`]. Rust's loaders are keyed by document type
//! (`"text"`, `"image"`, ...), so [`route_loader`] returns the document type
//! of the loader Python would have picked.

/// A file type as Python's `guess_file_type` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileType {
    /// MIME type, e.g. `image/png`.
    pub(crate) mime: String,
    /// Extension without the dot, e.g. `png`.
    pub(crate) extension: String,
}

impl FileType {
    fn new(mime: &str, extension: &str) -> Self {
        Self {
            mime: mime.to_string(),
            extension: extension.to_string(),
        }
    }
}

/// Guess the file type of `bytes` named with `path_ext` (the name's suffix
/// without the dot; empty when the name has none).
///
/// Port of `guess_file_type.py:60-86`. The extensions of formats that have no
/// magic number (`txt`, `csv`, `md`, `json`, `xml`, `yaml`) decide the type
/// without reading the bytes; the comparison is case-insensitive. Anything
/// else is sniffed, and bytes the sniffer does not know are plain text.
pub(crate) fn guess_file_type(bytes: &[u8], path_ext: &str) -> FileType {
    match path_ext.to_lowercase().as_str() {
        "txt" | "text" => return FileType::new("text/plain", "txt"),
        "csv" => return FileType::new("text/csv", "csv"),
        "md" | "markdown" => return FileType::new("text/markdown", "md"),
        "json" => return FileType::new("application/json", "json"),
        "xml" => return FileType::new("application/xml", "xml"),
        "yaml" | "yml" => return FileType::new("application/yaml", "yaml"),
        _ => {}
    }
    match infer::get(bytes) {
        // Python's `filetype` has no matchers for text formats (html, xml,
        // shell scripts), so they stay plain text there.
        Some(kind)
            if !matches!(
                kind.matcher_type(),
                infer::MatcherType::Text | infer::MatcherType::Custom
            ) =>
        {
            FileType::new(kind.mime_type(), kind.extension())
        }
        _ => FileType::new("text/plain", "txt"),
    }
}

/// Source-file extensions `code_loader` claims
/// (`core/code_loader.py`, `SUPPORTED_CODE_EXTENSIONS`).
const CODE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "cxx", "dart", "fs", "go", "h", "hcl", "hh", "hpp", "java", "js",
    "jsx", "kt", "kts", "php", "proto", "py", "rake", "rb", "rs", "scala", "svelte", "swift", "tf",
    "ts", "tsx", "vb", "vue",
];

const TEXT_EXTENSIONS: &[&str] = &["txt", "md", "json", "xml", "yaml", "yml", "log"];
const TEXT_MIMES: &[&str] = &[
    "text/plain",
    "text/markdown",
    "application/json",
    "text/xml",
    "application/xml",
    "text/yaml",
    "application/yaml",
];

const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "dwg", "xcf", "jpg", "jpe", "jpeg", "jpx", "apng", "gif", "webp", "cr2", "tif", "tiff",
    "bmp", "jxr", "psd", "ico", "heic", "avif",
];
const IMAGE_MIMES: &[&str] = &[
    "image/png",
    "image/vnd.dwg",
    "image/x-xcf",
    "image/jpeg",
    "image/jpx",
    "image/apng",
    "image/gif",
    "image/webp",
    "image/x-canon-cr2",
    "image/tiff",
    "image/bmp",
    "image/jxr",
    "image/vnd.adobe.photoshop",
    "image/vnd.microsoft.icon",
    "image/heic",
    "image/avif",
];

const AUDIO_EXTENSIONS: &[&str] = &[
    "aac", "mid", "mp3", "m4a", "ogg", "flac", "wav", "amr", "aiff",
];
const AUDIO_MIMES: &[&str] = &[
    "audio/aac",
    "audio/midi",
    "audio/mpeg",
    "audio/mp4",
    "audio/ogg",
    "audio/flac",
    "audio/wav",
    "audio/amr",
    "audio/aiff",
    "audio/x-wav",
];

/// The `can_handle(extension, mime_type)` of a Python core loader: the
/// extension AND the MIME type must both be in the loader's lists.
fn accepts(extensions: &[&str], mimes: &[&str], ext: &str, mime: &str) -> bool {
    extensions.contains(&ext) && mimes.contains(&mime)
}

/// Whether the loader for `doc_type` (Python priority order: code, text, pdf,
/// image, audio, csv, unstructured) accepts `ext` with the sniffed `mime`.
fn can_handle(doc_type: &str, ext: &str, mime: &str) -> bool {
    match doc_type {
        // `code_loader` matches by extension only (`code_loader.py:85-86`).
        "code" => CODE_EXTENSIONS.contains(&ext.to_lowercase().as_str()),
        "text" => accepts(TEXT_EXTENSIONS, TEXT_MIMES, ext, mime),
        "pdf" => ext == "pdf" && mime == "application/pdf",
        "image" => accepts(IMAGE_EXTENSIONS, IMAGE_MIMES, ext, mime),
        "audio" => accepts(AUDIO_EXTENSIONS, AUDIO_MIMES, ext, mime),
        "csv" => ext == "csv" && mime == "text/csv",
        // Python's office-format loaders are the optional docling/unstructured
        // ones; Rust has one extension-keyed loader and no MIME check.
        "unstructured" => {
            cognee_models::doc_type_for_extension(&ext.to_lowercase()) == Some("unstructured")
        }
        _ => false,
    }
}

/// Pick the document type whose loader Python's `LoaderEngine.get_loader`
/// would pick for a file named with `path_ext` whose sniffed type is
/// `sniffed`, or `None` when no loader claims it (Python raises "No loader
/// found").
///
/// For each loader in priority order, the path extension is tried first with
/// the sniffed MIME type, then the sniffed extension (`LoaderEngine.py:117-125`).
/// `path_ext` is compared as written: Python does not lower-case it there, so
/// `PHOTO.PNG` is found through the sniffed extension instead.
///
/// A local `.html`/`.htm` file whose bytes sniff as plain text goes to the
/// `html` loader, which extracts its text. Python's text loader would store
/// the markup instead; that is the documented difference of D14.
pub(crate) fn route_loader(path_ext: &str, sniffed: &FileType) -> Option<&'static str> {
    if matches!(path_ext.to_lowercase().as_str(), "html" | "htm") && sniffed.mime == "text/plain" {
        return Some("html");
    }
    // `code_loader` is Python's first loader; Rust has no code loader, so its
    // files are read by the text loader (same bytes, same stored text).
    const PRIORITY: [(&str, &str); 7] = [
        ("code", "text"),
        ("text", "text"),
        ("pdf", "pdf"),
        ("image", "image"),
        ("audio", "audio"),
        ("csv", "csv"),
        ("unstructured", "unstructured"),
    ];
    PRIORITY.iter().find_map(|(python_loader, doc_type)| {
        (can_handle(python_loader, path_ext, &sniffed.mime)
            || can_handle(python_loader, &sniffed.extension, &sniffed.mime))
        .then_some(*doc_type)
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R',
    ];
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F', 0];
    const PDF: &[u8] = b"%PDF-1.4\n%binary\n";

    fn route(bytes: &[u8], ext: &str) -> Option<&'static str> {
        route_loader(ext, &guess_file_type(bytes, ext))
    }

    #[test]
    fn short_circuit_extensions_are_not_sniffed() {
        // A PNG named `.txt` is plain text to the sniffer (Python: it then
        // fails in the text loader).
        let t = guess_file_type(PNG, "txt");
        assert_eq!(
            (t.mime.as_str(), t.extension.as_str()),
            ("text/plain", "txt")
        );
        let t = guess_file_type(PNG, "CSV");
        assert_eq!((t.mime.as_str(), t.extension.as_str()), ("text/csv", "csv"));
        for (ext, mime, canon) in [
            ("md", "text/markdown", "md"),
            ("markdown", "text/markdown", "md"),
            ("json", "application/json", "json"),
            ("xml", "application/xml", "xml"),
            ("yml", "application/yaml", "yaml"),
            ("text", "text/plain", "txt"),
        ] {
            let t = guess_file_type(PNG, ext);
            assert_eq!((t.mime.as_str(), t.extension.as_str()), (mime, canon));
        }
    }

    #[test]
    fn bytes_are_sniffed_outside_the_short_circuit_list() {
        let t = guess_file_type(PNG, "");
        assert_eq!(
            (t.mime.as_str(), t.extension.as_str()),
            ("image/png", "png")
        );
        let t = guess_file_type(PNG, "jpg");
        assert_eq!(
            (t.mime.as_str(), t.extension.as_str()),
            ("image/png", "png")
        );
        let t = guess_file_type(JPEG, "bin");
        assert_eq!(
            (t.mime.as_str(), t.extension.as_str()),
            ("image/jpeg", "jpg")
        );
        let t = guess_file_type(PDF, "");
        assert_eq!(
            (t.mime.as_str(), t.extension.as_str()),
            ("application/pdf", "pdf")
        );
    }

    #[test]
    fn unrecognised_bytes_are_plain_text() {
        for ext in ["conf", "", "py", "html"] {
            let t = guess_file_type(b"key = value\n", ext);
            assert_eq!(
                (t.mime.as_str(), t.extension.as_str()),
                ("text/plain", "txt"),
                "{ext}"
            );
        }
        // Markup is text for `filetype`, not `text/html`.
        let t = guess_file_type(b"<!doctype html><p>hi</p>", "");
        assert_eq!(
            (t.mime.as_str(), t.extension.as_str()),
            ("text/plain", "txt")
        );
    }

    #[test]
    fn routes_by_sniffed_type() {
        assert_eq!(route(PNG, ""), Some("image"));
        assert_eq!(route(PNG, "jpg"), Some("image"));
        assert_eq!(route(PNG, "PNG"), Some("image"));
        assert_eq!(route(JPEG, "jpeg"), Some("image"));
        assert_eq!(route(PDF, ""), Some("pdf"));
        assert_eq!(route(b"hello", "conf"), Some("text"));
        assert_eq!(route(b"hello", ""), Some("text"));
        assert_eq!(route(b"a,b\n1,2\n", "csv"), Some("csv"));
        assert_eq!(route(b"a,b\n1,2\n", "CSV"), Some("csv"));
        assert_eq!(route(b"fn main() {}", "rs"), Some("text"));
        assert_eq!(route(b"<p>hi</p>", "html"), Some("html"));
        // An office file sniffs as its own type; the loader is by extension.
        let docx = FileType::new(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "docx",
        );
        assert_eq!(route_loader("docx", &docx), Some("unstructured"));
    }

    #[test]
    fn png_named_txt_goes_to_the_text_loader() {
        // The text loader then rejects the non-UTF-8 bytes (D15).
        assert_eq!(route(PNG, "txt"), Some("text"));
    }

    #[test]
    fn a_binary_no_loader_claims_has_no_route() {
        // PK zip header named `.zip`.
        assert_eq!(route(&[b'P', b'K', 3, 4, 0, 0, 0, 0], "zip"), None);
        // PNG with a code extension: code_loader claims it by name, as Python.
        assert_eq!(route(PNG, "py"), Some("text"));
    }

    #[test]
    fn html_bytes_that_are_binary_do_not_take_the_html_route() {
        assert_eq!(route(PNG, "html"), Some("image"));
    }
}
