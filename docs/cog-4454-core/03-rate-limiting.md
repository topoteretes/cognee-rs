# Gap 3 — Rate Limiting

**Parent:** [../cog-4454-core-implementation-plan.md](../cog-4454-core-implementation-plan.md)
**Effort:** Medium
**Order:** Last (touches the executor retry loop and `ExecEnv` plumbing)
**Status:** ☑ Implemented (b709efd)

## Goal

Give pipeline tasks a way to throttle external calls — LLM APIs with request
quotas, web scraping that must respect per-host limits — via a reusable,
object-safe `RateLimiter` abstraction configurable at both the pipeline and the
per-task level.

## Python reference

There is no unified limiter in Python; instead:

- `cognee/tasks/web_scraper/default_url_crawler.py` has `_respect_rate_limit()`
  that sleeps to honor per-domain crawl delays.
- LLM tasks lean on `retry_delay_factor` to back off after hitting 429s.

Rust already has exponential backoff (`RetryPolicy::Limited` +
`RetryDelay::Exponential`, `pipeline.rs:19–76`) but **no proactive throttle**:
nothing limits how fast or how many calls go out before the first failure. This
gap adds that proactive control.

## Design rationale

### Object-safe trait + two bundled implementations

Per the project's trait-abstraction convention (`StorageTrait`, `Llm`, etc.), we
add a `Send + Sync` async trait so callers hold `Arc<dyn RateLimiter>` and swap
implementations:

