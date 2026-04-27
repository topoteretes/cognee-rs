//! Shared multipart-body parsing helpers.
//!
//! Provides [`parse_multipart`] — a generic drain over an `axum::extract::Multipart`
//! stream that classifies each part as either an in-memory **form field** (≤ 4 KiB)
//! or a **spooled file** written to a per-request temp directory.
//!
//! Per-router validation (filename traversal checks, URL-body detection, extension
//! checks, …) belongs in each router's own parse adapter, **not** here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::extract::Multipart;
use bytes::Bytes;
use tokio::io::AsyncWriteExt;

use crate::error::ApiError;

/// Options that control multipart parsing.
pub struct MultipartOpts {
    /// Maximum total number of parts (form fields + files combined).
    pub max_parts: usize,
    /// Maximum byte size of a form-field value (anything exceeding this is rejected).
    pub form_field_max_bytes: usize,
    /// Maximum byte size for a spooled file part.
    pub file_max_bytes: usize,
    /// Base directory for spooled temp files.  Each request gets its own sub-dir.
    pub spool_dir: PathBuf,
}

impl Default for MultipartOpts {
    fn default() -> Self {
        Self {
            max_parts: 256,
            form_field_max_bytes: 4096,
            file_max_bytes: 1024 * 1024 * 1024, // 1 GiB
            spool_dir: std::env::temp_dir().join("cognee-uploads"),
        }
    }
}

/// A single spooled-to-disk file part.
#[derive(Debug)]
pub struct SpooledFile {
    /// Original filename reported by the client (NOT sanitized for path safety —
    /// callers must validate before using as a file system component).
    pub filename: Option<String>,
    /// Content-Type header value from the part, if present.
    pub content_type: Option<String>,
    /// Absolute path of the spooled temp file.
    pub path: PathBuf,
    /// Number of bytes written to disk.
    pub byte_count: u64,
}

/// Result of a [`parse_multipart`] call.
pub struct ParsedForm {
    /// In-memory form fields keyed by part name.
    /// Repeated names collect into `Vec<String>` (the outer map value is always
    /// `Vec<String>` to handle repeated fields without losing data).
    pub fields: HashMap<String, Vec<String>>,
    /// Spooled files keyed by part name.  Multiple parts with the same name
    /// accumulate in the `Vec`.
    pub files: HashMap<String, Vec<SpooledFile>>,
    /// Directory where all spooled files in this request live.  Callers should
    /// either move/use the files before dropping this struct or wrap it in an
    /// [`UploadGuard`].
    pub spool_dir: PathBuf,
}

// ─── UploadGuard ─────────────────────────────────────────────────────────────

/// RAII wrapper that removes the per-request spool directory on `Drop`.
///
/// Wrap the `ParsedForm` (or just the `spool_dir` path) in this guard so that
/// failures — validation errors, pipeline panics, early returns — always clean
/// up the temp files.
pub struct UploadGuard {
    spool_dir: PathBuf,
}

impl UploadGuard {
    /// Create a guard for `dir`.  The directory is removed recursively when the
    /// guard is dropped (best-effort — errors are silently ignored).
    pub fn new(dir: PathBuf) -> Self {
        Self { spool_dir: dir }
    }

    /// Return the protected directory path.
    pub fn dir(&self) -> &Path {
        &self.spool_dir
    }
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.spool_dir);
    }
}

// ─── parse_multipart ─────────────────────────────────────────────────────────

