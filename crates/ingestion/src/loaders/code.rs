//! Source-code loader: port of Python's `code_loader`
//! (`cognee/infrastructure/loaders/core/code_loader.py`).
//!
//! Handles `document_type = "code"`. Selecting this loader is the add-time
//! recognition of a code file: the add pipeline stores its text as
//! `code_<content_hash><ext lowercased>` ([`code_storage_file_name`]), keeps
//! `loader_engine = "code_loader"`, and stamps the row's `system_metadata`
//! with [`CODE_SYSTEM_METADATA`] (`{"source": "code"}`), which cognify reads
//! to send the item down the per-file CODE route. The stored name keeps the
//! real extension because content sniffing reports code as plain text, so
//! the stored name is the only place the language survives for enola.

use async_trait::async_trait;
use cognee_models::Document;

use super::{DocumentLoader, LoaderError, LoaderOutput};

/// Source-file extensions of the languages enola extracts. Python's
/// `SUPPORTED_CODE_EXTENSIONS` (`code_loader.py`), lowercase. It leaves out
/// extensions that double as generic config or markup (`yml`, `json`, `md`,
/// templates), so ordinary documents are not claimed as code.
pub const SUPPORTED_CODE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cs", "cxx", "dart", "fs", "go", "h", "hcl", "hh", "hpp", "java", "js",
    "jsx", "kt", "kts", "php", "proto", "py", "rake", "rb", "rs", "scala", "svelte", "swift", "tf",
    "ts", "tsx", "vb", "vue",
];

/// The `system_metadata` route stamp the code loader writes onto a row
/// (Python `store_derived_text(..., system_metadata={"source": "code"})`),
/// spelled as Python's JSON column serialises the dict.
pub const CODE_SYSTEM_METADATA: &str = r#"{"source": "code"}"#;

/// The loader's Python `loader_name`.
pub const CODE_LOADER_NAME: &str = "code_loader";

/// Whether the code loader claims a file with this extension (no dot).
///
/// Python's `CodeLoader.can_handle`: by extension only, case-insensitive, and
/// never by MIME type, since content sniffing cannot tell a language.
pub fn is_code_extension(extension: &str) -> bool {
    SUPPORTED_CODE_EXTENSIONS.contains(&extension.to_lowercase().as_str())
}

/// The stored file name of a code file's text: `code_<content_hash>` plus the
/// source's suffix, lower-cased (Python `"code_" + content_hash +
/// Path(file_path).suffix.lower()`, `code_loader.py:96-98`). `extension` is
/// the suffix without its dot; an empty one adds nothing.
pub fn code_storage_file_name(content_hash: &str, extension: &str) -> String {
    if extension.is_empty() {
        format!("code_{content_hash}")
    } else {
        format!("code_{content_hash}.{}", extension.to_lowercase())
    }
}

/// Translate line endings as Python's text mode does (universal newlines):
/// `\r\n` and a lone `\r` both become `\n`.
pub fn universal_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// Loader for source-code files.
///
/// Decodes the bytes as strict UTF-8 (a BOM is kept, as Python's
/// `encoding="utf-8"` keeps it) and translates line endings as Python's text
/// mode read does (`open(file_path, encoding=encoding)`, `code_loader.py:100`).
pub struct CodeLoader;

#[async_trait]
impl DocumentLoader for CodeLoader {
    async fn extract(&self, bytes: &[u8], _doc: &Document) -> Result<LoaderOutput, LoaderError> {
        let text =
            std::str::from_utf8(bytes).map_err(|e| LoaderError::InvalidUtf8(e.to_string()))?;
        Ok(LoaderOutput::Text(universal_newlines(text)))
    }

    fn engine_name(&self) -> &'static str {
        CODE_LOADER_NAME
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
    use cognee_models::DataPoint;
    use uuid::Uuid;

    fn doc() -> Document {
        Document {
            base: DataPoint::new("TextDocument", None),
            document_type: "code".to_string(),
            name: "payments.py".to_string(),
            raw_data_location: "file:///payments.py".to_string(),
            mime_type: "text/plain".to_string(),
            extension: "txt".to_string(),
            data_id: Uuid::new_v4(),
            external_metadata: None,
        }
    }

    async fn text(bytes: &[u8]) -> String {
        match CodeLoader.extract(bytes, &doc()).await.unwrap() {
            LoaderOutput::Text(text) => text,
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn line_endings_are_translated_as_python_text_mode() {
        assert_eq!(text(b"a\r\nb\rc\nd\r").await, "a\nb\nc\nd\n");
        assert_eq!(text(b"\r\r\n\n").await, "\n\n\n");
        assert_eq!(text(b"plain\n").await, "plain\n");
    }

    #[tokio::test]
    async fn bom_is_kept_and_non_utf8_is_refused() {
        assert_eq!(text(b"\xEF\xBB\xBFx = 1").await, "\u{feff}x = 1");
        let error = CodeLoader.extract(&[0xFF, 0xFE], &doc()).await.unwrap_err();
        assert!(matches!(error, LoaderError::InvalidUtf8(_)));
    }

    #[test]
    fn engine_name_and_extensions_match_python() {
        assert_eq!(CodeLoader.engine_name(), "code_loader");
        assert_eq!(SUPPORTED_CODE_EXTENSIONS.len(), 31);
        assert!(is_code_extension("py"));
        assert!(is_code_extension("PY"));
        assert!(is_code_extension("Tsx"));
        for not_code in [
            "txt", "md", "json", "yml", "yaml", "html", "sh", "graphql", "",
        ] {
            assert!(!is_code_extension(not_code), "{not_code}");
        }
    }

    #[test]
    fn storage_name_keeps_the_lowercased_extension() {
        assert_eq!(code_storage_file_name("abc", "PY"), "code_abc.py");
        assert_eq!(code_storage_file_name("abc", "rs"), "code_abc.rs");
        assert_eq!(code_storage_file_name("abc", ""), "code_abc");
    }
}
