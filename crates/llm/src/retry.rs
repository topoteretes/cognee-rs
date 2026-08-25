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
use reqwest::header::HeaderMap;

/// First-retry backoff, matching Python's `wait_exponential_jitter(8, ...)`.
const INITIAL_BACKOFF_MS: u64 = 8_000;
/// Backoff ceiling, matching Python's `wait_exponential_jitter(..., 128)`.
const MAX_BACKOFF_MS: u64 = 128_000;
/// Upper bound on a `Retry-After` we will act on. A hint above this is ignored
/// rather than clamped, matching the OpenAI SDK's `0 < retry_after <= 60` guard
/// (`openai/_base_client.py:764`): a provider asking us to sleep for an hour is
/// not giving usable guidance, and clamping it to a minute would obey neither
/// the provider nor our own ladder.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Capped exponential base (8s, 16s, 32s, … capped at 128s) for a 1-indexed
/// `attempt`, delegated to the shared `cognee_utils` implementation.
///
/// The 8s-to-128s curve matches Python cognee's `wait_exponential_jitter(8, 128)`
/// (`cognee/infrastructure/llm/retry_config.py`). The previous 1s-to-30s curve
/// gave up long before a provider rate-limit window had reset.
fn base_backoff_ms(attempt: u32) -> u64 {
    // `RetryConfig::calculate_delay` is 0-indexed and, with no jitter factor,
    // returns `initial_delay_ms * multiplier^attempt` capped at `max_delay_ms`.
    RetryConfig::new(0, INITIAL_BACKOFF_MS, MAX_BACKOFF_MS)
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

/// When a retry loop is allowed to give up.
///
/// Python's stop condition is `stop_after_attempt(2) & stop_after_delay(240)`
/// (`cognee/infrastructure/llm/retry_config.py`) — note the `&`. Both are
/// *floors*: retrying continues until the attempt count **and** the elapsed
/// time have both been satisfied. The time floor is what carries a call through
/// a provider rate-limit window; an attempt count alone gives up in seconds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryBudget {
    /// Minimum attempts before giving up is permitted.
    min_attempts: u32,
    /// Minimum elapsed time before giving up is permitted. `ZERO` reduces the
    /// budget to a plain attempt cap, which is the documented `0` escape hatch
    /// for `LLM_MIN_RETRY_SECONDS`.
    min_elapsed: Duration,
}

impl RetryBudget {
    pub(crate) fn new(min_attempts: u32, min_elapsed: Duration) -> Self {
        Self {
            min_attempts: min_attempts.max(1),
            min_elapsed,
        }
    }

    /// Whether the loop may stop, having made `attempts_made` attempts over
    /// `elapsed`.
    pub(crate) fn is_exhausted(&self, attempts_made: u32, elapsed: Duration) -> bool {
        attempts_made >= self.min_attempts && elapsed >= self.min_elapsed
    }
}

/// Provider wordings that mean "this will never succeed" rather than "slow
/// down". They arrive as HTTP 429 alongside genuine per-minute rate limits, so
/// the status code alone cannot tell them apart.
///
/// Ported verbatim from `_TERMINAL_QUOTA_PATTERNS` in Python's `retry_config.py`,
/// including its deliberate omission: the bare phrase "exceeded your current
/// quota" is *not* listed, because Gemini's free tier uses it for a recoverable
/// limit. Keep this list narrow for that reason.
const TERMINAL_QUOTA_PATTERNS: [&str; 5] = [
    "insufficient_quota",
    "quota_exceeded",
    "billing hard limit",
    "credit balance is too low",
    "out of credits",
];

/// Whether an error body reports exhausted quota or billing, which no amount of
/// retrying can fix.
pub(crate) fn is_quota_or_billing_error(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    TERMINAL_QUOTA_PATTERNS
        .iter()
        .any(|pattern| lowered.contains(pattern))
}

/// The overload reason for a status code, if it is one.
///
/// 429 rate limited, 503 service unavailable (e.g. Ollama's queue-full reply),
/// 529 Anthropic overloaded — the same set as `_OVERLOAD_STATUS_CODES` in
/// Python's `overload_policy.py`.
pub(crate) fn overload_reason(status: u16) -> Option<&'static str> {
    match status {
        429 => Some("http_429"),
        503 => Some("http_503"),
        529 => Some("http_529"),
        _ => None,
    }
}

