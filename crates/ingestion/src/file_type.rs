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
    sniff(bytes).unwrap_or_else(|| FileType::new("text/plain", "txt"))
}

/// How many leading bytes `filetype` reads (`filetype/utils.py`,
/// `_NUM_SIGNATURE_BYTES`).
const SIGNATURE_BYTES: usize = 8192;

/// `filetype.guess` (1.2.0, the version Python pins): the type of the first
/// [`SIGNATURE_BYTES`] bytes as `filetype` names it, or `None`.
///
/// `infer` does the matching, but the two packages do not have the same
/// table. `filetype` has image matchers `infer` lacks (`dwg`, `xcf`, `jpx`,
/// `apng`) or draws differently (`heic`), checked here first in `filetype`'s
/// own order, before any other image; `infer`'s answer is then renamed to
/// `filetype`'s MIME type and extension (`m4a` is `audio/mp4`, `ico` is
/// `image/x-icon`, ...), and a type `filetype` does not know is `None`.
fn sniff(bytes: &[u8]) -> Option<FileType> {
    let buf = &bytes[..bytes.len().min(SIGNATURE_BYTES)];
    if buf.starts_with(b"AC10") {
        return Some(FileType::new("image/vnd.dwg", "dwg"));
    }
    if buf.starts_with(b"gimp xcf ") {
        return Some(FileType::new("image/x-xcf", "xcf"));
    }
    if buf.len() > 50 && buf.starts_with(&[0, 0, 0, 0x0C]) && &buf[16..24] == b"ftypjp2 " {
        return Some(FileType::new("image/jpx", "jpx"));
    }
    if is_apng(buf) {
        return Some(FileType::new("image/apng", "apng"));
    }
    if is_heic(buf) {
        return Some(FileType::new("image/heic", "heic"));
    }
    let kind = infer::get(buf)?;
    let (mime, extension) = match kind.extension() {
        // Same name in both packages.
        ext @ ("jpg" | "png" | "gif" | "webp" | "cr2" | "tif" | "bmp" | "jxr" | "psd" | "avif"
        | "mp4" | "m4v" | "mkv" | "webm" | "mov" | "avi" | "wmv" | "mpg" | "flv"
        | "midi" | "mp3" | "ogg" | "flac" | "wav" | "amr" | "aac" | "aiff" | "woff"
        | "woff2" | "ttf" | "otf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx"
        | "odt" | "ods" | "odp" | "epub" | "zip" | "tar" | "gz" | "bz2" | "7z" | "xz"
        | "pdf" | "swf" | "rtf" | "eot" | "ps" | "nes" | "crx" | "cab" | "ar" | "Z"
        | "lz" | "rpm" | "dcm" | "zst" | "lz4" | "wasm" | "elf") => (kind.mime_type(), ext),
        // Renamed: `infer`'s name -> `filetype`'s.
        "m4a" => ("audio/mp4", "m4a"),
        "opus" => ("audio/ogg", "ogg"),
        "ico" => ("image/x-icon", "ico"),
        "rar" => ("application/x-rar-compressed", "rar"),
        "sqlite" => ("application/x-sqlite3", "sqlite"),
        "deb" => ("application/x-deb", "deb"),
        "exe" | "dll" => ("application/x-msdownload", "exe"),
        // `filetype` has no matcher (text formats such as html, xml and shell
        // scripts among them), so the bytes are plain text there.
        _ => return None,
    };
    Some(FileType::new(mime, extension))
}

/// `filetype`'s `Apng.match`: a PNG whose `acTL` chunk comes before any
/// `IDAT`/`IEND`.
fn is_apng(buf: &[u8]) -> bool {
    if buf.len() <= 8 || !buf.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return false;
    }
    let mut i = 8;
    while buf.len() > i {
        let Some(len_bytes) = buf.get(i..i + 4) else {
            return false;
        };
        let data_length =
            u32::from_be_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]);
        i += 4;
        match buf.get(i..i + 4) {
            Some(b"IDAT" | b"IEND") | None => return false,
            Some(b"acTL") => return true,
            Some(_) => {}
        }
        i = i.saturating_add(8).saturating_add(data_length as usize);
    }
    false
}

