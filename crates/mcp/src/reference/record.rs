use std::ffi::OsStr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{ReferenceError, ReferenceLimits};
use crate::redact::redact_json;

pub const REFERENCE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    File(PathBuf),
    Stdin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    File,
    Stdin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceOperation {
    Upsert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PreparedDocument {
    pub source_id: String,
    pub source_kind: SourceKind,
    pub source_label: String,
    pub content_type: String,
    pub content_sha256: String,
    pub normalized_bytes: usize,
    pub content: String,
    pub redaction_count: usize,
}

impl PreparedDocument {
    pub fn from_bytes(
        source: Source,
        bytes: &[u8],
        logical_source_id: Option<&str>,
        label: Option<&str>,
        limits: &ReferenceLimits,
    ) -> Result<Self, ReferenceError> {
        if matches!(source, Source::File(_)) && (logical_source_id.is_some() || label.is_some()) {
            return Err(ReferenceError::InvalidInput);
        }
        if logical_source_id.is_some_and(|value| value.trim().is_empty()) {
            return Err(ReferenceError::InvalidInput);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ReferenceError::InvalidInput)?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let normalized = normalize_newlines(text);
        if normalized.is_empty() {
            return Err(ReferenceError::InvalidInput);
        }
        if normalized.len() > limits.max_input_bytes {
            return Err(ReferenceError::InputTooLarge);
        }
        let redacted = redact_json(&Value::String(normalized));
        let content = redacted
            .value
            .as_str()
            .ok_or(ReferenceError::InvalidInput)?
            .to_owned();
        let content_sha256 = sha256_bytes(content.as_bytes());

        let (source_id, source_kind, source_label, content_type) = match source {
            Source::File(path) => {
                let canonical = path
                    .canonicalize()
                    .map_err(|_| ReferenceError::InvalidInput)?;
                let metadata =
                    std::fs::metadata(&canonical).map_err(|_| ReferenceError::InvalidInput)?;
                if !metadata.is_file() {
                    return Err(ReferenceError::InvalidInput);
                }
                let source_label = canonical
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .ok_or(ReferenceError::InvalidInput)?
                    .to_owned();
                let extension = canonical
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or_default();
                let content_type = if extension.eq_ignore_ascii_case("md")
                    || extension.eq_ignore_ascii_case("markdown")
                {
                    "text/markdown"
                } else {
                    "text/plain"
                };
                (
                    file_source_id(canonical.as_os_str()),
                    SourceKind::File,
                    source_label,
                    content_type.to_owned(),
                )
            }
            Source::Stdin => {
                let logical_source_id = logical_source_id
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let source_id = logical_source_id.map_or_else(
                    || hash_fields(&[b"stdin-content", content_sha256.as_bytes()]),
                    |value| hash_fields(&[b"stdin-logical", value.as_bytes()]),
                );
                let source_label = label
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("stdin")
                    .to_owned();
                (
                    source_id,
                    SourceKind::Stdin,
                    source_label,
                    "text/plain".to_owned(),
                )
            }
        };

        Ok(Self {
            source_id,
            source_kind,
            source_label,
            content_type,
            content_sha256,
            normalized_bytes: content.len(),
            content,
            redaction_count: redacted.redaction_count,
        })
    }

    pub(crate) fn validate(&self, limits: &ReferenceLimits) -> Result<(), ReferenceError> {
        if self.content.is_empty()
            || self.content.starts_with('\u{feff}')
            || self.content.contains('\r')
            || self.content.len() != self.normalized_bytes
            || self.normalized_bytes > limits.max_input_bytes
            || self.content_sha256 != sha256_bytes(self.content.as_bytes())
            || !is_sha256_id(&self.source_id)
            || self.source_label.is_empty()
            || !matches!(self.content_type.as_str(), "text/plain" | "text/markdown")
        {
            return Err(ReferenceError::InvalidInput);
        }
        let redacted = redact_json(&Value::String(self.content.clone()));
        if redacted.value.as_str() != Some(self.content.as_str()) {
            return Err(ReferenceError::InvalidInput);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceRecord {
    pub schema_version: u32,
    pub sequence: u64,
    pub batch_id: String,
    pub event_id: String,
    pub operation: ReferenceOperation,
    pub source_id: String,
    pub source_kind: SourceKind,
    pub source_label: String,
    pub revision: u64,
    pub content_type: String,
    pub content_sha256: String,
    pub normalized_bytes: usize,
    pub committed_at: String,
    pub supersedes_event_id: Option<String>,
    pub content: String,
    pub redaction_count: usize,
}

impl ReferenceRecord {
    pub(crate) fn from_prepared(
        document: &PreparedDocument,
        sequence: u64,
        batch_id: String,
        revision: u64,
        supersedes_event_id: Option<String>,
        committed_at: String,
    ) -> Self {
        let event_id = event_id(
            &document.source_id,
            revision,
            &document.content_sha256,
            ReferenceOperation::Upsert,
        );
        Self {
            schema_version: REFERENCE_SCHEMA_VERSION,
            sequence,
            batch_id,
            event_id,
            operation: ReferenceOperation::Upsert,
            source_id: document.source_id.clone(),
            source_kind: document.source_kind,
            source_label: document.source_label.clone(),
            revision,
            content_type: document.content_type.clone(),
            content_sha256: document.content_sha256.clone(),
            normalized_bytes: document.normalized_bytes,
            committed_at,
            supersedes_event_id,
            content: document.content.clone(),
            redaction_count: document.redaction_count,
        }
    }

    pub fn verify(&self) -> bool {
        self.schema_version == REFERENCE_SCHEMA_VERSION
            && self.content_sha256 == sha256_bytes(self.content.as_bytes())
            && self.normalized_bytes == self.content.len()
            && self.event_id
                == event_id(
                    &self.source_id,
                    self.revision,
                    &self.content_sha256,
                    self.operation,
                )
    }
}

fn normalize_newlines(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            output.push('\n');
        } else {
            output.push(character);
        }
    }
    output
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub(crate) fn hash_fields(fields: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for field in fields {
        digest.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(field);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn is_sha256_id(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn file_source_id(path: &OsStr) -> String {
    hash_fields(&[b"file", path.as_encoded_bytes()])
}

fn event_id(
    source_id: &str,
    revision: u64,
    content_sha256: &str,
    operation: ReferenceOperation,
) -> String {
    let schema = REFERENCE_SCHEMA_VERSION.to_be_bytes();
    let revision = revision.to_be_bytes();
    let operation = match operation {
        ReferenceOperation::Upsert => b"upsert".as_slice(),
    };
    hash_fields(&[
        &schema,
        source_id.as_bytes(),
        &revision,
        content_sha256.as_bytes(),
        operation,
    ])
}

#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    use super::file_source_id;

    #[test]
    fn file_identity_preserves_non_utf8_path_bytes() {
        let first = file_source_id(OsStr::from_bytes(b"/reference/\xfe/standard.md"));
        let second = file_source_id(OsStr::from_bytes(b"/reference/\xff/standard.md"));

        assert_ne!(first, second);
    }
}
