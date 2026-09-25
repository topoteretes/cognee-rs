//! Payload / display / id helpers and the node-set filter predicate.
//!
//! Port of `cognee/modules/retrieval/hybrid/results.py`. Python's duck-typed
//! `Any` collapses to [`SearchItem`] here: every hybrid lane (chunk vector,
//! summary, entity, edge) is normalized to a [`SearchItem`] at its call site, so these helpers
//! operate on `&SearchItem` / `&serde_json::Value` directly.

use std::collections::HashSet;

use serde_json::Value;

use crate::types::SearchItem;

/// Return the item's JSON payload.
///
/// Port of `results.py:5-9`. Python defensively falls back to `{}` for non-dict
/// input; Rust's [`SearchItem`] always carries a `Value`, so this is a thin
/// accessor. Callers that need to iterate keys use `Value::get`, which already
/// returns `None` for non-object values.
pub(crate) fn payload(item: &SearchItem) -> &Value {
    &item.payload
}

/// Render a scalar JSON value as a trimmed display string, or `None`.
///
/// Port of `results.py:12-18`: `Null` → `None`; `String`/`Number`/`Bool` →
/// trimmed string (empty → `None`); `Array`/`Object` → `None`. Python's bare
/// `UUID` branch is unreachable here — ids are always JSON strings by the time
/// they reach a payload — so it is omitted.
pub(crate) fn display_value(value: &Value) -> Option<String> {
    let text = match value {
        Value::Null => return None,
        Value::String(s) => s.trim().to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => return None,
    };
    if text.is_empty() { None } else { Some(text) }
}

/// Resolve the display id for an item: the payload's `"id"` when present and
/// renderable, else the `SearchItem::id` field.
///
/// Port of `results.py:21-23`.
pub(crate) fn result_id(item: &SearchItem) -> Option<String> {
    payload(item)
        .get("id")
        .and_then(display_value)
        .or_else(|| item.id.map(|id| id.to_string()))
}

/// Whether `result_payload` satisfies the requested node-set filter.
///
/// Port of `results.py:35-51`. **Divergence from Python:** Rust's
/// `belongs_to_set` entries are either bare strings (dataset-membership form
/// emitted by `DataPoint::new`) or `NodeSet` objects
/// (`{"id","name","type":"NodeSet"}`, emitted by `classify_documents`), so the
/// entry→name step reads a string entry as-is, reads `.name` off an object entry
/// (skipping it if absent/non-string), and skips anything else. `"AND"` requires
/// `requested ⊆ payload_sets`; anything else (including garbage) is OR — a
/// non-empty intersection — matching Python's operator handling exactly.
pub(crate) fn payload_matches_node_filter(
    result_payload: &Value,
    node_name: Option<&[String]>,
    node_name_filter_operator: &str,
) -> bool {
    let requested: &[String] = match node_name {
        Some(names) if !names.is_empty() => names,
        _ => return true,
    };

    let Some(Value::Array(entries)) = result_payload.get("belongs_to_set") else {
        return false;
    };

    let payload_sets: HashSet<&str> = entries
        .iter()
        .filter_map(|entry| match entry {
            Value::String(name) => Some(name.as_str()),
            Value::Object(_) => entry.get("name").and_then(Value::as_str),
            _ => None,
        })
        .collect();

    let requested_sets: HashSet<&str> = requested.iter().map(String::as_str).collect();

    if node_name_filter_operator == "AND" {
        requested_sets.is_subset(&payload_sets)
    } else {
        !payload_sets.is_disjoint(&requested_sets)
    }
}

