//! Generic retry logic with exponential backoff.

use std::future::Future;
use std::time::Duration;

/// Configuration for exponential backoff retry logic.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (0 means no retries, just one attempt).
    pub max_retries: u32,

    /// Initial delay in milliseconds before the first retry.
    pub initial_delay_ms: u64,

    /// Maximum delay in milliseconds between retries.
    pub max_delay_ms: u64,

    /// Multiplier for exponential backoff (typically 2.0).
    pub backoff_multiplier: f64,

    /// Optional jitter factor (0.0 to 1.0) to randomize delays.
    /// If Some(0.5), actual delay will be in range [delay * 0.5, delay * 1.5].
    pub jitter_factor: Option<f64>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 30_000, // 30 seconds
            backoff_multiplier: 2.0,
            jitter_factor: None,
        }
    }
}

impl RetryConfig {
    /// Create a new retry configuration with custom values.
    pub fn new(max_retries: u32, initial_delay_ms: u64, max_delay_ms: u64) -> Self {
        Self {
            max_retries,
            initial_delay_ms,
            max_delay_ms,
            backoff_multiplier: 2.0,
            jitter_factor: None,
        }
    }

    /// Set the backoff multiplier.
    pub fn with_backoff_multiplier(mut self, multiplier: f64) -> Self {
        self.backoff_multiplier = multiplier;
        self
    }

    /// Set the jitter factor (0.0 to 1.0).
    pub fn with_jitter(mut self, jitter: f64) -> Self {
        self.jitter_factor = Some(jitter.clamp(0.0, 1.0));
        self
    }

    /// Calculate the delay for a given attempt number.
    fn calculate_delay(&self, attempt: u32) -> Duration {
        let base_delay =
            self.initial_delay_ms as f64 * self.backoff_multiplier.powi(attempt as i32);

        let delay_ms = base_delay.min(self.max_delay_ms as f64);

        let final_delay_ms = if let Some(jitter) = self.jitter_factor {
            let jitter_range = delay_ms * jitter;
            let random_jitter = (rand::random::<f64>() * 2.0 - 1.0) * jitter_range;
            (delay_ms + random_jitter).max(0.0)
        } else {
            delay_ms
        };

        Duration::from_millis(final_delay_ms as u64)
    }
}

/// Retry result indicating whether to continue retrying.
pub enum RetryDecision {
    /// Retry the operation after the calculated delay.
    Retry,
    /// Stop retrying and return the error.
    Abort,
}

/// Execute an async operation with exponential backoff retry logic.
///
/// # Type Parameters
/// * `T` - The success result type.
/// * `E` - The error type.
/// * `F` - The future type returned by the operation.
/// * `Op` - The operation function type.
/// * `Pred` - The predicate function type.
///
/// # Arguments
/// * `config` - Retry configuration (max retries, delays, etc.).
/// * `operation` - The async operation to retry. Called for each attempt.
/// * `should_retry` - Predicate that examines the error and decides whether to retry.
///   Returns `RetryDecision::Retry` to continue, `RetryDecision::Abort` to stop.
///
/// # Returns
/// * `Ok(T)` if the operation succeeds within the retry limit.
/// * `Err(E)` if all retries are exhausted or the predicate returns `Abort`.
///
/// # Example
/// ```ignore
/// use cognee_utils::retry::{retry_with_backoff, RetryConfig, RetryDecision};
///
/// #[derive(Debug)]
/// enum MyError {
///     Transient(String),
///     Permanent(String),
/// }
///
/// let config = RetryConfig::new(3, 100, 5000);
///
/// let result = retry_with_backoff(
///     config,
///     || async {
///         // Your async operation here
///         fetch_data().await
///     },
///     |error| {
///         match error {
///             MyError::Transient(_) => RetryDecision::Retry,
///             MyError::Permanent(_) => RetryDecision::Abort,
///         }
///     },
/// ).await;
/// ```
pub async fn retry_with_backoff<T, E, F, Op, Pred>(
    config: RetryConfig,
    mut operation: Op,
    should_retry: Pred,
) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
    Op: FnMut() -> F,
    Pred: Fn(&E) -> RetryDecision,
{
    let mut attempt = 0;

    loop {
        match operation().await {
            Ok(result) => {
                if attempt > 0 {
                    tracing::info!("Operation succeeded after {} retry attempt(s)", attempt);
                }
                return Ok(result);
            }
            Err(error) => {
                let decision = should_retry(&error);

                match decision {
                    RetryDecision::Abort => {
                        tracing::debug!(
                            "Aborting retry after {} attempt(s) due to non-retryable error",
                            attempt + 1
                        );
                        return Err(error);
                    }
                    RetryDecision::Retry => {
                        if attempt >= config.max_retries {
                            tracing::warn!(
                                "Max retries ({}) exceeded, returning error",
                                config.max_retries
                            );
                            return Err(error);
                        }

                        let delay = config.calculate_delay(attempt);
                        tracing::debug!(
                            "Retry attempt {}/{}, waiting {:?} before next attempt",
                            attempt + 1,
                            config.max_retries,
                            delay
                        );

                        // futures-timer's Delay is runtime-agnostic and works on
                        // both native and wasm32 (its wasm-bindgen feature routes
                        // through setTimeout). tokio::time would not fire on
                        // wasm32-unknown — no clock, no thread parking.
                        futures_timer::Delay::new(delay).await;
                        attempt += 1;
                    }
                }
            }
        }
    }
}

