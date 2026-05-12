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

use serde_json::Value;
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

#[cfg(test)]
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
}