/// `filetype`'s `Heic.match`: an ISO-BMFF `ftyp` box whose major brand is
/// `heic`, or `mif1`/`msf1` with a compatible `heic` brand.
fn is_heic(buf: &[u8]) -> bool {
    if buf.len() < 16 || &buf[4..8] != b"ftyp" {
        return false;
    }
    let ftyp_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < ftyp_len {
        return false;
    }
    let major = &buf[8..12];
    if major == b"heic" {
        return true;
    }
    (major == b"mif1" || major == b"msf1")
        && (16..ftyp_len)
            .step_by(4)
            .any(|i| buf.get(i..i + 4) == Some(b"heic".as_slice()))
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

    fn ft(bytes: &[u8]) -> (String, String) {
        let t = guess_file_type(bytes, "");
        (t.mime, t.extension)
    }

    fn pair(mime: &str, ext: &str) -> (String, String) {
        (mime.to_string(), ext.to_string())
    }

    /// `infer` names some types differently from Python's `filetype` 1.2.0;
    /// the row must carry `filetype`'s names, and routing uses them.
    #[test]
    fn infer_names_are_translated_to_filetype_names() {
        let mut m4a = b"\0\0\0\x20ftypM4A \0\0\0\0M4A mp42isom".to_vec();
        m4a.resize(64, 0);
        assert_eq!(ft(&m4a), pair("audio/mp4", "m4a"));
        // So an `.m4a` reaches the audio loader, as in Python.
        assert_eq!(route(&m4a, "m4a"), Some("audio"));
        assert_eq!(route(&m4a, ""), Some("audio"));

        let ico = [0u8, 0, 1, 0, 1, 0, 16, 16, 0, 0];
        assert_eq!(ft(&ico), pair("image/x-icon", "ico"));
        // `image/x-icon` is not in image_loader's MIME list: Python finds no
        // loader for an icon.
        assert_eq!(route(&ico, "ico"), None);

        let mut opus = b"OggS\0\x02".to_vec();
        opus.resize(28, 0);
        opus.extend_from_slice(b"OpusHead");
        opus.resize(64, 0);
        assert_eq!(ft(&opus), pair("audio/ogg", "ogg"));

        assert_eq!(
            ft(b"Rar!\x1a\x07\x00rest"),
            pair("application/x-rar-compressed", "rar")
        );
        assert_eq!(
            ft(b"MZ\x90\x00rest-of-exe"),
            pair("application/x-msdownload", "exe")
        );
        // A type `filetype` has no matcher for is plain text there.
        assert_eq!(ft(&[0xFF, 0x0A, 0x00, 0x00]), pair("text/plain", "txt"));
    }

    /// Image types `filetype` matches and `infer` does not (or differently)
    /// route to the image loader, as in Python.
    #[test]
    fn filetype_only_image_matchers() {
        let dwg = b"AC1018\0\0\0\0\0rest";
        assert_eq!(ft(dwg), pair("image/vnd.dwg", "dwg"));
        assert_eq!(route(dwg, ""), Some("image"));

        let xcf = b"gimp xcf v011\0rest";
        assert_eq!(ft(xcf), pair("image/x-xcf", "xcf"));

        let mut jpx = vec![
            0u8, 0, 0, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A,
        ];
        jpx.extend_from_slice(&[0, 0, 0, 0x14]);
        jpx.extend_from_slice(b"ftypjp2 ");
        jpx.resize(64, 0);
        assert_eq!(ft(&jpx), pair("image/jpx", "jpx"));
        assert_eq!(route(&jpx, "jp2"), Some("image"));

        let mut apng = PNG.to_vec(); // signature + IHDR length/type
        apng.extend_from_slice(&[0; 13 + 4]); // IHDR data + CRC
        apng.extend_from_slice(&[0, 0, 0, 8]);
        apng.extend_from_slice(b"acTL");
        apng.resize(80, 0);
        assert_eq!(ft(&apng), pair("image/apng", "apng"));
        // A plain PNG (IDAT before any acTL) stays `png`.
        let mut png = PNG.to_vec();
        png.extend_from_slice(&[0; 13 + 4]);
        png.extend_from_slice(&[0, 0, 0, 0]);
        png.extend_from_slice(b"IDAT");
        assert_eq!(ft(&png), pair("image/png", "png"));

        let mut heic = vec![0u8, 0, 0, 0x18];
        heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        heic.resize(40, 0);
        assert_eq!(ft(&heic), pair("image/heic", "heic"));
        let mut mif1 = vec![0u8, 0, 0, 0x18];
        mif1.extend_from_slice(b"ftypmif1\0\0\0\0mif1heic");
        mif1.resize(40, 0);
        assert_eq!(ft(&mif1), pair("image/heic", "heic"));
        assert_eq!(route(&mif1, "HEIC"), Some("image"));
    }

    /// `filetype` reads the first 8192 bytes only.
    #[test]
    fn only_the_signature_bytes_are_read() {
        let mut long = PNG.to_vec();
        long.resize(SIGNATURE_BYTES * 2, b'a');
        assert_eq!(ft(&long), pair("image/png", "png"));
        // An empty file is plain text.
        assert_eq!(ft(b""), pair("text/plain", "txt"));
    }

    #[test]
    fn html_bytes_that_are_binary_do_not_take_the_html_route() {
        assert_eq!(route(PNG, "html"), Some("image"));
    }
}