/// Parse a provider `Retry-After` hint into the delay to actually use.
///
/// When present and usable the hint **replaces** the computed backoff outright,
/// including when it asks for less — the provider knows when its window resets
/// and we do not. That is the OpenAI SDK's rule (`_base_client.py:764`), and by
/// extension Python cognee's effective behaviour, since litellm inherits it.
///
/// `None` means "no usable guidance, use the backoff". That covers an absent or
/// unparseable header, the HTTP-date form (deliberately unsupported), a hint
/// above [`MAX_RETRY_AFTER`], and a zero/negative hint. Zero is excluded on
/// purpose: "retry immediately" from a provider that just rate-limited us is
/// how a 128-wide burst becomes a tight loop, and the exponential ladder with
/// its jitter is the better answer there.
///
/// Checks `retry-after-ms` before `retry-after`, matching the same precedence.
pub(crate) fn retry_after_hint(headers: &HeaderMap) -> Option<Duration> {
    let parse = |name: &str, to_duration: fn(u64) -> Duration| -> Option<Duration> {
        headers
            .get(name)?
            .to_str()
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
            .map(to_duration)
    };

    parse("retry-after-ms", Duration::from_millis)
        .or_else(|| parse("retry-after", Duration::from_secs))
        .filter(|hint| !hint.is_zero() && *hint <= MAX_RETRY_AFTER)
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
        // capped at 128s.
        assert_eq!(base_backoff_ms(1), 8_000);
        assert_eq!(base_backoff_ms(2), 16_000);
        assert_eq!(base_backoff_ms(3), 32_000);
        assert_eq!(base_backoff_ms(4), 64_000);
        assert_eq!(base_backoff_ms(5), 128_000);
        assert_eq!(base_backoff_ms(9), 128_000); // capped
    }

    #[test]
    fn backoff_never_exceeds_the_cap() {
        for _ in 0..200 {
            assert!(retry_backoff(100).as_millis() as u64 <= MAX_BACKOFF_MS);
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

    // ── RetryBudget: the dual floor ─────────────────────────────────────────

    #[test]
    fn budget_is_not_exhausted_until_both_floors_are_met() {
        let budget = RetryBudget::new(2, Duration::from_secs(240));

        // Attempts met, time not — this is the case a plain attempt cap gets
        // wrong, and the whole reason for the dual floor.
        assert!(!budget.is_exhausted(5, Duration::from_secs(10)));
        // Time met, attempts not.
        assert!(!budget.is_exhausted(1, Duration::from_secs(300)));
        // Neither.
        assert!(!budget.is_exhausted(1, Duration::from_secs(10)));
        // Both.
        assert!(budget.is_exhausted(2, Duration::from_secs(240)));
        assert!(budget.is_exhausted(9, Duration::from_secs(600)));
    }

    #[test]
    fn a_zero_time_floor_reduces_the_budget_to_an_attempt_cap() {
        // The documented LLM_MIN_RETRY_SECONDS=0 fail-fast escape hatch.
        let budget = RetryBudget::new(2, Duration::ZERO);
        assert!(!budget.is_exhausted(1, Duration::ZERO));
        assert!(budget.is_exhausted(2, Duration::ZERO));
    }

    #[test]
    fn the_attempt_floor_is_at_least_one() {
        // A misconfigured 0 must still make one attempt, never zero.
        assert!(!RetryBudget::new(0, Duration::ZERO).is_exhausted(0, Duration::ZERO));
        assert!(RetryBudget::new(0, Duration::ZERO).is_exhausted(1, Duration::ZERO));
    }

    // ── Retry-After ─────────────────────────────────────────────────────────

    fn headers_with(name: &str, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .expect("test header name is valid"),
            value.parse().expect("test header value is valid"),
        );
        headers
    }

    #[test]
    fn retry_after_seconds_is_honoured() {
        assert_eq!(
            retry_after_hint(&headers_with("retry-after", "12")),
            Some(Duration::from_secs(12))
        );
    }

    #[test]
    fn retry_after_ms_takes_precedence() {
        let mut headers = headers_with("retry-after", "30");
        headers.insert("retry-after-ms", "1500".parse().expect("valid"));
        assert_eq!(
            retry_after_hint(&headers),
            Some(Duration::from_millis(1500))
        );
    }

    #[test]
    fn an_out_of_range_retry_after_falls_back_to_the_backoff() {
        // Not clamped to 60s — ignored, so the caller uses its own ladder.
        assert_eq!(retry_after_hint(&headers_with("retry-after", "3600")), None);
        assert_eq!(
            retry_after_hint(&headers_with("retry-after", "60")),
            Some(Duration::from_secs(60)),
            "exactly at the bound is still usable"
        );
    }

    #[test]
    fn a_zero_retry_after_is_ignored_rather_than_retrying_instantly() {
        // "Retry immediately" from a provider that just rate-limited us is how a
        // wide burst becomes a tight loop; fall back to the jittered ladder.
        assert_eq!(retry_after_hint(&headers_with("retry-after", "0")), None);
        assert_eq!(retry_after_hint(&headers_with("retry-after-ms", "0")), None);
    }

    #[test]
    fn a_short_retry_after_wins_over_a_longer_backoff() {
        // The regression Copilot caught: the hint must replace the backoff, not
        // lose a max() against it.
        let hint =
            retry_after_hint(&headers_with("retry-after", "1")).expect("1s is a usable hint");
        assert!(
            hint < retry_backoff(1),
            "a 1s hint must be shorter than the 8s first backoff, else this \
             test proves nothing"
        );
    }

    #[test]
    fn unparseable_or_absent_retry_after_is_none() {
        assert_eq!(retry_after_hint(&HeaderMap::new()), None);
        // HTTP-date form is deliberately unsupported.
        assert_eq!(
            retry_after_hint(&headers_with(
                "retry-after",
                "Wed, 21 Oct 2026 07:28:00 GMT"
            )),
            None
        );
        assert_eq!(retry_after_hint(&headers_with("retry-after", "")), None);
    }
}
