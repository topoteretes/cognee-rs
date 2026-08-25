//! Reactive request pacing for LLM and embedding dispatch.
//!
//! This is a port of Python cognee's pacing seam
//! (`cognee/shared/rate_limiting.py` + `cognee/infrastructure/llm/overload_policy.py`),
//! which has three properties worth stating explicitly because they are easy to
//! get wrong:
//!
//! 1. **The fast path is unbounded.** Pacing is *off* until either an operator
//!    turns it on (`LLM_RATE_LIMIT_ENABLED`) or the provider tells us it is
//!    unhappy. Steady-state throughput is unaffected.
//! 2. **Pacing is reactive.** HTTP 429/503/529 or a timeout opens an *episode*:
//!    for the next [`OVERLOAD_COOLDOWN`] the requests-per-interval bucket
//!    applies. Fresh evidence extends the episode; it lapses on quiet.
//! 3. **Admission happens per retry attempt, inside the retry loop** — not once
//!    per logical call. That is what lets an episode opened by one in-flight
//!    request throttle the remaining attempts of every other request already in
//!    flight. Callers must therefore `admit().await` immediately before each
//!    HTTP send, not before the loop.
//!
//! There is deliberately **no token/TPM accounting**: Python declares
//! `llm_rate_limit_tokens` and never reads it, so a TPM budget here would be a
//! divergence, not parity.
//!
//! ## Why no tokio
//!
//! `cognee-utils` builds for `wasm32` and carries no tokio library dependency
//! (see this crate's `Cargo.toml`). Sleeping therefore goes through
//! `futures-timer`, as [`crate::retry`] already does. The bucket and policy
//! keep their time-dependent logic in pure functions that take `now` as an
//! argument ([`TokenBucket::reserve`], [`OverloadPolicy::record_evidence_at`]),
//! so the arithmetic is tested synchronously and exactly rather than through a
//! paused runtime clock. The module is gated off `wasm32` because
//! `std::time::Instant::now()` panics there.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long an overload episode paces dispatch after the most recent piece of
/// evidence. Mirrors `COOLDOWN_SECONDS = 900.0` in Python's `overload_policy.py`.
pub const OVERLOAD_COOLDOWN: Duration = Duration::from_secs(900);

/// A requests-per-interval token bucket, equivalent to Python's
/// `aiolimiter.AsyncLimiter(requests, interval)`.
///
/// Capacity equals `requests`, so a burst of that size is admitted immediately
/// and the bucket then refills continuously at `requests / interval` per second.
/// Debt is allowed to accumulate — a caller that finds the bucket empty reserves
/// its token anyway and is told how long to wait for it, which keeps concurrent
/// waiters in arrival order instead of letting them stampede a single free slot.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    state: Mutex<BucketState>,
}

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// Build a bucket admitting `requests` per `interval`.
    ///
    /// Both are clamped to at least 1 / 1s: a zero rate would mean "never
    /// admit", which is never what a misconfigured env var intends.
    pub fn new(requests: u32, interval: Duration) -> Self {
        let capacity = f64::from(requests.max(1));
        let seconds = interval.as_secs_f64().max(1.0);
        Self {
            capacity,
            refill_per_sec: capacity / seconds,
            state: Mutex::new(BucketState {
                tokens: capacity,
                last: Instant::now(),
            }),
        }
    }

    /// Reserve one token as of `now`, returning how long the caller must wait
    /// before its request may proceed.
    ///
    /// Pure with respect to the clock — the caller supplies `now` — so the
    /// refill and debt arithmetic is directly testable without sleeping.
    pub fn reserve(&self, now: Instant) -> Duration {
        // A poisoned mutex only means some other caller panicked mid-update;
        // the bucket is still coherent enough to pace with, and refusing to
        // pace is strictly worse than pacing from slightly stale state.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let elapsed = now.saturating_duration_since(state.last).as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        state.last = now;
        state.tokens -= 1.0;

        if state.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-state.tokens / self.refill_per_sec)
        }
    }

    /// Reserve a token and wait for it.
    pub async fn acquire(&self) {
        let wait = self.reserve(Instant::now());
        if !wait.is_zero() {
            futures_timer::Delay::new(wait).await;
        }
    }
}

