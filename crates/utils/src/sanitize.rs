//! NUL-byte stripping for values on their way into Postgres.
//!
//! Postgres cannot store `0x00` in `text`/`varchar`, and its JSON(B) parser
//! rejects the `\u0000` escape that `serde_json` emits for an embedded NUL —
//! `unsupported Unicode escape sequence`. Text extracted from real-world PDFs
//! routinely contains literal NULs, so any write that carries such text into a
//! `text` column or a `jsonb` blob fails the whole statement.
//!
//! Mirrors Python's `sanitize_relational_payload`
//! (`cognee/modules/graph/methods/sanitize_relational_payload.py`): strip NUL
//! from strings and recurse through arrays and objects, sanitizing object keys
//! as well as values. Python's `bytes` arm has no Rust equivalent — a Rust
//! `String` is already valid UTF-8, and `serde_json` has no byte-string
//! variant.
//!
//! # Where to apply this
//!
//! **At the database adapter boundary, never at ingestion or chunking.** The
//! extracted text is hashed into `raw_content_hash`, and that hash is asserted
//! byte-equal against the Python SDK in the cross-SDK parity suite. Python
//! deliberately sanitizes only when writing, keeping the stored bytes and the
//! hash intact. Stripping earlier would change chunk boundaries, token counts
//! and content-addressed IDs, and break parity.
//!
//! Non-Postgres backends do not need this: SQLite tolerates an embedded NUL in
//! `TEXT`, and neither Ladybug nor LanceDB round-trips through a Postgres JSON
//! parser. Applying it unconditionally in shared conversion code is still
//! correct — a NUL byte carries no meaning in any of these payloads.

use std::borrow::Cow;

use serde_json::{Map, Value};

/// Strip NUL bytes from a string slice.
///
/// Returns `Cow::Borrowed` when there is nothing to strip. Callers that need an
/// owned `String` still pay one allocation via `into_owned`; the borrow only
/// pays off where the result can stay borrowed, so prefer [`sanitize_string`]
/// when the input is already owned.
///
/// ```
/// # use cognee_utils::sanitize::sanitize_str;
/// # use std::borrow::Cow;
/// assert!(matches!(sanitize_str("clean"), Cow::Borrowed("clean")));
/// assert_eq!(sanitize_str("a\0b"), "ab");
/// ```
pub fn sanitize_str(value: &str) -> Cow<'_, str> {
    if value.contains('\0') {
        Cow::Owned(value.replace('\0', ""))
    } else {
        Cow::Borrowed(value)
    }
}

/// Strip NUL bytes from an owned string, reusing the allocation when it is
/// already clean.
pub fn sanitize_string(mut value: String) -> String {
    if value.contains('\0') {
        value.retain(|c| c != '\0');
    }
    value
}

