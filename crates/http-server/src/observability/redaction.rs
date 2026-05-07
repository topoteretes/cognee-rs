//! JSON-walking secret redaction for the observability HTTP API.
//!
//! The single-string `redact()` helper now lives in
//! `cognee_utils::redact` so adapter crates can reach it without
//! depending on the http-server. This module keeps only the JSON
//! object walker.

use std::borrow::Cow;

use cognee_utils::redact::redact;

/// Walk a JSON object and redact any string-leaf values in place.
///
/// Object *keys* are intentionally left alone (matches Python).
pub fn redact_attributes(attrs: &mut serde_json::Map<String, serde_json::Value>) {
    for (_k, v) in attrs.iter_mut() {
        redact_value(v);
    }
}

fn redact_value(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::String(s) => {
            if let Cow::Owned(replaced) = redact(s) {
                *s = replaced;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_value(item);
            }
        }
        serde_json::Value::Object(map) => {
            for (_k, vv) in map.iter_mut() {
                redact_value(vv);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_object_redacted_in_place() {
        let mut value = json!({
            "headers": {
                "Authorization": "Bearer eyJabc.def.ghi-very-long-jwt-1234567890",
                "X-Other": "fine"
            },
            "key_unchanged": "value"
        });
        if let serde_json::Value::Object(map) = &mut value {
            redact_attributes(map);
        }
        let auth = value["headers"]["Authorization"].as_str().unwrap_or("");
        assert!(auth.contains("***REDACTED***"));
        assert!(!auth.contains("ghi-very-long-jwt"));
        assert_eq!(value["headers"]["X-Other"], "fine");
        assert_eq!(value["key_unchanged"], "value");
    }
}
