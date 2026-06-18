//! Python-parity `data_info` helper for `pipeline_runs.run_info`.
//!
//! Python writes `run_info["data"]` as one of three values:
//!   - `"None"` (literal string) when the input is empty / falsy
//!   - `[str(item.id) for item in data]` for `list[Data]`
//!   - `str(data)` (i.e. `repr()`) for anything else
//!
//! Rust only sees typed `&[Uuid]` inputs from typed call sites, so the
//! third branch is unreachable — the carrier is always either empty
//! (same wire shape as Python's `if not data: data_info = "None"`) or a
//! list of UUIDs.

use serde_json::{Map, Value};
use uuid::Uuid;

/// Build the value Python writes under `run_info["data"]`.
///
/// - Empty slice → `Value::String("None".into())` to match Python's
///   `data_info = "None"` branch.
/// - Non-empty slice → JSON array of hyphenated UUID strings
///   (`Uuid::to_string()`, which matches Python's `str(uuid.UUID(...))`).
///
/// The returned value is intended to be inserted as `run_info["data"]`,
/// not as the entire `run_info` document.
//
// Python's third branch (`else: data_info = str(data)`) is unreachable
// here: the Rust typed signature only accepts a slice of `Uuid`, so the
// only way to land in the "empty" path is an empty slice — which already
// maps to `"None"`. No fallback branch is needed.
pub fn data_info(data_ids: &[Uuid]) -> Value {
    if data_ids.is_empty() {
        Value::String("None".into())
    } else {
        Value::Array(
            data_ids
                .iter()
                .map(|id| Value::String(id.to_string()))
                .collect(),
        )
    }
}

/// Build `run_info` for the `STARTED` / `COMPLETED` rows.
///
/// Matches Python (`log_pipeline_run_start.py` / `log_pipeline_run_complete.py`):
///
/// ```text
/// run_info = {"data": data_info(data)}
/// ```
pub fn run_info_for_running(data_ids: &[Uuid]) -> Value {
    let mut m = Map::with_capacity(1);
    m.insert("data".into(), data_info(data_ids));
    Value::Object(m)
}

/// Build `run_info` for the `ERRORED` row.
///
/// Matches Python (`log_pipeline_run_error.py`):
///
/// ```text
/// run_info = {"data": data_info(data), "error": str(e)}
/// ```
///
/// `serde_json::Map` preserves insertion order, so `data` always precedes
/// `error` on the wire — required for byte-identical parity with Python.
pub fn run_info_for_errored(data_ids: &[Uuid], error: &str) -> Value {
    let mut m = Map::with_capacity(2);
    m.insert("data".into(), data_info(data_ids));
    m.insert("error".into(), Value::String(error.to_string()));
    Value::Object(m)
}

/// Build `run_info` for the `INITIATED` row.
///
/// Matches Python (`log_pipeline_run_initiated.py`):
///
/// ```text
/// run_info = {}
/// ```
///
/// Reserved for task 08-04 — currently exported and unit-tested but no
/// production caller invokes it.
pub fn run_info_for_initiated() -> Value {
    Value::Object(Map::new())
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

    #[test]
    fn empty_emits_string_none() {
        assert_eq!(data_info(&[]), json!("None"));
    }

    #[test]
    fn single_id_emits_one_element_array() {
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        assert_eq!(
            data_info(&[id]),
            json!(["00000000-0000-0000-0000-000000000001"])
        );
    }

    #[test]
    fn three_ids_preserve_order() {
        let id1 = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let id2 = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
        let id3 = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
        assert_eq!(
            data_info(&[id1, id2, id3]),
            json!([
                "11111111-1111-1111-1111-111111111111",
                "22222222-2222-2222-2222-222222222222",
                "33333333-3333-3333-3333-333333333333",
            ])
        );
    }

    // ── run_info_for_running ─────────────────────────────────────────

    #[test]
    fn running_run_info_with_empty_data_emits_none_literal() {
        let v = run_info_for_running(&[]);
        assert_eq!(v.to_string(), "{\"data\":\"None\"}");
    }

    #[test]
    fn running_run_info_with_single_id_matches_python_shape() {
        let id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("valid uuid literal");
        let v = run_info_for_running(&[id]);
        assert_eq!(
            v.to_string(),
            "{\"data\":[\"00000000-0000-0000-0000-000000000001\"]}"
        );
    }

    // ── run_info_for_errored ─────────────────────────────────────────

    #[test]
    fn errored_run_info_with_empty_data_includes_error() {
        let v = run_info_for_errored(&[], "boom");
        // `data` precedes `error` because Map preserves insertion order.
        assert_eq!(v.to_string(), "{\"data\":\"None\",\"error\":\"boom\"}");
    }

    #[test]
    fn errored_run_info_includes_data_and_error() {
        let id =
            Uuid::parse_str("00000000-0000-0000-0000-000000000002").expect("valid uuid literal");
        let v = run_info_for_errored(&[id], "boom");
        let obj = v.as_object().expect("object");
        let data = obj.get("data").expect("data key");
        assert_eq!(data.as_array().expect("array").len(), 1);
        assert_eq!(obj.get("error").and_then(Value::as_str), Some("boom"));
        // Key order on the wire: `data` first, then `error`.
        let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["data", "error"]);
    }

    // ── run_info_for_initiated ───────────────────────────────────────

    #[test]
    fn initiated_run_info_is_empty_object() {
        let v = run_info_for_initiated();
        assert_eq!(v.to_string(), "{}");
        assert!(v.as_object().expect("object").is_empty());
    }
}
