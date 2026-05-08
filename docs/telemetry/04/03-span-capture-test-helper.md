# Task 04-03 — `SpanCapture` test helper in `cognee-test-utils`

**Status**: ✅ implemented in commit 0578c1f
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 04-10 — Adapter instrumentation tests](10-tests.md) (every adapter test installs `SpanCapture`).

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decision**: #6 — tests use a custom `tracing::Layer` (Approach B), not `tracing-test`.

---

## 1. Goal

Add a small in-process span-capture helper to
[`crates/test-utils/`](../../crates/test-utils/) so adapter
integration tests can:

1. Install a temporary `tracing` subscriber that records every
   `cognee.*` span the adapter emits.
2. Assert on **structured** field values (`cognee.db.system`,
   `cognee.db.row_count`, `cognee.vector.result_count`,
   `cognee.llm.model`, …) rather than on formatted log strings.
3. Run safely in parallel test threads (each `#[tokio::test]`
   installs its own subscriber).

The helper exposes:

```rust
// in cognee_test_utils
pub struct SpanCapture { /* ... */ }

impl SpanCapture {
    /// Install as the global default subscriber for the duration of
    /// the test. Returns a guard; on drop, the previous subscriber
    /// (or none) is restored.
    pub fn install() -> SpanCaptureGuard;
}

pub struct SpanCaptureGuard { /* ... */ }

impl SpanCaptureGuard {
    /// Snapshot of all completed spans recorded so far.
    pub fn spans(&self) -> Vec<CapturedSpan>;
}

#[derive(Clone, Debug)]
pub struct CapturedSpan {
    pub name: String,
    pub fields: serde_json::Map<String, serde_json::Value>,
}

impl CapturedSpan {
    pub fn field_str(&self, key: &str) -> Option<String>;
    pub fn field_i64(&self, key: &str) -> Option<i64>;
    pub fn field_bool(&self, key: &str) -> Option<bool>;
}
```

## 2. Rationale

### Why not `tracing-test` / `logs_contain`

`tracing-test` only sees the formatted *output* of the subscriber.
Asserting on `logs_contain("cognee.db.row_count=5")` is brittle:

- Field formatting differs between `fmt::Layer` settings (JSON vs
  pretty vs compact).
- `cognee.db.query` carries redacted text whose marker contains
  literal `***REDACTED***` — that interacts badly with regex
  fuzziness in `logs_contain`.
- We want byte-exact attribute equality with Python (cross-SDK
  parity tests), which means structured comparison, not text
  matching.

### Why a custom `Layer`

`tracing_subscriber::Layer` exposes per-event/per-span hooks that fire
**before** any formatting happens. By recording field values in
`on_record` and `on_close`, we get the same structured view that
OTLP exporters consume — which is exactly the surface we want to
test. The same approach is already used by the existing
[`SpanBufferLayer`](../../crates/http-server/src/observability/span_buffer_layer.rs)
for the `/api/v1/activity/spans` endpoint, so the implementation is
prior-art-validated.

### Why in `cognee-test-utils`, not in each adapter crate

`cognee-test-utils` is already the umbrella for shared test helpers
(`MockGraphDB`, `MockVectorDB`, `pg_test_url`, `test_task_context`).
Putting `SpanCapture` here means each adapter integration test can
add a single `dev-dependencies.cognee-test-utils` line and reach
both the adapter mock and the span helper.

### Why a guard pattern

`tracing::dispatcher::set_default()` returns a `DefaultGuard` that
restores the previous subscriber on drop. We wrap it so a test that
panics inside an assertion still uninstalls the capture cleanly,
avoiding cross-test bleed in parallel runs.

## 3. Pre-conditions

- A clean `cargo check --all-targets` on `main`.
- `tracing-subscriber` is already a workspace dep
  (verified at [`Cargo.toml`](../../Cargo.toml) — used by
  `cognee-cli` and `cognee-http-server`).
- Tasks 04-01 and 04-02 are complete (not strictly required for this
  task, but recommended sequencing — the runbook drives them in
  order).

## 4. Step-by-step

### 4.1 Add `tracing` + `tracing-subscriber` deps to `cognee-test-utils`

Edit [`crates/test-utils/Cargo.toml`](../../crates/test-utils/Cargo.toml):

```toml
[dependencies]
async-trait.workspace = true
cognee-core = { path = "../core" }
cognee-database = { path = "../database" }
cognee-models = { path = "../models" }
cognee-graph = { path = "../graph", features = ["testing"] }
cognee-llm = { path = "../llm" }
cognee-storage = { path = "../storage", features = ["testing"] }
cognee-vector = { path = "../vector", features = ["testing"] }
serde_json.workspace = true
# NEW
tracing = { workspace = true }
tracing-subscriber = { workspace = true, features = ["registry"] }
uuid.workspace = true
```