/// First renderable display string among `values`, or `None`.
///
/// Port of `results.py:54-59`. Consumed by the entity/facts lane (P1-08).
pub(crate) fn first_display_value(values: &[&Value]) -> Option<String> {
    values.iter().find_map(|value| display_value(value))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::*;

    fn item(payload: Value, id: Option<Uuid>) -> SearchItem {
        SearchItem {
            id,
            score: None,
            payload,
        }
    }

    #[test]
    fn display_value_covers_each_variant() {
        assert_eq!(display_value(&Value::Null), None);
        assert_eq!(display_value(&json!("hello")), Some("hello".to_string()));
        assert_eq!(
            display_value(&json!("  spaced  ")),
            Some("spaced".to_string())
        );
        assert_eq!(display_value(&json!("   ")), None);
        assert_eq!(display_value(&json!("")), None);
        assert_eq!(display_value(&json!(42)), Some("42".to_string()));
        assert_eq!(display_value(&json!(1.5)), Some("1.5".to_string()));
        assert_eq!(display_value(&json!(true)), Some("true".to_string()));
        assert_eq!(display_value(&json!([1, 2, 3])), None);
        assert_eq!(display_value(&json!({"a": 1})), None);
    }

    #[test]
    fn first_display_value_returns_first_renderable() {
        let null = Value::Null;
        let empty = json!("  ");
        let good = json!("name");
        let other = json!("second");
        let values: Vec<&Value> = vec![&null, &empty, &good, &other];
        assert_eq!(first_display_value(&values), Some("name".to_string()));
        assert_eq!(first_display_value(&[&null, &empty]), None);
    }

    #[test]
    fn result_id_prefers_payload_then_falls_back() {
        let id = Uuid::new_v4();
        let with_payload_id = item(json!({"id": "payload-id"}), Some(id));
        assert_eq!(result_id(&with_payload_id), Some("payload-id".to_string()));

        let no_payload_id = item(json!({"text": "x"}), Some(id));
        assert_eq!(result_id(&no_payload_id), Some(id.to_string()));

        let neither = item(json!({"text": "x"}), None);
        assert_eq!(result_id(&neither), None);
    }

    #[test]
    fn node_filter_none_or_empty_always_matches() {
        let payload = json!({"belongs_to_set": ["a"]});
        assert!(payload_matches_node_filter(&payload, None, "OR"));
        assert!(payload_matches_node_filter(&payload, Some(&[]), "AND"));
    }

    #[test]
    fn node_filter_missing_set_fails_when_requested() {
        let payload = json!({"text": "x"});
        assert!(!payload_matches_node_filter(
            &payload,
            Some(&["a".to_string()]),
            "OR"
        ));
    }

    #[test]
    fn node_filter_bare_string_entries() {
        let payload = json!({"belongs_to_set": ["alpha", "beta"]});
        assert!(payload_matches_node_filter(
            &payload,
            Some(&["beta".to_string()]),
            "OR"
        ));
        assert!(!payload_matches_node_filter(
            &payload,
            Some(&["gamma".to_string()]),
            "OR"
        ));
    }

    #[test]
    fn node_filter_object_entries_read_name() {
        let payload = json!({
            "belongs_to_set": [
                {"id": "1", "name": "alpha", "type": "NodeSet"},
                {"id": "2", "name": "beta", "type": "NodeSet"}
            ]
        });
        assert!(payload_matches_node_filter(
            &payload,
            Some(&["alpha".to_string()]),
            "OR"
        ));
        // An object entry without a usable `name` is skipped.
        let no_name = json!({"belongs_to_set": [{"id": "1", "type": "NodeSet"}]});
        assert!(!payload_matches_node_filter(
            &no_name,
            Some(&["1".to_string()]),
            "OR"
        ));
    }

    #[test]
    fn node_filter_mixed_entries() {
        let payload = json!({
            "belongs_to_set": [
                "alpha",
                {"id": "2", "name": "beta", "type": "NodeSet"},
                42
            ]
        });
        assert!(payload_matches_node_filter(
            &payload,
            Some(&["beta".to_string()]),
            "OR"
        ));
        assert!(payload_matches_node_filter(
            &payload,
            Some(&["alpha".to_string()]),
            "OR"
        ));
    }

    #[test]
    fn node_filter_and_requires_subset() {
        let payload = json!({"belongs_to_set": ["alpha", "beta", "gamma"]});
        assert!(payload_matches_node_filter(
            &payload,
            Some(&["alpha".to_string(), "beta".to_string()]),
            "AND"
        ));
        assert!(!payload_matches_node_filter(
            &payload,
            Some(&["alpha".to_string(), "delta".to_string()]),
            "AND"
        ));
    }

    #[test]
    fn node_filter_or_requires_overlap() {
        let payload = json!({"belongs_to_set": ["alpha", "beta"]});
        // Any non-"AND" operator (incl. garbage) behaves as OR.
        assert!(payload_matches_node_filter(
            &payload,
            Some(&["beta".to_string(), "delta".to_string()]),
            "or"
        ));
        assert!(!payload_matches_node_filter(
            &payload,
            Some(&["delta".to_string()]),
            "garbage"
        ));
    }
}