/// Tracks whether the provider is currently signalling overload.
///
/// Port of Python's `LlmOverloadPolicy`. Evidence opens or extends an episode
/// lasting `cooldown`; the episode lapses once that window passes without
/// further evidence.
#[derive(Debug)]
pub struct OverloadPolicy {
    cooldown: Duration,
    /// Fixed reference point so the deadline can live in a lock-free atomic
    /// (an `Instant` is not atomically storable).
    origin: Instant,
    /// Milliseconds since `origin` at which the current episode ends; 0 = idle.
    paced_until_ms: AtomicU64,
}

impl OverloadPolicy {
    /// Build a policy with the given cooldown. Tests pass a short one.
    pub fn new(cooldown: Duration) -> Self {
        Self {
            cooldown,
            origin: Instant::now(),
            paced_until_ms: AtomicU64::new(0),
        }
    }

    fn millis_since_origin(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.origin).as_millis() as u64
    }

    /// Whether an episode is active as of `now`.
    pub fn is_paced_at(&self, now: Instant) -> bool {
        self.paced_until_ms.load(Ordering::Acquire) > self.millis_since_origin(now)
    }

    /// Whether an episode is active right now.
    pub fn is_paced(&self) -> bool {
        self.is_paced_at(Instant::now())
    }

    /// Record overload evidence observed at `now`, opening or extending an
    /// episode. Returns true when this evidence *opened* a fresh episode, which
    /// is the once-per-episode warning point (Python warns once per episode and
    /// extends silently).
    pub fn record_evidence_at(&self, now: Instant, reason: &str) -> bool {
        let now_ms = self.millis_since_origin(now);
        let until_ms = now_ms.saturating_add(self.cooldown.as_millis() as u64);
        // `fetch_max` makes concurrent evidence monotonic without a CAS loop:
        // the furthest deadline wins and no update can shorten an episode.
        let previous = self.paced_until_ms.fetch_max(until_ms, Ordering::AcqRel);
        let opened = previous <= now_ms;
        if opened {
            tracing::warn!(
                reason,
                cooldown_secs = self.cooldown.as_secs(),
                "LLM provider reported overload; pacing requests for the cooldown window. \
                 Tune with LLM_RATE_LIMIT_REQUESTS / LLM_RATE_LIMIT_INTERVAL, \
                 or disable with AUTO_RATE_LIMIT=false."
            );
        }
        opened
    }

    /// Record overload evidence observed now.
    pub fn record_evidence(&self, reason: &str) -> bool {
        self.record_evidence_at(Instant::now(), reason)
    }
}

/// Couples the bucket and the policy into the seam adapters call.
///
/// Mirrors `_governed_llm_dispatch` in Python's `rate_limiting.py`.
#[derive(Debug)]
pub struct Pacer {
    enabled_by_config: bool,
    auto_react: bool,
    bucket: TokenBucket,
    policy: OverloadPolicy,
}

impl Pacer {
    /// * `requests` / `interval` — the bucket rate once pacing is active.
    /// * `enabled_by_config` — `LLM_RATE_LIMIT_ENABLED`; pace unconditionally.
    /// * `auto_react` — `AUTO_RATE_LIMIT`; let provider errors open an episode.
    pub fn new(
        requests: u32,
        interval: Duration,
        enabled_by_config: bool,
        auto_react: bool,
    ) -> Self {
        Self::with_cooldown(
            requests,
            interval,
            enabled_by_config,
            auto_react,
            OVERLOAD_COOLDOWN,
        )
    }

    /// As [`Pacer::new`], with an explicit cooldown so tests need not wait 15
    /// minutes to observe an episode lapse.
    pub fn with_cooldown(
        requests: u32,
        interval: Duration,
        enabled_by_config: bool,
        auto_react: bool,
        cooldown: Duration,
    ) -> Self {
        Self {
            enabled_by_config,
            auto_react,
            bucket: TokenBucket::new(requests, interval),
            policy: OverloadPolicy::new(cooldown),
        }
    }

    /// Whether dispatch should be paced as of `now`.
    pub fn should_pace_at(&self, now: Instant) -> bool {
        self.enabled_by_config || self.policy.is_paced_at(now)
    }

    /// Whether an overload episode is currently active.
    pub fn is_paced(&self) -> bool {
        self.policy.is_paced()
    }

    /// Gate one dispatch attempt.
    ///
    /// Returns immediately on the fast path. Call this immediately before each
    /// HTTP send, inside the retry loop — see the module docs.
    pub async fn admit(&self) {
        if !self.should_pace_at(Instant::now()) {
            return;
        }
        self.bucket.acquire().await;
    }

