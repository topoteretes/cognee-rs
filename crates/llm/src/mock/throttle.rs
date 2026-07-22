//! Throttle and counting decorator for the offline load harness (issue #19).
//!
//! [`ReplayLlm`](super::replay::ReplayLlm) answers instantly, which is ideal for
//! deterministic replay but hides the one regime that matters for the cognify
//! LLM-call optimization: the high-load case where the provider rate limit, not
//! the network, is the bottleneck. [`ThrottleLlm`] wraps any [`Llm`] and adds the
//! two properties the instant mock lacks, so a benchmark can reproduce that
//! regime offline and for free:
//!
//! 1. Request counting. Every call is counted per outcome, so a run reports how
//!    many requests it issued. Request count is the direct measure of RPM
//!    pressure, and it is deterministic (the same corpus always issues the same
//!    number of calls), so it is the headline metric a caching or batching change
//!    moves, not wall-clock.
//! 2. A rolling requests-per-minute budget. With [`ThrottleConfig::rpm_limit`]
//!    set, calls beyond the budget in the current 60s window are rejected with
//!    the same [`LlmError::RateLimitExceeded`] a real HTTP 429 maps to, so retry
//!    and backoff paths are exercised deterministically.
//! 3. An optional per-call latency ([`ThrottleConfig::per_call_latency`]) so the
//!    overlap a pipelining change buys becomes visible against the instant mock.
//!
//! With `rpm_limit = None` the decorator is a pure request counter (the "before"
//! measurement). With a limit set it reproduces the rate-limited regime. The
//! simulated 429 is a model, not a promise that it matches a specific provider;
//! the deterministic request count is the metric meant to carry a headline claim.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::time::Instant;

use crate::error::{LlmError, LlmResult};
use crate::llm_trait::{Llm, StructuredOutputValidator};
use crate::types::{GenerationOptions, GenerationResponse, Message};

/// Rolling window over which [`ThrottleConfig::rpm_limit`] is enforced.
const WINDOW: Duration = Duration::from_secs(60);

/// Configuration for [`ThrottleLlm`].
#[derive(Clone, Debug, Default)]
pub struct ThrottleConfig {
    /// Maximum number of allowed (non-429) requests per rolling 60s window.
    /// `None` disables throttling, making the decorator a pure request counter.
    pub rpm_limit: Option<u32>,
    /// Latency injected before each allowed call, to simulate provider
    /// turnaround. `None` keeps the inner mock's instant response.
    pub per_call_latency: Option<Duration>,
}

/// Snapshot of what a [`ThrottleLlm`] has observed.
///
/// `allowed + rate_limited == total_calls`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThrottleMetrics {
    /// Every call the decorator received, including those rejected with a 429.
    pub total_calls: u64,
    /// Calls passed through to the inner LLM.
    pub allowed: u64,
    /// Calls rejected with a simulated 429 because the RPM budget was exceeded.
    pub rate_limited: u64,
}

struct State {
    /// Instants of the allowed requests still inside the current window.
    window: VecDeque<Instant>,
    metrics: ThrottleMetrics,
}

/// A decorator that wraps an inner [`Llm`], counts requests, and optionally
/// enforces a rolling requests-per-minute budget and per-call latency.
///
/// Cloneable-friendly: hold it behind an [`Arc`] and share it across the
/// concurrent extraction and summarization tasks so the counts and the budget
/// are global, exactly as a single provider account would see them.
pub struct ThrottleLlm {
    inner: Arc<dyn Llm>,
    config: ThrottleConfig,
    state: Mutex<State>,
}

impl ThrottleLlm {
    /// Wrap `inner` with the given throttle configuration.
    pub fn new(inner: Arc<dyn Llm>, config: ThrottleConfig) -> Self {
        Self {
            inner,
            config,
            state: Mutex::new(State {
                window: VecDeque::new(),
                metrics: ThrottleMetrics::default(),
            }),
        }
    }