- **`SemaphoreLimiter`** — caps *concurrent in-flight* calls (e.g. "≤ 4 LLM
  requests at once"). Backed by `tokio::sync::Semaphore`. The permit is released
  when the guard drops, i.e. when the call completes.
- **`TokenBucketLimiter`** — caps *throughput* (e.g. "≤ 60 requests/minute").
  Classic token bucket: a `Semaphore` holding up to `capacity` permits, refilled
  by a background task at `refill_per_sec`. `acquire()` waits for a token and
  forgets it (the refiller adds tokens back over time).

These compose: concurrency *and* throughput limits can be layered by wrapping,
but most tasks need only one.

### Acquire semantics: concurrency vs. throughput

The two limiters differ in whether `acquire()` returns a guard:

- For **throughput** (`TokenBucketLimiter`) the call should *consume* a token and
  return — there is no "release," the refiller restores tokens over time. So
  `acquire()` returns `()`.
- For **concurrency** (`SemaphoreLimiter`) the permit must be *held* for the
  duration of the call, then released. That requires a guard with a lifetime.

To keep the trait object-safe and uniform we adopt the simplest contract that
covers both: **`acquire(&self)` returns `()`** and the limiter manages its own
release internally. `SemaphoreLimiter` implements "hold for the call" by having
`acquire()` wait for a permit and then **spawn a short-lived release** is *not*
viable (the limiter can't know when the call ends). Therefore:

> **Decision:** the trait models *admission throttling* (rate of starts), not
> concurrency-with-hold. `acquire()` returns `()` and means "you may now start a
> call." `SemaphoreLimiter` uses a permit-per-time-window pattern identical to
> the token bucket but sized by concurrency. For true hold-until-done
> concurrency limiting, prefer `Pipeline::with_concurrency(n)` (already exists,
> `pipeline.rs:170`), which bounds data-item parallelism via `buffer_unordered`.

This keeps the trait object-safe (no associated guard type, no lifetimes) and
avoids overlapping responsibility with the existing `with_concurrency`. Document
this division clearly so callers pick the right tool:
- **proactive request-rate throttle** → `RateLimiter` (this gap)
- **bounded item-level parallelism** → `Pipeline::with_concurrency`
- **reactive backoff on failure** → `RetryPolicy` (existing)

### Acquire inside the retry loop, not before it

Each retry attempt is a *new* external call, so each attempt must pass through
the limiter. We therefore acquire **inside** `call_with_retry`'s loop
(before `task.call`), not once in `execute_from`. Resolution order — per-task
overrides pipeline-wide:

```
effective = info.rate_limiter.as_ref().or(env.rate_limiter)
```

## Step-by-step

### Step 1 — Create `crates/core/src/rate_limiter.rs`

```rust
//! Proactive request-rate throttling for pipeline tasks.
//!
//! See `docs/cog-4454-core/03-rate-limiting.md` for the design rationale and how
//! this differs from `Pipeline::with_concurrency` (item parallelism) and
//! `RetryPolicy` (reactive backoff).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Semaphore;

/// Admission throttle: `acquire().await` returns when the caller is permitted to
/// start an external call. Object-safe; hold as `Arc<dyn RateLimiter>`.
#[async_trait]
pub trait RateLimiter: Send + Sync {
    async fn acquire(&self);
}

/// Caps the number of starts allowed per refill window.
///
/// `capacity` tokens are available; a background task adds one token every
/// `1 / refill_per_sec` seconds, up to `capacity`. `acquire()` waits for a token
/// and consumes it.
pub struct TokenBucketLimiter {
    semaphore: Arc<Semaphore>,
}

impl TokenBucketLimiter {
    /// `capacity`: max burst. `refill_per_sec`: tokens restored per second.
    pub fn new(capacity: usize, refill_per_sec: f64) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(refill_per_sec > 0.0, "refill_per_sec must be > 0");
        let semaphore = Arc::new(Semaphore::new(capacity));
        let refill = semaphore.clone();
        let interval = Duration::from_secs_f64(1.0 / refill_per_sec);
        // Background refiller. Stops when the semaphore (last Arc) is dropped.
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                // Only add back up to `capacity`.
                if refill.available_permits() < capacity {
                    refill.add_permits(1);
                }
                // Stop if we are the only holder left (limiter dropped).
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
        // `forget()` consumes the permit; the refiller restores tokens over time.
        // Semaphore is never closed, so acquire cannot error.
        let permit = self
            .semaphore
            .acquire()
            .await
            .expect("rate-limiter semaphore is never closed");
        permit.forget();
    }
}

/// Concurrency-style admission limit: at most `max` starts may be outstanding
/// between refills. Distinct from `Pipeline::with_concurrency` (see module docs).
pub struct SemaphoreLimiter {
    inner: TokenBucketLimiter,
}

impl SemaphoreLimiter {
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
```

> **Refiller lifecycle:** the background task self-terminates one tick after the
> limiter is dropped (`Arc::strong_count == 1`). This avoids leaking a task per
> limiter. If a stricter shutdown is needed later, store a `tokio::task::JoinHandle`
> + `Notify` and abort on `Drop`; not required for the initial implementation.

### Step 2 — Register & export in `crates/core/src/lib.rs`

```rust
pub mod rate_limiter;
```

```rust
pub use rate_limiter::{RateLimiter, SemaphoreLimiter, TokenBucketLimiter};
```

### Step 3 — Add `rate_limiter` to `TaskInfo` (`crates/core/src/task.rs`)

Add `use std::sync::Arc;` if not already present, and
`use crate::rate_limiter::RateLimiter;`.

Add the field + update both constructors (mirror the `enriches` change from
[Gap 2](./02-enrichment-mode.md) — `TaskInfo::new` and the `parallel()` literal),
defaulting to `None`:

```rust
pub struct TaskInfo {
    // … existing fields, incl. `enriches` …
    /// Per-task rate limiter. Overrides the pipeline-level limiter. `None`
    /// inherits the pipeline limiter (or no throttling if that is also `None`).
    pub rate_limiter: Option<Arc<dyn RateLimiter>>,
}
```

Builder:

```rust
pub fn with_rate_limiter(mut self, rl: Arc<dyn RateLimiter>) -> Self {
    self.rate_limiter = Some(rl);
    self
}
```

### Step 4 — Add `rate_limiter` to `Pipeline` (`crates/core/src/pipeline.rs`)

Add the field to `Pipeline` (after `telemetry_settings`, line 119):

```rust
pub rate_limiter: Option<Arc<dyn RateLimiter>>,
```

Default in `Pipeline::new` (line 123) → `rate_limiter: None`. Add `rate_limiter:
None` to the `PipelineBuilder::build` literal (line 340) — the typed builder does
not expose a setter; callers set it on the built `Pipeline` via the method below.

Builder method on `Pipeline` (alongside `with_concurrency`):

```rust
/// Set a pipeline-wide proactive rate limiter. Individual tasks may override
/// it via [`TaskInfo::with_rate_limiter`].
pub fn with_rate_limiter(mut self, rl: Arc<dyn RateLimiter>) -> Self {
    self.rate_limiter = Some(rl);
    self
}
```

Add `use crate::rate_limiter::RateLimiter;` to the imports.

### Step 5 — Thread it through `ExecEnv`

Add to the `ExecEnv<'a>` struct (line 987):

```rust
/// Pipeline-wide rate limiter; per-task limiters override it.
rate_limiter: Option<&'a Arc<dyn RateLimiter>>,
```

Populate it in the `ExecEnv { … }` construction (line 774):

```rust
rate_limiter: pipeline.rate_limiter.as_ref(),
```

### Step 6 — Pass the effective limiter into `call_with_retry`

In `execute_from`, compute the effective limiter and pass it to the call (the
existing `call_with_retry(&info.task, …)` at line 1089):

```rust
let effective_rl = info.rate_limiter.as_ref().or(env.rate_limiter);

let resolved = call_with_retry(
    &info.task,
    input,
    first_index,
    task_name,
    data_id.as_deref(),
    info.summary_template.as_deref(),
    &prov_inputs,
    effective_rl,   // ← new argument
    env,
)
.await?;
```

Add the parameter to `call_with_retry`'s signature (line 1310). It already has
`#[allow(clippy::too_many_arguments)]`, so adding one more is fine:

```rust
async fn call_with_retry(
    task: &Task,
    input: Arc<dyn Value>,
    task_index: usize,
    task_name: Option<&str>,
    data_id: Option<&str>,
    summary_template: Option<&str>,
    prov_inputs: &ProvenanceInputs<'_>,
    rate_limiter: Option<&Arc<dyn RateLimiter>>,   // ← new
    env: &ExecEnv<'_>,
) -> Result<Resolved, ExecutionError> {
```

Acquire inside the retry loop, immediately before `let call = task.call(…)` at
line 1349:

```rust
for attempt in 1..=max_attempts {
    // Proactive throttle: every attempt is a fresh external call.
    if let Some(rl) = rate_limiter {
        rl.acquire().await;
    }
    let call = task.call(input.clone(), Arc::clone(&task_ctx));
    // … unchanged …
}
```

### Step 7 — Rate-limit batch-task calls (`dispatch_batch`)

Batch tasks bypass `call_with_retry` (design note at line 1144), so they must
acquire separately. In `dispatch_batch`, before
`let call = next_info.task.call_batch(&batch, env.ctx.clone());` (line 1175):

```rust
if let Some(rl) = next_info.rate_limiter.as_ref().or(env.rate_limiter) {
    rl.acquire().await;
}
```

This guarantees both single-value and batch external calls honor the limiter.

## Test plan

New integration test file `crates/core/tests/rate_limiting.rs` (use
`tokio::time` — keep assertions coarse to avoid flakiness):

1. **Throughput cap** — `TokenBucketLimiter::new(2, 2.0)` (2 capacity, 2/sec) on a
   pipeline of 6 trivial tasks/items. Assert total wall-clock ≥ ~2 s (6 calls,
   first 2 free, remaining 4 at 2/sec). Use a generous lower bound.
2. **Per-task override** — a pipeline limiter of 100/sec and one task overriding
   with `TokenBucketLimiter::new(1, 1.0)`; assert that task is the bottleneck.
3. **Acquire per retry** — a flaky task (fails twice, succeeds on attempt 3) under
   `RetryPolicy::Limited` and a counting fake `RateLimiter` (test-only struct with
   an `AtomicUsize`); assert `acquire` was called 3 times (once per attempt).
4. **No limiter = no throttle** — sanity: pipeline without a limiter runs without
   added delay.

Unit tests inside `rate_limiter.rs`:
- `TokenBucketLimiter` lets `capacity` immediate acquisitions through, then blocks
  until the next refill tick.

## Acceptance criteria

- [ ] `crates/core/src/rate_limiter.rs` with `RateLimiter` + `TokenBucketLimiter` + `SemaphoreLimiter`, exported from `lib.rs`
- [ ] `TaskInfo.rate_limiter` + `with_rate_limiter`; all constructors updated
- [ ] `Pipeline.rate_limiter` + `with_rate_limiter`; `Pipeline::new` and `build()` updated
- [ ] `ExecEnv` carries the limiter; `call_with_retry` acquires per attempt; `dispatch_batch` acquires per batch call
- [ ] Integration + unit tests pass (including the per-retry acquire count)
- [ ] `scripts/check_all.sh` green (run after this final gap)

## Follow-up consumers (out of scope here, but the reason this exists)

Once the API lands, wire it into the real external-call tasks:
- LLM extraction tasks in `cognee-cognify` → pipeline-level `TokenBucketLimiter`
  sized to the model's RPM quota.
- URL crawler tasks in `cognee-ingestion` → per-task limiter for per-host crawl
  delay (Python's `_respect_rate_limit` parity).

These are separate tickets; this gap only delivers the core primitive + wiring.