    /// Feed a provider error to the policy. No-op when `auto_react` is off.
    pub fn record_overload(&self, reason: &str) {
        if self.auto_react {
            self.policy.record_evidence(reason);
        }
    }
}

static LLM_PACER: OnceLock<Arc<Pacer>> = OnceLock::new();
static EMBEDDING_PACER: OnceLock<Arc<Pacer>> = OnceLock::new();

/// Install the process-wide LLM pacer. First call wins, mirroring Python's lazy
/// module-level limiter; later calls return the existing instance so repeated
/// component construction is harmless.
pub fn init_llm_pacer(
    requests: u32,
    interval: Duration,
    enabled_by_config: bool,
    auto_react: bool,
) -> Arc<Pacer> {
    Arc::clone(LLM_PACER.get_or_init(|| {
        Arc::new(Pacer::new(
            requests,
            interval,
            enabled_by_config,
            auto_react,
        ))
    }))
}

/// The process-wide LLM pacer, if one has been installed.
///
/// Adapters used as a plain library — with no component factory to install a
/// pacer — get `None` and keep their previous unpaced behaviour.
pub fn llm_pacer() -> Option<Arc<Pacer>> {
    LLM_PACER.get().map(Arc::clone)
}

/// Install the process-wide embedding pacer.
///
/// Always constructed with `auto_react = false`: Python's embedding limiter is
/// flag-gated only and neither feeds nor consults the overload policy
/// (`rate_limiting.py`'s `embedding_rate_limiter_context_manager`). Reactive
/// pacing is an LLM-path behaviour.
pub fn init_embedding_pacer(
    requests: u32,
    interval: Duration,
    enabled_by_config: bool,
) -> Arc<Pacer> {
    Arc::clone(
        EMBEDDING_PACER
            .get_or_init(|| Arc::new(Pacer::new(requests, interval, enabled_by_config, false))),
    )
}