    /// Snapshot the metrics observed so far.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    pub fn metrics(&self) -> ThrottleMetrics {
        self.state.lock().unwrap().metrics
    }

    /// Account for one call and enforce the RPM budget.
    ///
    /// Evicts timestamps older than [`WINDOW`], then either records an allowed
    /// request or returns [`LlmError::RateLimitExceeded`] when the budget is
    /// full. The lock is released before any `.await` in [`gate`](Self::gate),
    /// so it is never held across a suspension point.
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    fn admit(&self) -> LlmResult<()> {
        let mut state = self.state.lock().unwrap();
        state.metrics.total_calls += 1;

        if let Some(limit) = self.config.rpm_limit {
            let now = Instant::now();
            while let Some(oldest) = state.window.front() {
                if now.duration_since(*oldest) >= WINDOW {
                    state.window.pop_front();
                } else {
                    break;
                }
            }
            if state.window.len() as u32 >= limit {
                state.metrics.rate_limited += 1;
                return Err(LlmError::RateLimitExceeded(
                    "simulated 429: RPM budget exceeded (ThrottleLlm)".to_string(),
                ));
            }
            state.window.push_back(now);
        }

        state.metrics.allowed += 1;
        Ok(())
    }

    /// Admit one call (enforcing the budget), then replay the configured latency.
    async fn gate(&self) -> LlmResult<()> {
        self.admit()?;
        if let Some(latency) = self.config.per_call_latency {
            tokio::time::sleep(latency).await;
        }
        Ok(())
    }
}

