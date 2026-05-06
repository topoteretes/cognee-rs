//! Recursive URL-sanitization for caller-supplied properties.
//!
//! Mirrors Python `_sanitize_nested_properties` (utils.py:107-124).
//! For every nested string value whose key matches one of `names`,
//! the value is replaced with `uuid5(NAMESPACE_OID, value)`.

#[cfg(feature = "telemetry")]
use serde_json::Value;
#[cfg(feature = "telemetry")]
use uuid::Uuid;

/// SHA-1 OID namespace: `6ba7b812-9dad-11d1-80b4-00c04fd430c8`.
///
/// Same constant used elsewhere in the workspace
/// (`cognee_utils::id_generation::NAMESPACE_OID`) — reusing it keeps
/// uuid5 derivations consistent across the SDK.
#[cfg(feature = "telemetry")]
const NAMESPACE_OID: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x12, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

/// Replace, in-place, every string value whose key is in `names`
/// with `uuid5(NAMESPACE_OID, value).to_string()`. Walks objects
/// and arrays recursively; leaves other scalar types untouched.
#[cfg(feature = "telemetry")]
pub fn sanitize_nested_properties(value: &mut Value, names: &[&str]) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if names.contains(&k.as_str())
                    && let Value::String(s) = v
                {
                    *v = Value::String(Uuid::new_v5(&NAMESPACE_OID, s.as_bytes()).to_string());
                    continue;
                }
                sanitize_nested_properties(v, names);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                sanitize_nested_properties(item, names);
            }
        }
        _ => {}
    }
}

/// Noop when the feature is off — caller-supplied properties are
/// dropped entirely on the dispatch path, so sanitization is moot.
#[cfg(not(feature = "telemetry"))]
pub fn sanitize_nested_properties(_value: &mut (), _names: &[&str]) {}

#[cfg(all(test, feature = "telemetry"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replaces_top_level_url_string() {
        let mut v = json!({ "url": "https://example.com", "other": "x" });
        sanitize_nested_properties(&mut v, &["url"]);
        let url = v["url"].as_str().expect("url is string");
        assert_ne!(url, "https://example.com");
        assert!(Uuid::parse_str(url).is_ok(), "expected uuid5 string");
        assert_eq!(v["other"], "x");
    }

    #[test]
    fn descends_into_nested_objects_and_arrays() {
        let mut v = json!({
            "outer": {
                "inner": [
                    { "url": "https://a.example", "keep": "yes" },
                    { "url": "https://b.example" }
                ]
            }
        });
        sanitize_nested_properties(&mut v, &["url"]);
        let urls: Vec<String> = v["outer"]["inner"]
            .as_array()
            .expect("array")
            .iter()
            .map(|i| i["url"].as_str().expect("url").to_string())
            .collect();
        for u in &urls {
            assert!(Uuid::parse_str(u).is_ok(), "expected uuid5, got {u}");
        }
        assert_ne!(urls[0], urls[1], "different inputs -> different uuid5");
        assert_eq!(v["outer"]["inner"][0]["keep"], "yes");
    }

    #[test]
    fn non_string_url_value_is_left_alone() {
        // Defensive: if a caller mistakenly passes `url: 42`, do not
        // panic; just leave it alone (Python silently coerces, we
        // diverge slightly here for safety).
        let mut v = json!({ "url": 42 });
        sanitize_nested_properties(&mut v, &["url"]);
        assert_eq!(v["url"], 42);
    }
}
