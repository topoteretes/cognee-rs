//! Secret redaction for span attributes.
//!
//! Mirrors Python's `redact_secrets`
//! (`cognee/modules/observability/tracing.py`): four regex patterns covering
//! OpenAI-style keys, generic `api_key=`/`api-key=`, `Bearer <token>`, and
//! `password=`. On match we keep the first 6 characters of the value and
//! replace the rest with `***REDACTED***` so the original prefix remains
//! visible for debugging.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

/// Build the four secret patterns. Compilation is wrapped in `OnceLock` so we
/// pay the cost once.
fn patterns() -> &'static [Regex] {
    static SECRET_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    SECRET_PATTERNS
        .get_or_init(|| {
            vec![
                Regex::new(r"sk-[A-Za-z0-9]{20,}")
                    .expect("openai-key pattern compiles (build-time guarantee)"),
                Regex::new(r#"(?i)(api[_-]?key\s*[=:]\s*)['"]?[A-Za-z0-9\-_]{16,}['"]?"#)
                    .expect("api-key pattern compiles (build-time guarantee)"),
                Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9\-_\.]{20,}")
                    .expect("bearer pattern compiles (build-time guarantee)"),
                Regex::new(r#"(?i)(password\s*[=:]\s*)['"]?[^\s'"]{8,}['"]?"#)
                    .expect("password pattern compiles (build-time guarantee)"),
            ]
        })
        .as_slice()
}

/// Replace any secret matches in `value` with the redacted form.
///
/// On a hit we keep the first six characters of the *match* (not of the entire
/// string) and append `***REDACTED***`, e.g.
/// `"sk-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"` → `"sk-AAA***REDACTED***"`.
///
/// Returns `Cow::Borrowed` when nothing matched so the common path is
/// allocation-free.
pub fn redact(value: &str) -> Cow<'_, str> {
    let mut current: Cow<'_, str> = Cow::Borrowed(value);
    for re in patterns() {
        let scratch = current.clone();
        let replaced = re.replace_all(&scratch, |caps: &regex::Captures<'_>| {
            let m = caps.get(0).map(|m| m.as_str()).unwrap_or("");
            let prefix_end = m
                .char_indices()
                .nth(6)
                .map(|(idx, _)| idx)
                .unwrap_or(m.len());
            format!("{}***REDACTED***", &m[..prefix_end])
        });
        match replaced {
            Cow::Borrowed(_) => {
                // No matches in this pass — keep `current` as-is.
            }
            Cow::Owned(s) => {
                current = Cow::Owned(s);
            }
        }
    }
    current
}

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
    fn redacts_openai_key() {
        let out = redact("token=sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ12345");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("ABCDEFGHIJKL"));
        assert!(out.contains("sk-ABC"));
    }

    #[test]
    fn redacts_api_key_assignment() {
        let out = redact("config: api_key=ABCDEFGHIJKLMNOP");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("LMNOP"));
    }

    #[test]
    fn redacts_bearer_token() {
        let out = redact("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("payload"));
    }

    #[test]
    fn redacts_password_assignment() {
        let out = redact("password=hunter2hunter2");
        assert!(out.contains("***REDACTED***"));
        assert!(!out.contains("hunter2hunter2"));
    }

    #[test]
    fn inert_string_passes_through_borrowed() {
        let s = "this is a normal string with no secrets";
        let out = redact(s);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(&*out, s);
    }

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
        // Non-secret leaves untouched.
        assert_eq!(value["headers"]["X-Other"], "fine");
        assert_eq!(value["key_unchanged"], "value");
    }

    #[test]
    fn multiple_secrets_in_one_string_all_redacted() {
        let out = redact(
            "sk-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA Bearer eyJabc.def.ghi-very-long-jwt-1234567890",
        );
        // Both prefixes still visible, both suffixes redacted.
        assert!(out.contains("sk-AAA***REDACTED***"));
        assert!(out.contains("***REDACTED***"));
    }

    #[test]
    fn pattern_module_loads_without_panic() {
        // Touches the OnceLock initializer — guards against a regex typo
        // landing without a compile-time signal.
        let _ = patterns();
    }
}