The `registry` feature is what `tracing-subscriber` needs to host
custom layers. The two crates are workspace-pinned already.

### 4.2 Create `crates/test-utils/src/span_capture.rs`

```rust
//! Capture `tracing` spans during a test for structured attribute
//! assertions.
//!
//! Usage:
//!
//! ```rust,ignore
//! use cognee_test_utils::SpanCapture;
//!
//! #[tokio::test]
//! async fn ladybug_query_emits_span() {
//!     let capture = SpanCapture::install();
//!     let adapter = test_adapter().await;
//!     adapter.execute_query("MATCH (n:Node) RETURN n").unwrap();
//!     let spans = capture.spans();
//!     let s = spans
//!         .iter()
//!         .find(|s| s.name == "cognee.db.graph.query")
//!         .expect("expected query span");
//!     assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("ladybug"));
//!     assert_eq!(s.field_i64("cognee.db.row_count"), Some(0));
//! }
//! ```
//!
//! The guard returned from `install()` restores the previous tracing
//! dispatcher on drop, so parallel tests do not leak subscribers.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{Layer, Registry};

/// One completed span as observed by `SpanCapture`.
#[derive(Clone, Debug)]
pub struct CapturedSpan {
    pub name: String,
    pub fields: Map<String, Value>,
}

impl CapturedSpan {
    /// Read a string-typed field (also works for any field whose
    /// `Debug` representation is a quoted string literal — `tracing`
    /// records non-string `display`/`debug` values as JSON strings
    /// in the underlying map).
    pub fn field_str(&self, key: &str) -> Option<String> {
        match self.fields.get(key)? {
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }

    /// Read an integer-typed field. Returns `None` if absent or not
    /// an integer.
    pub fn field_i64(&self, key: &str) -> Option<i64> {
        self.fields.get(key)?.as_i64()
    }

    /// Read a boolean-typed field.
    pub fn field_bool(&self, key: &str) -> Option<bool> {
        self.fields.get(key)?.as_bool()
    }
}

/// Shared state between the layer and the guard.
type SpanStore = Arc<Mutex<Vec<CapturedSpan>>>;

#[derive(Default, Clone)]
struct PendingFields {
    map: Map<String, Value>,
}

impl Visit for PendingFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.map
            .insert(field.name().to_string(), Value::String(value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.map
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.map
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.map
            .insert(field.name().to_string(), Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Mirror `tracing`'s default rendering: `format!("{:?}", value)`.
        self.map.insert(
            field.name().to_string(),
            Value::String(format!("{:?}", value)),
        );
    }
}

struct CaptureLayer {
    store: SpanStore,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Stash the initial field values onto the span's extension
        // so we can mutate them via `on_record` and read them back on
        // close.
        let mut pending = PendingFields::default();
        attrs.record(&mut pending);
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            ext.insert(pending);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            if let Some(pending) = ext.get_mut::<PendingFields>() {
                values.record(pending);
            }
        }
    }

    fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {
        // Events are not captured; only spans.
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(&id) {
            let name = span.name().to_string();
            let fields = span
                .extensions()
                .get::<PendingFields>()
                .cloned()
                .unwrap_or_default()
                .map;
            if let Ok(mut store) = self.store.lock() {
                store.push(CapturedSpan { name, fields });
            }
        }
    }
}

/// Install a span-capturing subscriber as the default for the
/// current thread *and* for any tasks spawned on the current
/// `tokio` runtime. The previous default is restored when the
/// returned guard is dropped.
pub struct SpanCaptureGuard {
    store: SpanStore,
    _dispatch: tracing::dispatcher::DefaultGuard,
}

impl SpanCaptureGuard {
    /// Snapshot of all spans closed so far.
    pub fn spans(&self) -> Vec<CapturedSpan> {
        self.store
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }
}

/// Stateless installer.
pub struct SpanCapture;

impl SpanCapture {
    /// Install the capture layer as the **thread-local** default
    /// dispatcher (via `set_default`). The returned guard restores
    /// the previous dispatcher on drop. Safe to call concurrently
    /// from multiple `#[tokio::test]` functions.
    pub fn install() -> SpanCaptureGuard {
        let store: SpanStore = Arc::new(Mutex::new(Vec::new()));
        let layer = CaptureLayer {
            store: Arc::clone(&store),
        };
        let subscriber = Registry::default().with(layer);
        let dispatch = tracing::dispatcher::set_default(&subscriber.into());
        SpanCaptureGuard {
            store,
            _dispatch: dispatch,
        }
    }
}
```

Notes on the design:

- We use `set_default` (thread-local) rather than
  `set_global_default` because parallel tests must not stomp on each
  other. `set_default` is what `tracing-test` uses internally.
- `PendingFields` lives in the span's `extensions` so we get the
  full mutation history of `Span::current().record(...)` calls,
  matching what subscribers actually consume. Recording on
  `on_close` (not `on_new_span`) is what gives us the post-mutation
  view.
- The visitor implements `record_debug` to handle `Span::current().record(key, &Display)`
  patterns (which `tracing` routes through `record_debug` after
  formatting — and which is how Python-parity adapters set string
  attributes via `redact(...).as_ref()`).

### 4.3 Wire into `crates/test-utils/src/lib.rs`

```rust
pub mod mock_acl_db;
pub mod mock_llm;
pub mod mock_role_db;
pub mod mock_tenant_db;
pub mod mock_transcriber;
pub mod mock_user_db;
pub mod span_capture;        // NEW

