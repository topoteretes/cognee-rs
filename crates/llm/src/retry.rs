//! Shared retry backoff with jitter for transient LLM API failures.
//!
//! The adapters retry transient network / HTTP 429 / 5xx failures with
//! exponential backoff. A purely deterministic schedule (1s, 2s, 4s, …) means a
//! batch of requests that all hit a rate limit at the same instant also retry at
//! the same instants — a thundering herd that keeps tripping the limit. Adding
//! jitter spreads those retries out. See issue #19.
//!
//! The capped-exponential base is computed by [`cognee_utils::retry::RetryConfig`]
//! so the backoff math has a single source of truth shared with the rest of the
//! workspace; this module only layers **equal jitter** on top.

use std::time::Duration;

use cognee_utils::retry::RetryConfig;

/// Capped exponential base (1s, 2s, 4s, … capped at 30s) for a 1-indexed
/// `attempt`, delegated to the shared `cognee_utils` implementation.
fn base_backoff_ms(attempt: u32) -> u64 {
    // `RetryConfig::calculate_delay` is 0-indexed and, with no jitter factor,
    // returns `initial_delay_ms * multiplier^attempt` capped at `max_delay_ms`.
    // 1000ms initial, 2x multiplier, 30s cap matches the previous local math.
    RetryConfig::new(0, 1_000, 30_000)
        .calculate_delay(attempt.saturating_sub(1))
        .as_millis() as u64
}

/// Exponential backoff with **equal jitter** for retry `attempt` (1-indexed).
///
/// Returns a duration in `[base/2, base]`, where `base` is the capped
/// exponential backoff. Keeping at least half the backoff preserves the growing
/// delay, while the random half spreads simultaneous retries to avoid a
/// thundering herd (e.g. a batch that all hit HTTP 429 at once).
///
/// `attempt` is 1-indexed (the first retry is attempt 1); callers guard on
/// `attempt > 0`.
pub(crate) fn retry_backoff(attempt: u32) -> Duration {
    debug_assert!(
        attempt >= 1,
        "retry_backoff expects a 1-indexed attempt >= 1"
    );
    let base = base_backoff_ms(attempt);
    let half = base / 2;
    let jitter = if half == 0 {
        0
    } else {
        rand::random::<u64>() % (half + 1)
    };
    Duration::from_millis(half + jitter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_stays_within_equal_jitter_bounds() {
        for attempt in 1..=8u32 {
            let base = base_backoff_ms(attempt);
            for _ in 0..200 {
                let ms = retry_backoff(attempt).as_millis() as u64;
                assert!(ms >= base / 2, "attempt {attempt}: {ms} < {}", base / 2);
                assert!(ms <= base, "attempt {attempt}: {ms} > {base}");
            }
        }
    }

    #[test]
    fn base_matches_capped_exponential_schedule() {
        // Delegated to cognee_utils but must still produce the 1s/2s/4s… schedule
        // capped at 30s.
        assert_eq!(base_backoff_ms(1), 1_000);
        assert_eq!(base_backoff_ms(2), 2_000);
        assert_eq!(base_backoff_ms(3), 4_000);
        assert_eq!(base_backoff_ms(6), 30_000); // 32s would exceed the cap
    }

    #[test]
    fn backoff_never_exceeds_30s_cap() {
        for _ in 0..200 {
            assert!(retry_backoff(100).as_millis() as u64 <= 30_000);
        }
    }

    #[test]
    fn backoff_is_randomized() {
        // Over many samples at a fixed attempt we should see more than one value
        // (otherwise jitter is not being applied).
        let distinct: std::collections::HashSet<u64> = (0..50)
            .map(|_| retry_backoff(4).as_millis() as u64)
            .collect();
        assert!(distinct.len() > 1, "expected jittered (varied) delays");
    }
}
