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
//! The `STARTED` row carries `{"recovery_lock": "<token>"}` when the run holds
//! an ownership marker ([`crate::RunOwnership`]), as in Python, and `{}`
//! otherwise. Rust has no in-row progress ticks (task progress is published
//! through the progress token and the payload-field table instead). Rust-only
//! additions are additive keys such as cognify's `cognify_failures` on a
//! tolerantly completed run.
//!
//! Python writes no `INITIATED` row at all since `dc0b4ad6a`; neither does any
//! Rust pipeline run. Old rows that carry that status are still read.

use serde_json::{Map, Value};
use uuid::Uuid;

/// `run_info` for the `STARTED` row: `{"recovery_lock": <token>}`, or `{}` for
/// a run with no ownership marker, as Python's `log_pipeline_run_start` writes.
pub fn run_info_for_started(recovery_lock: Option<Uuid>) -> Value {
    let mut m = Map::new();
    if let Some(token) = recovery_lock {
        m.insert("recovery_lock".into(), Value::String(token.to_string()));
    }
    Value::Object(m)
}

/// The ownership token a `STARTED` row's `run_info` carries, if any.
pub fn recovery_lock_of(run_info: Option<&Value>) -> Option<Uuid> {
    run_info?
        .get("recovery_lock")?
        .as_str()
        .and_then(|raw| Uuid::parse_str(raw).ok())
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
        assert_eq!(run_info_for_started(None).to_string(), "{}");
    }

    /// Python's `log_pipeline_run_start(recovery_token=…)`.
    #[test]
    fn started_run_info_carries_the_recovery_lock() {
        let token = Uuid::new_v4();
        let info = run_info_for_started(Some(token));
        assert_eq!(
            info.to_string(),
            format!("{{\"recovery_lock\":\"{token}\"}}")
        );
        assert_eq!(recovery_lock_of(Some(&info)), Some(token));
        assert_eq!(recovery_lock_of(Some(&run_info_for_started(None))), None);
        assert_eq!(recovery_lock_of(None), None);
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
            run_info_for_started(None),
            run_info_for_started(Some(Uuid::new_v4())),
            run_info_for_completed(),
            run_info_for_errored("boom"),
        ] {
            assert!(info.get("data").is_none(), "{info}");
        }
    }
}
