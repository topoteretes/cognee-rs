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

use std::time::{Duration, Instant};

use cognee_utils::retry::RetryConfig;
use reqwest::header::HeaderMap;

use crate::error::LlmError;

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

/// How long the wait for dispatch — pacing, then the in-flight queue — may take
/// on this attempt, given the caller's aggregate deadline. `None` is unbounded.
///
/// The adapters wait twice before they can send: the pacer's bucket, which can
/// hold a caller for a whole [`cognee_utils::pacing::OVERLOAD_COOLDOWN`] (900s),
/// and the in-flight queue, which has no bound of its own. Neither is visible to
/// the guard at the top of the retry loop, because both happen after it. The
/// documented ceiling is `deadline + one request timeout`, so an attempt that
/// parks in either of them past the deadline has already blown it before it
/// opens a socket — and bounding those waits by what is left is what keeps the
/// ceiling true.
///
/// Two cases return `None`, and both are deliberate:
///
/// * `attempt == 0` — every call makes at least one attempt, as it did before
///   the deadline existed. A first attempt is allowed to wait out a whole
///   overload episode; a call that never dispatches cannot even report what the
///   provider said.
/// * no deadline — `LLM_REQUEST_DEADLINE_SECONDS=0` means unbounded, and a long
///   pacing wait must not abort a call that asked for no budget at all.
///
/// Otherwise the budget is what is left, **including zero**. Zero is the case
/// this function exists for: the guard at the top of the loop clamps a retry
/// backoff to the remaining budget and lets the attempt it slept for start, so
/// that attempt reaches this point with its budget exactly spent. It keeps its
/// right to dispatch — `Pacer::admit_within` and `acquire_in_flight_within` both
/// admit on a zero budget whenever they do not have to block — but it loses the
/// right to wait, which is what stops a clamped attempt from silently adding a
/// 900s cooldown on top of the ceiling.
pub(crate) fn dispatch_budget(
    attempt: u32,
    deadline: Option<Instant>,
    now: Instant,
) -> Option<Duration> {
    if attempt == 0 {
        return None;
    }
    deadline.map(|deadline| deadline.saturating_duration_since(now))
}

/// The error a retry attempt returns when the wait for dispatch will not fit in
/// what is left of the caller's aggregate budget.
///
/// Shared so the three adapters report the overshoot identically; `label` is the
/// provider noun each of them already uses in its other deadline messages.
pub(crate) fn dispatch_budget_spent(
    label: &str,
    elapsed: Duration,
    attempts: u32,
    last_error: &LlmError,
) -> LlmError {
    LlmError::Timeout(format!(
        "{label} request abandoned after {:.0}s with {attempts} attempt(s): waiting for \
         dispatch (pacing or the in-flight queue) would have run past the call's aggregate \
         budget (LLM_REQUEST_DEADLINE_SECONDS); last error: {last_error}",
        elapsed.as_secs_f64(),
    ))
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
#[allow(
    clippy::expect_used,
    reason = "test code: a panic on a malformed fixture is an acceptable failure"
)]
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

    /// `Instant` arithmetic without sleeping: an offset from a fixed base.
    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    /// The first attempt always runs. Every call made one before the deadline
    /// existed, and a call that never dispatches cannot report a provider error.
    #[test]
    fn the_first_attempt_waits_for_dispatch_unbounded() {
        let base = Instant::now();
        assert_eq!(dispatch_budget(0, Some(at(base, 10)), at(base, 1)), None);
        // Even with the budget already gone.
        assert_eq!(dispatch_budget(0, Some(at(base, 10)), at(base, 99)), None);
    }

    /// `LLM_REQUEST_DEADLINE_SECONDS=0` disables the budget, and an unbounded
    /// call must not be aborted by a long pacing wait.
    #[test]
    fn no_deadline_leaves_the_dispatch_wait_unbounded() {
        let base = Instant::now();
        assert_eq!(dispatch_budget(3, None, at(base, 9_999)), None);
    }

    #[test]
    fn a_retry_may_wait_for_what_is_left_of_the_budget() {
        let base = Instant::now();
        assert_eq!(
            dispatch_budget(1, Some(at(base, 10)), at(base, 4)),
            Some(Duration::from_secs(6))
        );
    }

    /// The hole this function closes. A retry backoff is clamped to the
    /// remaining budget and then slept, so the attempt it clamped for reaches
    /// the dispatch wait at or past the deadline. That attempt keeps its right
    /// to dispatch — `Some(ZERO)`, not an abort — but must not be handed an
    /// unbounded wait, which is what `None` here would mean: up to a 900s
    /// cooldown plus an untimed queue on top of a ceiling already reached.
    #[test]
    fn a_spent_budget_bounds_the_wait_at_zero_rather_than_removing_it() {
        let base = Instant::now();
        let deadline = at(base, 10);
        assert_eq!(
            dispatch_budget(1, Some(deadline), deadline),
            Some(Duration::ZERO),
            "reaching the deadline exactly leaves no time to wait"
        );
        assert_eq!(
            dispatch_budget(4, Some(deadline), at(base, 900)),
            Some(Duration::ZERO),
            "long past it, still bounded rather than unbounded"
        );
    }

    /// The boundary: one tick short of the deadline is still budget.
    #[test]
    fn a_tick_of_budget_is_still_budget() {
        let base = Instant::now();
        let deadline = at(base, 10);
        assert_eq!(
            dispatch_budget(1, Some(deadline), deadline - Duration::from_nanos(1)),
            Some(Duration::from_nanos(1))
        );
    }
}