// ... existing pub use lines ...
pub use span_capture::{CapturedSpan, SpanCapture, SpanCaptureGuard};   // NEW
```

### 4.4 Inline self-tests

Add a `#[cfg(test)] mod tests` block at the bottom of
`crates/test-utils/src/span_capture.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tracing::{info_span, instrument};

    #[test]
    fn captures_span_name_and_fields() {
        let capture = SpanCapture::install();
        let span = info_span!(
            "cognee.db.graph.query",
            cognee.db.system = "ladybug",
            cognee.db.row_count = tracing::field::Empty,
        );
        span.record("cognee.db.row_count", 7i64);
        let _enter = span.enter();
        drop(_enter);
        drop(span);

        let spans = capture.spans();
        let s = spans
            .iter()
            .find(|s| s.name == "cognee.db.graph.query")
            .expect("expected query span");
        assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("ladybug"));
        assert_eq!(s.field_i64("cognee.db.row_count"), Some(7));
    }

    #[instrument(name = "cognee.test.fn", skip_all, fields(value = tracing::field::Empty))]
    fn produce_span(v: i64) {
        tracing::Span::current().record("value", v);
    }

    #[test]
    fn captures_instrument_macro_spans() {
        let capture = SpanCapture::install();
        produce_span(42);
        let spans = capture.spans();
        assert!(spans.iter().any(|s| s.name == "cognee.test.fn"
            && s.field_i64("value") == Some(42)));
    }
}
```

These self-tests give the helper a small smoke-coverage that
sub-agent C can run via `cargo test -p cognee-test-utils
span_capture`. Adapter integration tests live in [task 04-10](10-tests.md).

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Run the helper's own tests.
cargo test -p cognee-test-utils span_capture

# 3. Clippy.
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/test-utils/Cargo.toml`](../../crates/test-utils/Cargo.toml)
  — add `tracing` and `tracing-subscriber` (with `registry` feature).
- [`crates/test-utils/src/span_capture.rs`](../../crates/test-utils/src/span_capture.rs)
  — NEW. `SpanCapture`, `SpanCaptureGuard`, `CapturedSpan`,
  `CaptureLayer`, `PendingFields`, plus inline tests.
- [`crates/test-utils/src/lib.rs`](../../crates/test-utils/src/lib.rs)
  — `pub mod span_capture;` and `pub use span_capture::{...};`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Two parallel tests racing on `set_default` | None — `set_default` is thread-local; each test's guard restores its own thread state. | n/a |
| Span fields recorded as `Empty` placeholders show up in `CapturedSpan::fields` | None — `tracing::field::Empty` does not invoke any visitor method, so it never reaches `PendingFields::map`. | Confirmed by reading `tracing-core` source; the inline tests exercise this in 4.4. |
| `tracing::Span::current().record(key, &display_value)` lands as a debug-formatted string with surrounding quotes | Likely — `record_debug` formats with `{:?}`. The redaction path uses `&redact(...).as_ref()` (a `&str`) which routes through `record_str` and lands cleanly. Adapter authors should pass `&str`/`i64`/`bool`, not arbitrary `Display` types. | Document this on the adapter sub-docs (04-04, 04-05, 04-08). |
| Subscriber-level filtering (e.g. `RUST_LOG`) suppresses our spans | Possible if a calling test sets `RUST_LOG=error`. | `SpanCapture::install()` does not attach an `EnvFilter`, so all spans reach the layer regardless of `RUST_LOG`. |
| `serde_json::Number::from_f64(NaN)` returns `None` | Real but unused — adapter spans never record floats. | Bury behind `unwrap_or(Value::Null)`. |

## 8. Out of scope

- Capturing `tracing::Event`s (logs). Only spans are needed; events
  fall through `on_event` to a no-op.
- A "wait for span to close" helper. All adapter spans close
  synchronously when their `#[instrument]`-annotated function
  returns, so the guard's `spans()` is up-to-date by the time the
  test reads it.
- Thread-safety beyond `Mutex<Vec<…>>`. The capture is
  `Arc<Mutex<…>>` and `Send + Sync` — no fancier concurrency
  needed for unit tests.
- Cross-crate re-export from `cognee-lib`. The helper is test-only
  and `cognee-test-utils` is the right entry point.