// These are #[tokio::test] async tests on the native libtest harness; tokio is a
// native-only dev-dependency, so the module is gated off wasm (matching how the
// dev-deps are target-split in Cargo.toml). This keeps `cargo test --target
// wasm32` compiling — the tests run on native, where retry actually executes.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[derive(Debug, PartialEq)]
    enum TestError {
        Transient,
        Permanent,
    }

    #[tokio::test]
    async fn test_succeeds_immediately() {
        let config = RetryConfig::new(3, 10, 1000);

        let result = retry_with_backoff(
            config,
            || async { Ok::<i32, TestError>(42) },
            |_| RetryDecision::Retry,
        )
        .await;

        assert_eq!(result, Ok(42));
    }

    #[tokio::test]
    async fn test_succeeds_after_retries() {
        let config = RetryConfig::new(3, 10, 1000);
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let result = retry_with_backoff(
            config,
            || {
                let counter = counter_clone.clone();
                async move {
                    let count = counter.fetch_add(1, Ordering::SeqCst);
                    if count < 2 {
                        Err(TestError::Transient)
                    } else {
                        Ok(42)
                    }
                }
            },
            |_| RetryDecision::Retry,
        )
        .await;

        assert_eq!(result, Ok(42));
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_max_retries_exceeded() {
        let config = RetryConfig::new(2, 10, 1000);
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let result = retry_with_backoff(
            config,
            || {
                let counter = counter_clone.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err::<i32, TestError>(TestError::Transient)
                }
            },
            |_| RetryDecision::Retry,
        )
        .await;

        assert_eq!(result, Err(TestError::Transient));
        assert_eq!(counter.load(Ordering::SeqCst), 3); // 1 initial + 2 retries
    }

    #[tokio::test]
    async fn test_abort_on_permanent_error() {
        let config = RetryConfig::new(3, 10, 1000);
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let result = retry_with_backoff(
            config,
            || {
                let counter = counter_clone.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Err::<i32, TestError>(TestError::Permanent)
                }
            },
            |error| match error {
                TestError::Transient => RetryDecision::Retry,
                TestError::Permanent => RetryDecision::Abort,
            },
        )
        .await;

        assert_eq!(result, Err(TestError::Permanent));
        assert_eq!(counter.load(Ordering::SeqCst), 1); // Only 1 attempt, no retries
    }

    #[tokio::test]
    async fn test_exponential_backoff_timing() {
        let config = RetryConfig::new(3, 100, 5000);

        // Test delay calculations
        assert_eq!(config.calculate_delay(0).as_millis(), 100);
        assert_eq!(config.calculate_delay(1).as_millis(), 200);
        assert_eq!(config.calculate_delay(2).as_millis(), 400);
        assert_eq!(config.calculate_delay(3).as_millis(), 800);

        // Test max delay cap
        let long_config = RetryConfig::new(10, 1000, 3000);
        assert_eq!(long_config.calculate_delay(5).as_millis(), 3000); // Capped at max
    }

    #[tokio::test]
    async fn test_with_custom_backoff_multiplier() {
        let config = RetryConfig::new(3, 100, 5000).with_backoff_multiplier(3.0);

        assert_eq!(config.calculate_delay(0).as_millis(), 100);
        assert_eq!(config.calculate_delay(1).as_millis(), 300);
        assert_eq!(config.calculate_delay(2).as_millis(), 900);
    }
}
