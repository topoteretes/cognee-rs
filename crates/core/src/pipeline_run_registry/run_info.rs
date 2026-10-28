//! The `pipeline_runs.run_info` value each lifecycle row carries.
//!
//! Python stopped storing the run's data items in `run_info` in `a88402500`
//! (COG-6089): its writers (`log_pipeline_run_start`, `log_pipeline_run_complete`,
//! `log_pipeline_run_error`) write only the run's own bookkeeping, and alembic
//! `b7e4c2a9d6f1` strips the old `data` key from stored rows. Rust follows:
//!
//! | status      | Python                                          | Rust      |
//! |-------------|-------------------------------------------------|-----------|
//! | `STARTED`   | `{}`, or `{"recovery_lock": …}` (+ `progress`)  | `{}`      |
//! | `COMPLETED` | `{}`                                            | `{}`      |
//! | `ERRORED`   | `{"error": …}`                                  | `{"error": …}` |
//!
//! Rust has no recovery lock and no in-row progress ticks (task progress is
//! published through the progress token and the payload-field table instead),
//! so its `STARTED` row is `{}`. Rust-only additions are additive keys such as
//! cognify's `cognify_failures` on a tolerantly completed run.
//!
//! Python writes no `INITIATED` row at all since `dc0b4ad6a`; neither does any
//! Rust pipeline run. [`run_info_for_initiated`] remains only for the explicit,
//! opt-in re-arm API (`cognee::reset_pipeline_run_status`) that pairs with the
//! opt-in pipeline cache.

use serde_json::{Map, Value};

/// `run_info` for the `STARTED` row: `{}`.
pub fn run_info_for_started() -> Value {
    Value::Object(Map::new())
}

/// `run_info` for the `COMPLETED` row: `{}`, as Python's
/// `log_pipeline_run_complete` writes.
pub fn run_info_for_completed() -> Value {
    Value::Object(Map::new())
}

/// `run_info` for the `ERRORED` row: `{"error": <message>}`, as Python's
/// `log_pipeline_run_error` writes.
pub fn run_info_for_errored(error: &str) -> Value {
    let mut m = Map::with_capacity(1);
    m.insert("error".into(), Value::String(error.to_string()));
    Value::Object(m)
}

/// `run_info` for an `INITIATED` row: `{}`.
///
/// No pipeline run writes this status. Only the explicit re-arm API does.
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

    #[test]
    fn started_run_info_is_empty_object() {
        assert_eq!(run_info_for_started().to_string(), "{}");
    }

    #[test]
    fn completed_run_info_is_empty_object() {
        assert_eq!(run_info_for_completed().to_string(), "{}");
    }

    #[test]
    fn errored_run_info_carries_only_the_error() {
        assert_eq!(
            run_info_for_errored("boom").to_string(),
            "{\"error\":\"boom\"}"
        );
    }

    /// Python's `test_run_info_has_no_data.py`: every row holds only run
    /// bookkeeping, never the data items.
    #[test]
    fn no_row_carries_a_data_key() {
        for info in [
            run_info_for_started(),
            run_info_for_completed(),
            run_info_for_errored("boom"),
            run_info_for_initiated(),
        ] {
            assert!(info.get("data").is_none(), "{info}");
        }
    }
}