/// Drain an axum `Multipart` stream into a [`ParsedForm`].
///
/// Parts whose name is absent are silently skipped.
///
/// # Errors
/// Returns [`ApiError::BadRequest`] when:
/// - more than `opts.max_parts` parts are seen,
/// - a non-file part exceeds `opts.form_field_max_bytes`,
/// - a file part exceeds `opts.file_max_bytes`.
pub async fn parse_multipart(
    mut multipart: Multipart,
    opts: &MultipartOpts,
    request_id: &str,
) -> Result<ParsedForm, ApiError> {
    let spool_dir = opts.spool_dir.join(sanitize_path_component(request_id));
    tokio::fs::create_dir_all(&spool_dir)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("failed to create spool dir: {e}")))?;

    let mut fields: HashMap<String, Vec<String>> = HashMap::new();
    let mut files: HashMap<String, Vec<SpooledFile>> = HashMap::new();
    let mut part_count = 0usize;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("multipart parse error: {e}")))?
    {
        part_count += 1;
        if part_count > opts.max_parts {
            return Err(ApiError::BadRequest(format!(
                "Too many parts (max {})",
                opts.max_parts
            )));
        }

        let name = match field.name() {
            Some(n) => n.to_owned(),
            None => continue, // skip nameless parts
        };

        let filename = field.file_name().map(|s| s.to_owned());
        let content_type = field.content_type().map(|s| s.to_owned());

        // Decide: file part or form field?
        // Treat a part as a file if it has a filename OR has a non-text content-type.
        let is_file = filename.is_some()
            || content_type
                .as_deref()
                .map(|ct| !ct.starts_with("text/"))
                .unwrap_or(false);

        if is_file {
            let safe_name = filename
                .as_deref()
                .map(sanitize_path_component)
                .unwrap_or_else(|| format!("part-{part_count}"));
            let dest = spool_dir.join(format!("{part_count}-{safe_name}"));
            let byte_count = stream_to_disk(field, &dest, opts.file_max_bytes).await?;
            files.entry(name).or_default().push(SpooledFile {
                filename,
                content_type,
                path: dest,
                byte_count,
            });
        } else {
            // Form field — buffer in memory with a size cap.
            let data: Bytes = field
                .bytes()
                .await
                .map_err(|e| ApiError::BadRequest(format!("form field read error: {e}")))?;
            if data.len() > opts.form_field_max_bytes {
                return Err(ApiError::BadRequest(format!(
                    "Form field {name} exceeds {} bytes",
                    opts.form_field_max_bytes
                )));
            }
            let value = String::from_utf8_lossy(&data).into_owned();
            fields.entry(name).or_default().push(value);
        }
    }

    Ok(ParsedForm {
        fields,
        files,
        spool_dir,
    })
}

// ─── stream_to_disk ──────────────────────────────────────────────────────────

/// Stream a multipart `field` to `dest`, enforcing a `max_bytes` limit.
///
/// Returns the total byte count written.
async fn stream_to_disk(
    field: axum::extract::multipart::Field<'_>,
    dest: &Path,
    max_bytes: usize,
) -> Result<u64, ApiError> {
    let mut file = tokio::fs::File::create(dest)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("failed to create spool file: {e}")))?;

    let data: Bytes = field
        .bytes()
        .await
        .map_err(|e| ApiError::BadRequest(format!("file read error: {e}")))?;

    if data.len() > max_bytes {
        return Err(ApiError::BadRequest(format!(
            "file part exceeds maximum size of {} bytes",
            max_bytes
        )));
    }

    file.write_all(&data)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("failed to write spool file: {e}")))?;

    Ok(data.len() as u64)
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Replace any filesystem-unsafe characters with `_` and truncate to 200 chars.
pub fn sanitize_path_component(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c == '/'
                || c == '\\'
                || c == ':'
                || c == '*'
                || c == '?'
                || c == '"'
                || c == '<'
                || c == '>'
                || c == '|'
                || c == '\0'
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Truncate to 200 bytes (safe for most filesystems).
    let mut truncated = sanitized;
    while truncated.len() > 200 {
        truncated.pop();
    }
    truncated
}

/// Check a filename for path traversal sequences.
///
/// Returns `Err` with an [`ApiError::BadRequest`] if the name contains `../`,
/// `..\`, or starts with `/`.
#[allow(clippy::result_large_err)] // ApiError is inherently large; boxing at this level adds noise
pub fn check_filename_traversal(filename: &str) -> Result<(), ApiError> {
    if filename.contains("../")
        || filename.contains("..\\")
        || filename.starts_with('/')
        || filename.starts_with('\\')
    {
        return Err(ApiError::BadRequest(format!(
            "Invalid filename: {filename}"
        )));
    }
    Ok(())
}