#[async_trait]
impl Llm for ThrottleLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        self.gate().await?;
        self.inner.generate(messages, options).await
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        self.gate().await?;
        self.inner
            .create_structured_output_with_messages_raw(messages, json_schema, options)
            .await
    }

    /// Overridden so the caller's typed `validator` reaches the wrapped real
    /// adapter's repair loop (the trait default would drop it and delegate to the
    /// non-validated method, bypassing validation-retry). The gate still counts
    /// and throttles exactly one call per structured-output request.
    async fn create_structured_output_with_messages_raw_validated(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        options: Option<GenerationOptions>,
        validator: StructuredOutputValidator<'_>,
    ) -> LlmResult<Value> {
        self.gate().await?;
        self.inner
            .create_structured_output_with_messages_raw_validated(
                messages,
                json_schema,
                options,
                validator,
            )
            .await
    }

    async fn transcribe_image(
        &self,
        image_bytes: &[u8],
        mime_type: &str,
        options: Option<GenerationOptions>,
    ) -> LlmResult<String> {
        self.gate().await?;
        self.inner
            .transcribe_image(image_bytes, mime_type, options)
            .await
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn supports_function_calling(&self) -> bool {
        self.inner.supports_function_calling()
    }

    fn max_context_length(&self) -> u32 {
        self.inner.max_context_length()
    }

    fn supports_vision(&self) -> bool {
        self.inner.supports_vision()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable"
    )]
    use super::*;
    use serde_json::json;

    use crate::types::MessageRole;

    // A trivial always-succeeding LLM. A local stub avoids the dev-dependency
    // cycle described in `recording.rs` (cognee-test-utils depends on cognee-llm
    // without the `mock` feature).
    struct OkLlm;

    #[async_trait]
    impl Llm for OkLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Ok(GenerationResponse {
                content: "ok".to_string(),
                model: "ok-llm".to_string(),
                usage: None,
                finish_reason: Some("stop".to_string()),
            })
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            Ok(json!({"nodes": [], "relationships": []}))
        }

        fn model(&self) -> &str {
            "ok-llm"
        }
    }

    fn msgs() -> Vec<Message> {
        vec![Message {
            role: MessageRole::User,
            content: "hi".to_string(),
        }]
    }

    async fn call(t: &ThrottleLlm) -> LlmResult<GenerationResponse> {
        t.generate(msgs(), None).await
    }

    #[tokio::test]
    async fn counts_every_call_when_unlimited() {
        let t = ThrottleLlm::new(Arc::new(OkLlm), ThrottleConfig::default());
        for _ in 0..5 {
            call(&t).await.expect("unlimited call succeeds");
        }
        assert_eq!(
            t.metrics(),
            ThrottleMetrics {
                total_calls: 5,
                allowed: 5,
                rate_limited: 0,
            }
        );
    }

    #[tokio::test]
    async fn counts_structured_and_generate_paths() {
        let t = ThrottleLlm::new(Arc::new(OkLlm), ThrottleConfig::default());
        t.generate(msgs(), None).await.expect("generate");
        let schema = json!({"type": "object"});
        t.create_structured_output_with_messages_raw(msgs(), &schema, None)
            .await
            .expect("structured");
        assert_eq!(t.metrics().total_calls, 2);
        assert_eq!(t.metrics().allowed, 2);
    }

    // Returns different markers per method so a test can tell which inner method
    // a wrapper delegated to.
    struct MarkerLlm;

    #[async_trait]
    impl Llm for MarkerLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            unimplemented!()
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            Ok(json!({"marker": "raw"}))
        }

        async fn create_structured_output_with_messages_raw_validated(
            &self,
            _messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
            _validator: crate::llm_trait::StructuredOutputValidator<'_>,
        ) -> LlmResult<Value> {
            Ok(json!({"marker": "validated"}))
        }

        fn model(&self) -> &str {
            "marker"
        }
    }

    #[tokio::test]
    async fn validated_path_delegates_to_inner_validated_and_counts_once() {
        // #2: ThrottleLlm must forward the validator to the inner adapter's
        // `_validated` (not the trait default, which would call `_raw` and bypass
        // validation-retry), while still counting exactly one gated call.
        let t = ThrottleLlm::new(Arc::new(MarkerLlm), ThrottleConfig::default());
        let schema = json!({"type": "object"});
        let validate = |_: &Value| Ok(());
        let value = t
            .create_structured_output_with_messages_raw_validated(msgs(), &schema, None, &validate)
            .await
            .expect("validated delegation");
        assert_eq!(value, json!({"marker": "validated"}));
        assert_eq!(t.metrics().total_calls, 1);
        assert_eq!(t.metrics().allowed, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn enforces_rpm_budget_within_window() {
        let t = ThrottleLlm::new(
            Arc::new(OkLlm),
            ThrottleConfig {
                rpm_limit: Some(3),
                per_call_latency: None,
            },
        );
        // First 3 succeed, the next 2 are rejected with a simulated 429.
        for _ in 0..3 {
            call(&t).await.expect("within budget");
        }
        for _ in 0..2 {
            let err = call(&t).await.expect_err("over budget must 429");
            assert!(matches!(err, LlmError::RateLimitExceeded(_)));
        }
        assert_eq!(
            t.metrics(),
            ThrottleMetrics {
                total_calls: 5,
                allowed: 3,
                rate_limited: 2,
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn budget_refills_as_the_window_slides() {
        let t = ThrottleLlm::new(
            Arc::new(OkLlm),
            ThrottleConfig {
                rpm_limit: Some(2),
                per_call_latency: None,
            },
        );
        call(&t).await.expect("1st");
        call(&t).await.expect("2nd");
        call(&t).await.expect_err("3rd exceeds budget");

        // Advance past the window so the first two timestamps are evicted.
        tokio::time::advance(WINDOW + Duration::from_secs(1)).await;

        call(&t).await.expect("budget refilled after window");
        assert_eq!(t.metrics().allowed, 3);
        assert_eq!(t.metrics().rate_limited, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn injects_configured_latency() {
        let t = ThrottleLlm::new(
            Arc::new(OkLlm),
            ThrottleConfig {
                rpm_limit: None,
                per_call_latency: Some(Duration::from_millis(200)),
            },
        );
        let start = Instant::now();
        call(&t).await.expect("call with latency");
        // Under paused time, the sleep advances the clock by exactly the latency.
        assert_eq!(start.elapsed(), Duration::from_millis(200));
    }
}