/// Recursively strip NUL bytes from a JSON value, in place.
///
/// Walks strings, arrays and object values, and rewrites object *keys* too. Two
/// keys that collapse to the same text after stripping merge, last one wins —
/// the same outcome as Python's dict comprehension.
///
/// "Last" means last in `serde_json::Map` iteration order, and *which* key that
/// is depends on the final binary's build graph, not on this crate. With
/// serde_json's `preserve_order` enabled anywhere in it, `Map` is an
/// insertion-ordered `IndexMap` and the last *inserted* colliding key wins —
/// Python's dict-comprehension rule. Without it `Map` is a `BTreeMap` and the
/// winner is whichever *raw* key sorts last instead.
///
/// This crate deliberately does not enable the feature itself, because doing so
/// would force the `IndexMap` backing on every downstream consumer of
/// `cognee-utils` (see the dependency comment in `Cargo.toml`). Every cognee
/// build gets the Python rule regardless, via `cognee-database` and
/// `cognee-visualization`. The divergence is reachable only when one object
/// holds two keys differing by nothing but an embedded NUL.
pub fn sanitize_json_in_place(value: &mut Value) {
    match value {
        Value::String(s) => {
            if s.contains('\0') {
                s.retain(|c| c != '\0');
            }
        }
        Value::Array(items) => {
            for item in items {
                sanitize_json_in_place(item);
            }
        }
        Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                sanitize_json_in_place(v);
            }
            if map.keys().any(|k| k.contains('\0')) {
                let mut rebuilt = Map::with_capacity(map.len());
                for (k, v) in std::mem::take(map) {
                    rebuilt.insert(sanitize_string(k), v);
                }
                *map = rebuilt;
            }
        }
        // Numbers, booleans and null cannot carry a NUL byte.
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Owned convenience wrapper around [`sanitize_json_in_place`].
///
/// ```
/// # use cognee_utils::sanitize::sanitize_json;
/// # use serde_json::json;
/// assert_eq!(sanitize_json(json!({"text": "a\0b"})), json!({"text": "ab"}));
/// ```
pub fn sanitize_json(mut value: Value) -> Value {
    sanitize_json_in_place(&mut value);
    value
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_str_is_borrowed() {
        assert!(matches!(sanitize_str("no nulls here"), Cow::Borrowed(_)));
        assert!(matches!(sanitize_str(""), Cow::Borrowed(_)));
    }

    #[test]
    fn dirty_str_is_stripped() {
        assert_eq!(sanitize_str("a\0b\0c"), "abc");
        assert_eq!(sanitize_str("\0"), "");
        assert_eq!(sanitize_str("\0lead"), "lead");
        assert_eq!(sanitize_str("trail\0"), "trail");
    }

    #[test]
    fn sanitize_string_matches_str_variant() {
        assert_eq!(sanitize_string("a\0b".to_string()), "ab");
        assert_eq!(sanitize_string("clean".to_string()), "clean");
    }

    #[test]
    fn non_ascii_is_preserved() {
        // The NUL scan must not disturb multi-byte UTF-8 around it.
        assert_eq!(sanitize_str("é\0日本語\0—"), "é日本語—");
    }

    #[test]
    fn nested_json_is_sanitized() {
        let input = json!({
            "text": "pdf\0text",
            "nested": {"inner": ["a\0", {"deep": "b\0c"}]},
            "count": 42,
            "flag": true,
            "nothing": null,
        });
        let expected = json!({
            "text": "pdftext",
            "nested": {"inner": ["a", {"deep": "bc"}]},
            "count": 42,
            "flag": true,
            "nothing": null,
        });
        assert_eq!(sanitize_json(input), expected);
    }

    #[test]
    fn object_keys_are_sanitized() {
        let mut input = Map::new();
        input.insert("ke\0y".to_string(), json!("value"));
        let out = sanitize_json(Value::Object(input));
        assert_eq!(out, json!({"key": "value"}));
    }

    /// Direct port of Python's
    /// `test_sanitize_relational_payload_strips_null_bytes_recursively`
    /// (`cognee/tests/unit/modules/graph/test_relational_upserts.py:37`), with
    /// its tuple entry expressed as a JSON array — JSON has no tuple type, and
    /// `serde_json` serializes a Rust tuple as an array anyway.
    #[test]
    fn python_parity_strips_null_bytes_recursively() {
        let payload = json!({
            "ti\u{0}tle": "hel\u{0}lo",
            "nested": ["wo\u{0}rld", {"ke\u{0}y": "va\u{0}lue"}],
            "tupled": ["a\u{0}", 1],
        });

        assert_eq!(
            sanitize_json(payload),
            json!({
                "title": "hello",
                "nested": ["world", {"key": "value"}],
                "tupled": ["a", 1],
            })
        );
    }

    /// Python's companion case,
    /// `test_sanitize_relational_payload_decodes_bytes_and_bytearray`, has no
    /// Rust analogue and deliberately gets no port: it exists because a Python
    /// payload can hold raw `bytes` that are not valid UTF-8, which Python
    /// decodes with `errors="replace"`. A Rust `String` is UTF-8 by
    /// construction and `serde_json::Value` has no byte-string variant, so the
    /// failure it guards against is unrepresentable here. This test pins the
    /// one part that *is* representable — a lone replacement character is
    /// ordinary text and must survive untouched.
    #[test]
    fn python_parity_replacement_char_is_not_a_nul() {
        assert_eq!(sanitize_str("\u{fffd}"), "\u{fffd}");
        assert_eq!(sanitize_str("a\u{fffd}\u{0}b"), "a\u{fffd}b");
    }

    /// Pins the collision rule under *both* `serde_json::Map` backings, since
    /// which one is in play is a property of the final binary's build graph
    /// rather than of this crate (see `sanitize_json_in_place`).
    ///
    /// `"ab"` is inserted first and `"a\u{0}b"` second. With `preserve_order`
    /// somewhere in the graph — which is every cognee build, via
    /// `cognee-database` and `cognee-visualization` — `Map` is insertion-ordered
    /// and the *second* insertion wins the collapse, matching Python's dict
    /// comprehension. In a bare `cargo test -p cognee-utils` or the wasm32 lane
    /// `Map` is a `BTreeMap`, the raw keys iterate sorted (`"a\u{0}b"` before
    /// `"ab"`, since 0x00 < 0x62) and the first insertion survives instead.
    ///
    /// Asserting both keeps the standalone configuration covered too; the old
    /// version of this test only held in the feature-enabled one.
    #[test]
    fn colliding_keys_resolve_to_the_last_in_map_order() {
        let mut input = Map::new();
        input.insert("ab".to_string(), json!("inserted_first"));
        input.insert("a\u{0}b".to_string(), json!("inserted_second"));

        let insertion_ordered = input.keys().map(String::as_str).eq(["ab", "a\u{0}b"]);
        let expected = if insertion_ordered {
            "inserted_second"
        } else {
            "inserted_first"
        };

        let out = sanitize_json(Value::Object(input));

        assert_eq!(
            out,
            json!({ "ab": expected }),
            "the last colliding key in Map iteration order must win \
             (insertion_ordered = {insertion_ordered})"
        );
    }

    #[test]
    fn clean_json_round_trips_unchanged() {
        let input = json!({"a": [1, "two", {"b": null}]});
        assert_eq!(sanitize_json(input.clone()), input);
    }

    #[test]
    fn sanitized_json_survives_serialization() {
        // The failure this guards: `serde_json` renders an embedded NUL as the
        // `\u0000` escape, which Postgres' jsonb parser rejects outright.
        let dirty = serde_json::to_string(&json!({"text": "a\0b"}))
            .expect("json object with a string value always serializes");
        assert!(dirty.contains("\\u0000"));

        let clean = serde_json::to_string(&sanitize_json(json!({"text": "a\0b"})))
            .expect("json object with a string value always serializes");
        assert!(!clean.contains("\\u0000"));
    }
}