/// The process-wide embedding pacer, if one has been installed.
pub fn embedding_pacer() -> Option<Arc<Pacer>> {
    EMBEDDING_PACER.get().map(Arc::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TokenBucket ─────────────────────────────────────────────────────────

    #[test]
    fn burst_up_to_capacity_is_admitted_without_waiting() {
        let bucket = TokenBucket::new(3, Duration::from_secs(3));
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(bucket.reserve(t0), Duration::ZERO);
        }
    }

    #[test]
    fn the_request_after_a_burst_waits_one_refill_period() {
        // 3 per 3s => one token per second.
        let bucket = TokenBucket::new(3, Duration::from_secs(3));
        let t0 = Instant::now();
        for _ in 0..3 {
            bucket.reserve(t0);
        }
        let wait = bucket.reserve(t0);
        assert!(
            (wait.as_secs_f64() - 1.0).abs() < 1e-6,
            "expected ~1s, got {wait:?}"
        );
    }

    #[test]
    fn concurrent_waiters_queue_rather_than_collide() {
        let bucket = TokenBucket::new(1, Duration::from_secs(1));
        let t0 = Instant::now();
        assert_eq!(bucket.reserve(t0), Duration::ZERO);
        // Debt accumulates, so each subsequent caller waits strictly longer.
        let first = bucket.reserve(t0);
        let second = bucket.reserve(t0);
        assert!(
            second > first,
            "waits should increase with queue depth: {first:?} then {second:?}"
        );
    }

    #[test]
    fn tokens_refill_over_time_but_never_exceed_capacity() {
        let bucket = TokenBucket::new(2, Duration::from_secs(2));
        let t0 = Instant::now();
        bucket.reserve(t0);
        bucket.reserve(t0);
        // Idle far longer than it takes to refill; capacity must still cap it.
        let later = t0 + Duration::from_secs(600);
        assert_eq!(bucket.reserve(later), Duration::ZERO);
        assert_eq!(bucket.reserve(later), Duration::ZERO);
        assert!(bucket.reserve(later) > Duration::ZERO, "capacity is 2");
    }

    #[test]
    fn a_zero_rate_is_clamped_rather_than_blocking_forever() {
        let bucket = TokenBucket::new(0, Duration::ZERO);
        assert_eq!(bucket.reserve(Instant::now()), Duration::ZERO);
    }

    // ── OverloadPolicy ──────────────────────────────────────────────────────

    #[test]
    fn evidence_opens_an_episode_that_lapses_after_the_cooldown() {
        let policy = OverloadPolicy::new(Duration::from_secs(100));
        let t0 = Instant::now();
        assert!(!policy.is_paced_at(t0));

        assert!(policy.record_evidence_at(t0, "429"), "fresh episode");
        assert!(policy.is_paced_at(t0 + Duration::from_secs(99)));
        assert!(!policy.is_paced_at(t0 + Duration::from_secs(101)));
    }

    #[test]
    fn evidence_inside_an_episode_extends_it_silently() {
        let policy = OverloadPolicy::new(Duration::from_secs(100));
        let t0 = Instant::now();
        policy.record_evidence_at(t0, "429");

        let later = t0 + Duration::from_secs(99);
        assert!(
            !policy.record_evidence_at(later, "429"),
            "extending must not report a fresh episode (warn-once)"
        );
        // Originally due to lapse at t0+100; now runs to later+100.
        assert!(policy.is_paced_at(t0 + Duration::from_secs(101)));
        assert!(!policy.is_paced_at(later + Duration::from_secs(101)));
    }

    #[test]
    fn a_lapsed_episode_reports_fresh_on_the_next_evidence() {
        let policy = OverloadPolicy::new(Duration::from_secs(100));
        let t0 = Instant::now();
        policy.record_evidence_at(t0, "429");

        let after_lapse = t0 + Duration::from_secs(200);
        assert!(!policy.is_paced_at(after_lapse));
        assert!(
            policy.record_evidence_at(after_lapse, "503"),
            "a new episode warns again"
        );
    }

    #[test]
    fn an_episode_can_never_be_shortened_by_later_evidence() {
        let policy = OverloadPolicy::new(Duration::from_secs(100));
        let t0 = Instant::now();
        policy.record_evidence_at(t0 + Duration::from_secs(50), "429");
        // Out-of-order/stale evidence must not pull the deadline back in.
        policy.record_evidence_at(t0, "429");
        assert!(policy.is_paced_at(t0 + Duration::from_secs(149)));
    }

    // ── Pacer composition ───────────────────────────────────────────────────

    #[test]
    fn the_fast_path_does_not_pace_even_with_an_empty_bucket() {
        let pacer = Pacer::new(1, Duration::from_secs(3600), false, true);
        let now = Instant::now();
        // Drain the bucket; admission must still be a no-op while idle.
        pacer.bucket.reserve(now);
        pacer.bucket.reserve(now);
        assert!(!pacer.should_pace_at(now));
    }

    #[test]
    fn evidence_flips_admission_on_without_a_config_change() {
        let pacer = Pacer::with_cooldown(
            60,
            Duration::from_secs(60),
            false,
            true,
            Duration::from_secs(100),
        );
        let now = Instant::now();
        assert!(!pacer.should_pace_at(now));
        pacer.record_overload("429");
        assert!(pacer.should_pace_at(now));
    }

    #[test]
    fn auto_react_off_ignores_evidence() {
        let pacer = Pacer::new(60, Duration::from_secs(60), false, false);
        pacer.record_overload("429");
        assert!(
            !pacer.should_pace_at(Instant::now()),
            "AUTO_RATE_LIMIT=false must leave dispatch unpaced"
        );
    }

    #[test]
    fn enabled_by_config_paces_from_the_start() {
        let pacer = Pacer::new(60, Duration::from_secs(60), true, false);
        assert!(pacer.should_pace_at(Instant::now()));
    }

    #[tokio::test]
    async fn admit_returns_immediately_on_the_fast_path() {
        let pacer = Pacer::new(1, Duration::from_secs(3600), false, true);
        let started = Instant::now();
        pacer.admit().await;
        pacer.admit().await;
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn admit_waits_once_an_episode_is_open() {
        // 20 per second => a 50ms spacing once the single-token burst is spent.
        let pacer = Pacer::new(20, Duration::from_secs(1), true, true);
        for _ in 0..20 {
            pacer.admit().await;
        }
        let started = Instant::now();
        pacer.admit().await;
        assert!(
            started.elapsed() >= Duration::from_millis(20),
            "expected the bucket to delay dispatch, waited {:?}",
            started.elapsed()
        );
    }
}
