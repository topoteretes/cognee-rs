//! Backend values as plain JSON, as Python's CODE retriever reads them.
//!
//! Ports of `_json_key`, `_normalize_value`, `_normalize_properties` and
//! `_scalar_text` (`code_retriever.py:264-300, 422-431`).

use serde_json::{Map, Value};

use crate::pyjson;

/// `_json_key`: `json.dumps(value, ensure_ascii=False, sort_keys=True,
/// separators=(",", ":"))`. The context text of a CODE result and the order
/// of JSON values in sorts.
pub fn json_key(value: &Value) -> String {
    pyjson::dumps_compact(value)
}

/// `_normalize_value`: backend containers as plain JSON.
///
/// A string that starts (after stripping) with `{` or `[` and parses as JSON
/// is replaced by its parsed, normalized value: Ladybug hands a stored dict
/// or list back as JSON text. Objects and arrays are normalized recursively.
/// (Python also turns UUIDs and pydantic models into JSON; a `serde_json`
/// value has neither.)
pub fn normalize_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), normalize_value(item)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(normalize_value).collect()),
        Value::String(text) => {
            let stripped = pyjson::py_strip(text);
            if (stripped.starts_with('{') || stripped.starts_with('['))
                && let Ok(parsed) = pyjson::loads(stripped)
            {
                return normalize_value(&parsed);
            }
            value.clone()
        }
        other => other.clone(),
    }
}

/// `_normalize_properties`: a node's or edge's properties as one flat object.
///
/// Adapters that keep the user properties under a nested `properties` object
/// (or JSON text of one) have it merged into the top level; the top-level
/// keys win. Anything that does not normalize to an object is empty.
pub fn normalize_properties(value: &Value) -> Map<String, Value> {
    let Value::Object(normalized) = normalize_value(value) else {
        return Map::new();
    };
    match normalized.get("properties") {
        Some(Value::Object(nested)) => {
            let mut flat = nested.clone();
            for (key, item) in normalized {
                if key != "properties" {
                    flat.insert(key, item);
                }
            }
            flat
        }
        _ => normalized,
    }
}

/// `_scalar_text`: the text a property value is indexed and compared under.
/// `true`, `false` and `null` as JSON writes them, objects and arrays as
/// [`json_key`], anything else as Python's `str()`.
pub fn scalar_text(value: &Value) -> String {
    match value {
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Null => "null".to_string(),
        Value::Object(_) | Value::Array(_) => json_key(value),
        other => pyjson::py_str(other),
    }
}

/// Python's `str(value)` of a node field read with a default
/// (`str(node.get(key, default))`): the default when the key is missing,
/// `"None"` for a stored null.
pub(crate) fn py_str_or(value: Option<&Value>, default: &str) -> String {
    match value {
        None => default.to_string(),
        Some(value) => pyjson::py_str(value),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn json_text_is_parsed_recursively() {
        let value = json!({
            "a": " {\"b\": \"[1, 2]\"} ",
            "c": "[not json",
            "d": "plain",
            "e": [ "{\"x\": null}" ],
        });
        assert_eq!(
            normalize_value(&value),
            json!({"a": {"b": [1, 2]}, "c": "[not json", "d": "plain", "e": [{"x": null}]})
        );
    }

    #[test]
    fn nested_properties_are_flattened_and_top_level_wins() {
        let value = json!({
            "id": "n1",
            "name": "top",
            "properties": "{\"name\": \"nested\", \"file_path\": \"a.py\"}",
        });
        let flat = normalize_properties(&value);
        assert_eq!(
            Value::Object(flat),
            json!({"id": "n1", "name": "top", "file_path": "a.py"})
        );
        assert!(normalize_properties(&json!("text")).is_empty());
        assert!(normalize_properties(&json!(null)).is_empty());
    }

    #[test]
    fn scalar_text_matches_python() {
        assert_eq!(scalar_text(&json!(true)), "true");
        assert_eq!(scalar_text(&json!(null)), "null");
        assert_eq!(scalar_text(&json!(12)), "12");
        assert_eq!(scalar_text(&json!(1.0)), "1.0");
        assert_eq!(scalar_text(&json!("POST")), "POST");
        assert_eq!(
            scalar_text(&json!({"b": 1, "a": "é"})),
            r#"{"a":"é","b":1}"#
        );
        assert_eq!(scalar_text(&json!(["get"])), r#"["get"]"#);
    }
}
