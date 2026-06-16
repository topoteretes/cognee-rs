//! Process-global router-override counters.
//!
//! Ports `override_counts` / `record_override()` from
//! `cognee/api/v1/recall/query_router.py` (lines 152–168).
//!
//! When a caller passes an explicit `query_type` to `recall()` while
//! `auto_route=True`, the router still runs so we can compare its choice
//! against the user's override. Mismatches are accumulated in this
//! process-global counter so the router's systematic misroutings can be
//! surfaced in telemetry / diagnostics.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::types::SearchType;

static OVERRIDE_COUNTS: OnceLock<Mutex<HashMap<(SearchType, SearchType), u64>>> = OnceLock::new();

fn counts() -> &'static Mutex<HashMap<(SearchType, SearchType), u64>> {
    OVERRIDE_COUNTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record that the router picked `routed` while the user chose `override_type`.
///
/// When `routed == override_type` this is a no-op (the router agreed with the
/// user, so there is nothing to surface). Otherwise the `(routed, override)`
/// pair's counter is bumped by one and an info-level log record is emitted.
pub fn record_override(routed: SearchType, override_type: SearchType) {
    if routed == override_type {
        return;
    }
    // lock poison is unrecoverable
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    let mut guard = counts().lock().unwrap();
    let key = (routed, override_type);
    let entry = guard.entry(key).or_insert(0);
    *entry += 1;
    tracing::info!(
        routed = ?routed,
        user_chose = ?override_type,
        total = *entry,
        "Router override recorded"
    );
}

/// Return a clone of the current override counters.
///
/// Intended primarily for diagnostics and tests; the underlying storage is
/// short-lived (process-global, unbounded) so snapshots are cheap to take.
pub fn override_counts_snapshot() -> HashMap<(SearchType, SearchType), u64> {
    // lock poison is unrecoverable
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    counts().lock().unwrap().clone()
}

/// Clear all recorded overrides. Primarily used by tests that need a clean
/// process-global state before asserting specific counts.
pub fn clear_override_counts() {
    // lock poison is unrecoverable
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    counts().lock().unwrap().clear();
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
    #[serial_test::serial]
    fn record_override_increments() {
        clear_override_counts();
        record_override(SearchType::GraphCompletion, SearchType::Temporal);
        record_override(SearchType::GraphCompletion, SearchType::Temporal);
        let snap = override_counts_snapshot();
        assert_eq!(
            snap.get(&(SearchType::GraphCompletion, SearchType::Temporal))
                .copied(),
            Some(2)
        );
    }

    #[test]
    #[serial_test::serial]
    fn same_type_not_recorded() {
        clear_override_counts();
        record_override(SearchType::Temporal, SearchType::Temporal);
        assert!(override_counts_snapshot().is_empty());
    }
}
