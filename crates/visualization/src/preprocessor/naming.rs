//! Human-readable node naming and node-set membership helpers.
//!
//! Direct port of the following pieces of
//! `cognee/modules/visualization/preprocessor.py`:
//!   * `_node_set_names()` (lines 141–153)
//!   * `is_distilled_learning_node()` (lines 156–158)
//!   * `_IDENTIFIER_LIKE_RE` / `looks_like_identifier()` (lines 186–194)
//!   * `derive_node_name()` (lines 197–216)
//!
//! Identifier-shaped values (UUIDs, content hashes) are never used as display
//! names — surfacing them as labels was the single biggest first-session trust
//! killer in user testing.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::py_str;

/// Node set produced by the distillation bridge (Python
/// `_DISTILLED_LEARNING_NODE_SET`, `preprocessor.py:133`).
pub(crate) const DISTILLED_LEARNING_NODE_SET: &str = "session_learnings";

/// Maximum length (in **characters**, matching Python's `normalized[:120]`
/// slice) of a name derived from a text-bearing field.
const MAX_DERIVED_NAME_CHARS: usize = 120;

/// Fallback keys `derive_node_name` walks, in Python's order
/// (`preprocessor.py:209`).
pub(crate) const NAME_FALLBACK_KEYS: [&str; 5] =
    ["title", "text", "summary", "description", "content"];

/// True for UUID- or content-hash-shaped strings that would read as noise in
/// the UI.
///
/// Hand-rolled equivalent of Python's case-insensitive
/// `^(?:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32,64})$`
/// applied to the **trimmed** value (`preprocessor.py:186-194`). Non-string
/// JSON values are never identifier-shaped, exactly like Python's
/// `isinstance(value, str)` guard.
pub(crate) fn looks_like_identifier(value: &Value) -> bool {
    match value {
        Value::String(s) => is_identifier_shaped(s),
        _ => false,
    }
}

/// String-level half of [`looks_like_identifier`].
///
/// Python `.strip()`s before matching and the pattern is fully anchored, so a
/// simple length + character-class check is exact. Byte indexing is safe: any
/// multi-byte character fails the hex-digit test anyway.
pub(crate) fn is_identifier_shaped(raw: &str) -> bool {
    let bytes = raw.trim().as_bytes();

    // Dashed UUID form: 8-4-4-4-12 hex digits.
    if bytes.len() == 36 {
        const DASH_POSITIONS: [usize; 4] = [8, 13, 18, 23];
        let uuid_shaped = bytes.iter().enumerate().all(|(index, byte)| {
            if DASH_POSITIONS.contains(&index) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
        if uuid_shaped {
            return true;
        }
    }

    // Bare hash form: 32–64 hex digits.
    (32..=64).contains(&bytes.len()) && bytes.iter().all(u8::is_ascii_hexdigit)
}

/// Pick a human-readable label for a node, falling back through
/// name/title/text/summary/description/content and finally an explicit
/// `Unnamed <Type> (<id prefix>)` placeholder.
///
/// Port of `derive_node_name()` (`preprocessor.py:197-216`). The whitespace
/// collapse mirrors `" ".join(value.split())` and the truncation is by
/// characters, not bytes, like Python's `normalized[:120]`.
pub(crate) fn derive_node_name(node_info: &Map<String, Value>, node_id: &str) -> String {
    // `if name and not looks_like_identifier(name)` — a blank name is falsy.
    if let Some(Value::String(name)) = node_info.get("name")
        && !name.is_empty()
        && !is_identifier_shaped(name)
    {
        return name.clone();
    }

    for key in NAME_FALLBACK_KEYS {
        if let Some(Value::String(value)) = node_info.get(key)
            && !value.trim().is_empty()
            && !is_identifier_shaped(value)
        {
            let normalized: Vec<&str> = value.split_whitespace().collect();
            return normalized
                .join(" ")
                .chars()
                .take(MAX_DERIVED_NAME_CHARS)
                .collect();
        }
    }

    // `node_info.get("type") or "node"`, stringified into the placeholder.
    let node_type = node_info
        .get("type")
        .filter(|value| super::is_truthy(value))
        .map_or_else(|| "node".to_string(), py_str);
    let id_prefix: String = node_id.chars().take(8).collect();
    format!("Unnamed {node_type} ({id_prefix})")
}

/// Collect the node-set names attached to a node via `source_node_set`
/// (a comma-joined string) or `belongs_to_set` (a list of name strings).
///
/// Port of `_node_set_names()` (`preprocessor.py:141-153`). Returns a
/// `BTreeSet` so callers get deterministic iteration order.
pub(crate) fn node_set_names(node_info: &Map<String, Value>) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();

    if let Some(Value::String(raw)) = node_info.get("source_node_set") {
        names.extend(
            raw.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string),
        );
    }

    match node_info.get("belongs_to_set") {
        Some(Value::Array(items)) => {
            // Only the string members count, mirroring Python's
            // `b for b in belongs if isinstance(b, str)`.
            names.extend(items.iter().filter_map(|item| match item {
                Value::String(name) => Some(name.clone()),
                _ => None,
            }));
        }
        Some(Value::String(name)) => {
            names.insert(name.clone());
        }
        _ => {}
    }

    names
}

