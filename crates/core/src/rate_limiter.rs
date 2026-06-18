//! Proactive request-rate throttling for pipeline tasks.
// The semaphore is never closed (only dropped on teardown) so acquire() cannot
// fail — expect() is safe here.
#![allow(
    clippy::expect_used,
    reason = "semaphore is never closed; acquire() cannot return Err"
)]
//!
//! See `docs/cog-4454-core/03-rate-limiting.md` for the design rationale and how
//! this differs from `Pipeline::with_concurrency` (item parallelism) and
//! `RetryPolicy` (reactive backoff).
//!
//! # Choosing the right tool
//!
//! - **Proactive request-rate throttle** → [`RateLimiter`] (this module)
//! - **Bounded item-level parallelism** → [`Pipeline::with_concurrency`](crate::pipeline::Pipeline::with_concurrency)
//! - **Reactive backoff on failure** → [`RetryPolicy`](crate::pipeline::RetryPolicy)

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Semaphore;

/// Admission throttle: `acquire().await` returns when the caller is permitted to
/// start an external call. Object-safe; hold as `Arc<dyn RateLimiter>`.
///
/// Both [`TokenBucketLimiter`] and [`SemaphoreLimiter`] implement this trait.
/// The `acquire()` contract models *admission* (rate of starts), not
/// concurrency-with-hold. For true hold-until-done concurrency limiting,
/// prefer [`Pipeline::with_concurrency`](crate::pipeline::Pipeline::with_concurrency).
#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Wait until the caller is permitted to start an external call.
    async fn acquire(&self);
}

/// Caps the number of starts allowed per refill window (token-bucket algorithm).
///
/// `capacity` tokens are available at startup; a background task adds one token
/// every `1 / refill_per_sec` seconds, up to `capacity`. `acquire()` waits for a
/// token and consumes it (the refiller restores tokens over time).
///
/// # Panics (constructor)
///
/// `new` asserts `capacity > 0` and `refill_per_sec > 0.0`. These are API
/// misuse guards: a zero capacity would permanently block all callers and a
/// non-positive rate is mathematically undefined, so a panic at construction
/// time is the right signal.
///
/// # Refiller lifecycle
///
/// The background refiller task self-terminates one tick after the limiter is
/// dropped (`Arc::strong_count == 1`). This avoids leaking a task per limiter.
/// If a stricter shutdown is needed later, store a `tokio::task::JoinHandle`
/// + `Notify` and abort on `Drop`; not required for the initial implementation.
pub struct TokenBucketLimiter {
    semaphore: Arc<Semaphore>,
}

impl TokenBucketLimiter {
    /// Create a new token-bucket limiter.
    ///
    /// * `capacity` — maximum burst size (initial token count, upper refill bound).
    /// * `refill_per_sec` — tokens restored per second.
    ///
    /// # Panics
    ///
    /// Panics if `capacity == 0` or `refill_per_sec <= 0.0` (API misuse guards).
    pub fn new(capacity: usize, refill_per_sec: f64) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(refill_per_sec > 0.0, "refill_per_sec must be > 0");

        let semaphore = Arc::new(Semaphore::new(capacity));
        let refill = semaphore.clone();
        let interval = Duration::from_secs_f64(1.0 / refill_per_sec);

        // Background refiller. Stops when the semaphore (last Arc) is dropped.
        // Use interval_at (start = now + interval) so the first tick is one full
        // interval away; tokio::time::interval's first tick fires immediately, which
        // would refill a permit right away if all tokens were consumed before the
        // spawned task first runs.
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            loop {
                ticker.tick().await;
                // Only add back up to `capacity` (avoid unbounded growth).
                if refill.available_permits() < capacity {
                    refill.add_permits(1);
                }
                // Stop if we are the only holder left (limiter was dropped).
                if Arc::strong_count(&refill) == 1 {
                    break;
                }
            }
        });

        Self { semaphore }
    }
}

#[async_trait]
impl RateLimiter for TokenBucketLimiter {
    async fn acquire(&self) {
        // `forget()` consumes the permit without releasing it back to the
        // semaphore; the refiller restores tokens over time.
        // The semaphore is never closed (only dropped on limiter teardown),
        // so `acquire()` cannot return `Err`.
        let permit = self
            .semaphore
            .acquire()
            .await
            .expect("rate-limiter semaphore is never closed");
        permit.forget();
    }
}

/// Admission-style concurrency limit: at most `max_per_sec` starts may be
/// issued per second.
///
/// Distinct from [`Pipeline::with_concurrency`](crate::pipeline::Pipeline::with_concurrency),
/// which bounds data-item parallelism at the executor level. `SemaphoreLimiter`
/// is a proactive request-rate throttle implemented as a token bucket whose
/// capacity and refill rate are both set to `max_per_sec`.
pub struct SemaphoreLimiter {
    inner: TokenBucketLimiter,
}

impl SemaphoreLimiter {
    /// Create a new semaphore-style limiter that allows at most `max_per_sec`
    /// acquisitions per second.
    pub fn new(max_per_sec: usize) -> Self {
        Self {
            inner: TokenBucketLimiter::new(max_per_sec, max_per_sec as f64),
        }
    }
}

#[async_trait]
impl RateLimiter for SemaphoreLimiter {
    async fn acquire(&self) {
        self.inner.acquire().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// `TokenBucketLimiter` lets `capacity` immediate acquisitions through (the
    /// initial token pool is full), then the next acquire must wait for a refill
    /// tick.
    #[tokio::test]
    async fn token_bucket_burst_then_wait() {
        // 2 tokens, refill at 10/sec (100ms per token).
        let limiter = TokenBucketLimiter::new(2, 10.0);

        // First two acquires should be nearly instant (tokens available).
        let t0 = Instant::now();
        limiter.acquire().await;
        limiter.acquire().await;
        let burst_elapsed = t0.elapsed();
        assert!(
            burst_elapsed < Duration::from_millis(80),
            "burst acquires should be fast, took {burst_elapsed:?}"
        );

        // Third acquire must wait ~100ms for the next refill tick.
        let t1 = Instant::now();
        limiter.acquire().await;
        let wait_elapsed = t1.elapsed();
        assert!(
            wait_elapsed >= Duration::from_millis(50),
            "third acquire should wait for refill, took {wait_elapsed:?}"
        );
    }

    /// `SemaphoreLimiter::new` panics if `max_per_sec == 0` (delegated through
    /// `TokenBucketLimiter::new`'s capacity assert).
    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn semaphore_limiter_rejects_zero() {
        let _ = SemaphoreLimiter::new(0);
    }

    /// `TokenBucketLimiter::new` panics on zero capacity.
    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn token_bucket_rejects_zero_capacity() {
        let _ = TokenBucketLimiter::new(0, 1.0);
    }

    /// `TokenBucketLimiter::new` panics on non-positive refill rate.
    #[test]
    #[should_panic(expected = "refill_per_sec must be > 0")]
    fn token_bucket_rejects_zero_rate() {
        let _ = TokenBucketLimiter::new(1, 0.0);
    }
}
