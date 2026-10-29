//! The marker for graph nodes that must never be surfaced in retrieval output.
//!
//! Port of Python's `InternalDataPoint` (`cognee/infrastructure/engine/models/
//! InternalDataPoint.py`). Internal nodes hold per-user or system state inside
//! the shared graph — in Python, `UserPreference` — and are recognized by an
//! `is_internal` property on the raw node, never by type name. Rust writes
//! them too ([`crate::UserPreference`]), so the read paths that Python filters
//! must filter here as well.

use serde_json::{Map, Value};

/// The node property that marks an internal node (`INTERNAL_PROPERTY`).
pub const INTERNAL_PROPERTY: &str = "is_internal";

/// True for a raw graph-node property map that must not reach a user.
///
/// Python's `is_internal_node` is `bool(props.get("is_internal"))`, so this
/// follows Python truthiness: `false`, `0`, `""`, `[]`, `{}` and `null` are
/// not internal.
pub fn is_internal_node(properties: &Map<String, Value>) -> bool {
    marks_internal(properties.get(INTERNAL_PROPERTY))
}

/// True when a node's `is_internal` property value (absent: `None`) marks it
/// internal — [`is_internal_node`] for property maps of any shape.
pub fn marks_internal(value: Option<&Value>) -> bool {
    value.is_some_and(is_truthy)
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;

    fn props(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn follows_python_truthiness() {
        assert!(is_internal_node(&props(json!({ "is_internal": true }))));
        assert!(is_internal_node(&props(json!({ "is_internal": 1 }))));
        assert!(!is_internal_node(&props(json!({ "is_internal": false }))));
        assert!(!is_internal_node(&props(json!({ "is_internal": null }))));
        assert!(!is_internal_node(&props(json!({ "is_internal": 0 }))));
        assert!(!is_internal_node(&props(json!({ "name": "Alice" }))));
    }
}