/// True when a node belongs to the distilled session-learnings node set
/// (`preprocessor.py:156-158`).
pub(crate) fn is_distilled_learning_node(node_info: &Map<String, Value>) -> bool {
    node_set_names(node_info).contains(DISTILLED_LEARNING_NODE_SET)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn info(value: Value) -> Map<String, Value> {
        value
            .as_object()
            .expect("test fixture is a JSON object")
            .clone()
    }

    #[test]
    fn identifiers_detected() {
        assert!(is_identifier_shaped("13e52fce-2d52-4a8b-9f01-aabbccddeeff"));
        assert!(is_identifier_shaped("13E52FCE-2D52-4A8B-9F01-AABBCCDDEEFF"));
        assert!(is_identifier_shaped(&"a".repeat(32)));
        assert!(is_identifier_shaped(&"a".repeat(64)));
        // Surrounding whitespace is stripped before matching.
        assert!(is_identifier_shaped(&format!("  {}  ", "f".repeat(40))));
        assert!(is_identifier_shaped(
            "\t13e52fce-2d52-4a8b-9f01-aabbccddeeff\n"
        ));
    }

    #[test]
    fn non_identifiers_rejected() {
        assert!(!is_identifier_shaped("Alice"));
        assert!(!is_identifier_shaped(&"a".repeat(31)));
        assert!(!is_identifier_shaped(&"a".repeat(65)));
        // Right length, wrong alphabet.
        assert!(!is_identifier_shaped(&"z".repeat(32)));
        // Dashes in the wrong places.
        assert!(!is_identifier_shaped(
            "13e52fce2d52-4a8b-9f01-aabbccddeeff-"
        ));
        assert!(!looks_like_identifier(&json!(42)));
        assert!(!looks_like_identifier(&Value::Null));
    }

    #[test]
    fn derive_prefers_readable_name() {
        let node = info(json!({"name": "Alice", "text": "ignored"}));
        assert_eq!(derive_node_name(&node, "n1"), "Alice");
    }

    #[test]
    fn derive_skips_identifier_shaped_name_and_fields() {
        let node = info(json!({
            "type": "DocumentChunk",
            "name": "13e52fce-2d52-4a8b-9f01-aabbccddeeff",
            "text": "a".repeat(64),
            "description": "  real   text\nhere ",
        }));
        assert_eq!(derive_node_name(&node, "n1"), "real text here");
    }

    #[test]
    fn derive_truncates_to_120_characters() {
        let node = info(json!({"type": "TextSummary", "text": "é".repeat(200)}));
        let name = derive_node_name(&node, "n1");
        assert_eq!(name.chars().count(), 120);
    }

    #[test]
    fn derive_falls_back_to_unnamed_placeholder() {
        let node = info(json!({"type": "Entity"}));
        assert_eq!(
            derive_node_name(&node, "13e52fce-2d52-4a8b-9f01-aabbccddeeff"),
            "Unnamed Entity (13e52fce)"
        );
        // No type at all → the literal "node".
        let untyped = info(json!({}));
        assert_eq!(derive_node_name(&untyped, "short"), "Unnamed node (short)");
    }

    #[test]
    fn node_set_names_from_comma_string_and_list() {
        let node = info(json!({
            "source_node_set": " a , b ,,c ",
            "belongs_to_set": ["d", 5, "e"],
        }));
        let names = node_set_names(&node);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec!["a", "b", "c", "d", "e"]
        );
    }

    #[test]
    fn node_set_names_from_bare_string_belongs_to_set() {
        let node = info(json!({"belongs_to_set": "solo"}));
        assert!(node_set_names(&node).contains("solo"));
    }

    #[test]
    fn distilled_learning_node_detected() {
        let node = info(json!({"source_node_set": "other,session_learnings"}));
        assert!(is_distilled_learning_node(&node));
        let plain = info(json!({"source_node_set": "other"}));
        assert!(!is_distilled_learning_node(&plain));
    }
}
