//! On-disk cassette format and content-addressed hashing for the record/replay
//! mock LLM.
//!
//! Both the recorder (writes responses) and the replay mock (looks them up) share
//! this format and hashing rule so a value written by the recorder is found by the
//! mock under the identical key. We key on content
//! (`sha256(user input + canonical schema)`) rather than title-substring matching:
//! content addressing is unambiguous, needs no per-corpus tuning, and matches the
//! repo's existing UUID5 philosophy.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{LlmError, LlmResult};
use crate::types::Message;

/// Which LLM method produced a recorded response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CassetteMethod {
    Generate,
    StructuredOutput,
    TranscribeImage,
}

/// A single recorded response, keyed in [`LlmCassette::entries`] by its input hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CassetteEntry {
    /// The method that produced this response.
    pub method: CassetteMethod,
    /// A short, human-readable preview of the user input (for hand-editing).
    pub user_input_preview: String,
    /// The schema name, when the call requested structured output.
    pub schema_name: Option<String>,
    /// The recorded response payload.
    pub response: Value,
}

/// A human-readable, hand-editable collection of recorded LLM responses.
///
/// Serialized to pretty JSON. `entries` is a [`BTreeMap`] so the file is
/// deterministically ordered and diffs cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCassette {
    /// On-disk format version.
    pub version: u32,
    /// Model the responses were recorded against.
    pub model: String,
    /// Recorded responses, keyed by input hash.
    pub entries: BTreeMap<String, CassetteEntry>,
}

impl LlmCassette {
    /// Load a cassette from `path`.
    ///
    /// Filesystem failures map to [`LlmError::ConfigError`]; JSON-parse failures map
    /// to [`LlmError::DeserializationError`].
    pub fn load(path: impl AsRef<Path>) -> LlmResult<Self> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path).map_err(|e| {
            LlmError::ConfigError(format!("failed to read cassette {}: {e}", path.display()))
        })?;
        serde_json::from_str(&contents).map_err(|e| {
            LlmError::DeserializationError(format!(
                "failed to parse cassette {}: {e}",
                path.display()
            ))
        })
    }

    /// Save this cassette to `path` as pretty JSON.
    ///
    /// Serialization failures map to [`LlmError::SerializationError`]; filesystem
    /// failures map to [`LlmError::ConfigError`].
    pub fn save(&self, path: impl AsRef<Path>) -> LlmResult<()> {
        let path = path.as_ref();
        let contents = serde_json::to_string_pretty(self).map_err(|e| {
            LlmError::SerializationError(format!("failed to serialize cassette: {e}"))
        })?;
        // Create the parent directory if it does not exist yet, so recording to a
        // fresh `tests/fixtures/cassettes/<name>.json` path succeeds on the first
        // run instead of silently failing in `RecordingLlm`'s Drop flush.
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| {
                LlmError::ConfigError(format!(
                    "failed to create cassette directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
        std::fs::write(path, contents).map_err(|e| {
            LlmError::ConfigError(format!("failed to write cassette {}: {e}", path.display()))
        })
    }
}

/// Compute a stable content hash for an LLM call.
///
/// The hash is `sha256` of each message rendered as `"{role}:{content}\n"` (role via
/// its serde representation so it is stable), optionally followed by the
/// canonicalized schema (object keys recursively sorted). Returns lowercase hex.
pub fn input_hash(messages: &[Message], schema: Option<&Value>) -> String {
    let mut buf = String::new();
    for message in messages {
        buf.push_str(role_str(&message.role));
        buf.push(':');
        buf.push_str(&message.content);
        buf.push('\n');
    }
    if let Some(schema) = schema {
        buf.push_str(&canonicalize(schema));
    }
    hex_digest(buf.as_bytes())
}

/// Compute a stable content hash for a vision/transcription call:
/// `sha256(mime_type bytes + image bytes)`. Returns lowercase hex.
pub fn vision_hash(image_bytes: &[u8], mime_type: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mime_type.as_bytes());
    hasher.update(image_bytes);
    hex(hasher.finalize())
}

/// Render a [`crate::types::MessageRole`] via its serde representation so the hash
/// is stable regardless of the Rust variant name.
fn role_str(role: &crate::types::MessageRole) -> &'static str {
    use crate::types::MessageRole;
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

/// Recursively serialize a JSON value with object keys sorted, so two logically
/// equal values with different insertion order produce identical strings.
///
/// Note: object *keys* are order-insensitive, but JSON *array* order is
/// significant and part of the hash. The hashed structured-output schema
/// contains arrays (e.g. `"required": [...]`, property lists), so a schemars
/// version bump or a field reorder in a schema type (`KnowledgeGraph`,
/// `SummarizedContent`, …) changes the hash and misses every recorded entry.
/// That surfaces loudly — replay tests use `MissPolicy::Error` and the e2e
/// skip-blocks call `fail_loudly_in_cassette_mode` — so the remedy is simply to
/// re-record the cassettes (see docs/build/ci-test-parallelism.md), not a
/// silent test-coverage loss.
fn canonicalize(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            let mut out = String::from("{");
            for (i, (key, val)) in sorted.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                // serde_json::to_string on a &str cannot fail; fall back to a
                // debug-quoted form defensively.
                let key_str = serde_json::to_string(key).unwrap_or_else(|_| format!("{key:?}"));
                out.push_str(&key_str);
                out.push(':');
                out.push_str(&canonicalize(val));
            }
            out.push('}');
            out
        }
        Value::Array(items) => {
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&canonicalize(item));
            }
            out.push(']');
            out
        }
        // Scalars serialize deterministically already.
        other => serde_json::to_string(other).unwrap_or_else(|_| format!("{other:?}")),
    }
}

/// `sha256` of `bytes`, lowercase hex.
fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(hasher.finalize())
}

/// Render a 32-byte digest as lowercase hex.
fn hex(digest: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let digest = digest.as_ref();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;
    use serde_json::json;

    fn msgs() -> Vec<Message> {
        vec![
            Message::system("You are a helpful assistant."),
            Message::user("Extract entities from: Alice met Bob."),
        ]
    }

    #[test]
    fn input_hash_is_stable_for_same_input() {
        let schema = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        let h1 = input_hash(&msgs(), Some(&schema));
        let h2 = input_hash(&msgs(), Some(&schema));
        assert_eq!(h1, h2);
        // Lowercase hex of a sha256 digest is always 64 chars.
        assert_eq!(h1.len(), 64);
        assert!(
            h1.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    #[test]
    fn input_hash_differs_when_content_differs() {
        let base = input_hash(&msgs(), None);
        let different_content = input_hash(&[Message::user("totally different prompt")], None);
        assert_ne!(base, different_content);

        let with_schema = input_hash(&msgs(), Some(&json!({"type": "object"})));
        assert_ne!(base, with_schema);
    }

    #[test]
    fn canonicalize_is_order_independent() {
        // Same keys, different insertion order, nested objects + arrays.
        let a = json!({
            "b": [1, {"y": 2, "x": 1}],
            "a": {"k2": "v2", "k1": "v1"}
        });
        let b = json!({
            "a": {"k1": "v1", "k2": "v2"},
            "b": [1, {"x": 1, "y": 2}]
        });
        assert_eq!(canonicalize(&a), canonicalize(&b));
        assert_eq!(input_hash(&msgs(), Some(&a)), input_hash(&msgs(), Some(&b)));
    }

    #[test]
    fn canonicalize_distinguishes_array_order() {
        // Array order is semantically significant and must change the hash.
        let a = json!([1, 2, 3]);
        let b = json!([3, 2, 1]);
        assert_ne!(canonicalize(&a), canonicalize(&b));
    }

    #[test]
    fn vision_hash_is_stable_and_sensitive() {
        let h1 = vision_hash(b"\x89PNG\r\n", "image/png");
        let h2 = vision_hash(b"\x89PNG\r\n", "image/png");
        assert_eq!(h1, h2);
        assert_ne!(h1, vision_hash(b"\x89PNG\r\n", "image/jpeg"));
        assert_ne!(h1, vision_hash(b"different", "image/png"));
    }

    #[test]
    fn cassette_round_trips_through_save_load() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("cassette.json");

        let mut entries = BTreeMap::new();
        entries.insert(
            input_hash(&msgs(), None),
            CassetteEntry {
                method: CassetteMethod::Generate,
                user_input_preview: "Extract entities from: Alice met Bob.".to_string(),
                schema_name: None,
                response: json!({"content": "Alice, Bob"}),
            },
        );
        entries.insert(
            vision_hash(b"img", "image/png"),
            CassetteEntry {
                method: CassetteMethod::TranscribeImage,
                user_input_preview: "[image/png]".to_string(),
                schema_name: Some("KnowledgeGraph".to_string()),
                response: json!({"text": "a cat"}),
            },
        );

        let cassette = LlmCassette {
            version: 1,
            model: "gpt-4o-mini".to_string(),
            entries,
        };

        cassette.save(&path).expect("save cassette");
        let loaded = LlmCassette::load(&path).expect("load cassette");

        assert_eq!(loaded.version, cassette.version);
        assert_eq!(loaded.model, cassette.model);
        assert_eq!(loaded.entries.len(), cassette.entries.len());
        for (key, entry) in &cassette.entries {
            let got = loaded
                .entries
                .get(key)
                .expect("entry present after round-trip");
            assert_eq!(got.method, entry.method);
            assert_eq!(got.user_input_preview, entry.user_input_preview);
            assert_eq!(got.schema_name, entry.schema_name);
            assert_eq!(got.response, entry.response);
        }
    }
}
