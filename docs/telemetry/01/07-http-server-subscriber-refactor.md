# Action item 7 — Refactor `cognee-http-server` subscriber composition and store `TelemetryGuard` on `AppState`

- **Status**: Implemented in commit 56433e5
- **Owner / Dependencies:**
  - **Depends on:**
    - Task [`02` — bootstrap the `cognee-observability` crate](./02-observability-crate-scaffold.md) (provides `TelemetryGuard`, `init_telemetry`, the noop fallback layer, the `SettingsView` trait)
    - Task [`03` — `telemetry` feature wiring across crates](./03-cognee-lib-feature-wiring.md) (adds the `telemetry` feature to `crates/http-server/Cargo.toml` plus the **optional** `cognee-observability` dependency)
    - Task [`04` — `init_telemetry` implementation](./04-init-telemetry-implementation.md) (the actual OTEL provider this task installs and the `BoxedTelemetryLayer<Registry>` return shape that constrains layer ordering)
    - Task [`05` — `cognee-lib` re-exports](./05-cognee-lib-reexports.md) (defines the public re-export surface; this task imports from `cognee-observability` directly because `cognee-http-server` intentionally does not depend on `cognee-lib`)
  - **Sibling references:**
    - Task [`06` — CLI subscriber refactor](./06-cli-subscriber-refactor.md) — same composition pattern (and same type-stacking constraint on the boxed `Layer<Registry>`); the CLI keeps the guard in a local `main()` binding rather than on a long-lived state struct.
    - Task [`09` — observability unit tests](./09-observability-unit-tests.md) — exercises the `init_telemetry` paths this task installs.
    - Task [`10` — OTEL export integration test](./10-otel-export-integration-test.md) — exercises the composition this task installs against a fake collector.
- **Anchor in parent:** action item 7 of [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md#action-items) ("Refactor the HTTP server subscriber"), bound by [Design decisions 1, 9, 10, 11](../01-otel-otlp-export.md#design-decisions-locked).

## Rationale

Today, the http-server subscriber in
[`crates/http-server/src/main.rs:100`](../../../crates/http-server/src/main.rs#L100)
composes only `EnvFilter → fmt → SpanBufferLayer`. After tasks 02–05 land
we have a `cognee_observability::init_telemetry::<Registry>(&settings)` helper that
returns `(BoxedTelemetryLayer<Registry>, TelemetryGuard)`, where the layer is either a real
`tracing-opentelemetry` bridge or `tracing_subscriber::layer::Identity`
when the `telemetry` feature is off (or the feature is on but neither
`COGNEE_TRACING_ENABLED` nor `OTEL_EXPORTER_OTLP_ENDPOINT` is set —
decision 2). This task wires that helper into the http-server entry
point.

Three constraints from the locked design decisions drive the shape of
the refactor:

1. **Decision 9 — guard ownership.** The flush-on-drop guard must live
   for the full server lifetime, so it goes on `AppState`. The CLI
   variant (task 06) holds the guard in a `main()` local because the
   CLI completes a single request and exits; the http-server, by
   contrast, runs until SIGTERM and must still flush the final batch
   *after* the last request handler returns. Putting the guard on
   `AppState` ties its lifetime to axum's state map, which axum drops
   automatically once `axum::serve(...)` returns.
2. **Decision 1 — feature off by default.** Both code paths (with and
   without `--features telemetry`) must compile and run. Because
   `init_telemetry` already returns `Identity` in the noop path,
   composition stays identical and the only thing the http-server
   needs to gate is the optional `cognee-observability` dep declared
   in task 03.
3. **Decision 10 — guard type name `TelemetryGuard`.** The field on
   `AppState` is called `telemetry_guard` and types as
   `Option<Arc<TelemetryGuard>>` for the noop+real symmetry described
   below.

### Subscriber composition order

Per the parent doc's [Subscriber composition](../01-otel-otlp-export.md#subscriber-composition)
section the order is

```
tracing-opentelemetry::layer → EnvFilter → fmt::layer (stdout) → SpanBufferLayer
```

There is one hard type-system constraint that pins this ordering: the
boxed `Layer<Registry>` returned by `init_telemetry::<Registry>` only
typechecks when slotted **directly above `Registry`**. Any other
position would require the boxed layer to satisfy `Layer<Layered<...>>`
for an arbitrarily nested subscriber type — a bound the `Box<dyn
Layer<Registry>>` shape does not provide. See task 06's Implementation
notes for the full type-stacking explanation; the same constraint
applies here verbatim, with the addition of the `SpanBufferLayer` as a
fourth layer at the end.

`tracing_subscriber::Layer` runs registered layers in the order they
were added: `Registry::default().with(A).with(B).with(C)` invokes
`A`'s callbacks first, then `B`'s, then `C`'s, for each event. The OTEL
bridge therefore observes spans before `EnvFilter`, `fmt`, and
`SpanBufferLayer`. Both the OTEL layer and `SpanBufferLayer`
independently read span metadata; neither mutates shared span state, so
the ordering between them is functionally equivalent for both sinks
today, but the OTEL-first slotting is forced by the type constraint and
matches the Python ordering (Python attaches
`SimpleSpanProcessor(otlp_exporter)` to the same provider as
`SimpleSpanProcessor(in_memory_exporter)`, so both observe spans on
commit; we mirror that with the OTEL bridge running first).

### Comparison with Python's `setup_tracing`

Python has no http-server tracing equivalent — `cognee` ships a
FastAPI app that relies on `opentelemetry-instrument` or a separate
ASGI middleware to install OTEL bridging. There is no Python analogue
of `SpanBufferLayer` *and* `TracerProvider` composed on the same
subscriber, because Python's logger/tracer wiring is global rather
than layered. **This composition is Rust-specific.** The closest
analogue is Python's `setup_tracing` attaching both the
`CogneeSpanExporter` (in-memory) and the OTLP exporter to the same
`TracerProvider`; we mirror the spirit of that ("both sinks see every
span") via two `tracing` layers on one `Registry`.

## Pre-conditions

- Task [`02`](./02-observability-crate-scaffold.md) merged: the
  `cognee-observability` crate exists with `init_telemetry::<S>(&dyn
  SettingsView) -> Result<(BoxedTelemetryLayer<S>, TelemetryGuard), TelemetryInitError>`
  and a `TelemetryGuard` whose `Drop` calls
  `provider.force_flush()` followed by `provider.shutdown()`. Under
  `not(feature = "telemetry")`, the boxed layer collapses to
  `tracing_subscriber::layer::Identity` and `TelemetryGuard` is a
  zero-sized noop. The crate also defines `SettingsView` (the
  borrow-only OTEL-fields view) in its public API.
- Task [`03`](./03-cognee-lib-feature-wiring.md) merged: the
  `crates/http-server/Cargo.toml` manifest now contains
  ```toml
  cognee-observability = { path = "../observability", optional = true }
  ```
  and a forwarding feature
  ```toml
  telemetry = ["dep:cognee-observability", "cognee-observability/telemetry", "cognee-core/telemetry"]
  ```
  Because `cognee-observability` is `optional = true`, every reference
  to its types in `cognee-http-server` source must be cfg-gated under
  `#[cfg(feature = "telemetry")]`.
- Task [`04`](./04-init-telemetry-implementation.md): `init_telemetry`
  actually builds an `SdkTracerProvider` with an OTLP exporter when
  the `telemetry` feature is on and the relevant env vars / settings
  are present.
- Task [`05`](./05-cognee-lib-reexports.md) optional: the
  `cognee_lib::observability` re-export exists. `cognee-http-server`
  does not import from `cognee-lib` (per task 03's manifest note: "the
  HTTP server intentionally does not depend on `cognee-lib`"); it
  reads the OTEL settings directly from environment variables via the
  new `EnvSettingsView` helper described below.

## Step-by-step

### 1. Add `EnvSettingsView` to `cognee-observability`

Because `cognee-http-server` does **not** depend on `cognee-lib`, it
cannot use `cognee_lib::Settings` to drive `init_telemetry`. The
cleanest fix is to add a tiny env-backed implementation of the
existing `SettingsView` trait inside `cognee-observability` itself, so
that any crate which already pulls in `cognee-observability` (e.g. the
HTTP server, future workers, examples) can call `init_telemetry`
without a `cognee-lib` round-trip.

Edit `crates/observability/src/settings.rs` (or, if the file grows, a
new sibling module re-exported from `settings.rs`) to add:

```rust
/// Snapshot of OTEL-relevant env vars usable as a `SettingsView`. Lets
/// crates that don't depend on `cognee-lib::Settings` (e.g. the HTTP
/// server) drive `init_telemetry` directly from the environment.
#[derive(Debug, Default, Clone)]
pub struct EnvSettingsView {
    tracing_enabled: bool,
    service_name: String,
    otlp_endpoint: String,
    otlp_headers: String,
    otlp_protocol: String,
    span_processor: String,
    traces_sampler: String,
    traces_sampler_arg: String,
}

impl EnvSettingsView {
    /// Read all eight OTEL env vars at construction time. Missing/empty
    /// vars fall back to the same defaults `cognee-lib::Settings` uses
    /// (mirror those defaults — keep them in one source of truth in
    /// this crate, e.g. `pub(crate) const` definitions, so a future
    /// drift between `cognee-lib::Settings::default()` and this view
    /// is caught by a unit test rather than silently desynchronising).
    pub fn from_env() -> Self { /* ... */ }
}

impl SettingsView for EnvSettingsView {
    fn tracing_enabled(&self) -> bool { self.tracing_enabled }
    fn service_name(&self) -> &str { &self.service_name }
    fn otlp_endpoint(&self) -> &str { &self.otlp_endpoint }
    fn otlp_headers(&self) -> &str { &self.otlp_headers }
    fn otlp_protocol(&self) -> &str { &self.otlp_protocol }
    fn span_processor(&self) -> &str { &self.span_processor }
    fn traces_sampler(&self) -> &str { &self.traces_sampler }
    fn traces_sampler_arg(&self) -> &str { &self.traces_sampler_arg }
}
```

The env vars to read (these names are taken verbatim from
`Settings::overlay_from_env()` in `crates/lib/src/config.rs`, so the
two views stay aligned):

| Env var | Field | Notes |
|---|---|---|
| `COGNEE_TRACING_ENABLED` | `tracing_enabled` | Lowercase-compare against `"true"`, `"1"`, `"yes"`. Default `false`. |
| `OTEL_SERVICE_NAME` | `service_name` | Default `"cognee"` (matches `Settings::default()`). |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `otlp_endpoint` | Default `""`. |
| `OTEL_EXPORTER_OTLP_HEADERS` | `otlp_headers` | Default `""`. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `otlp_protocol` | Default `"grpc"` (matches `Settings::default()`). |
| `OTEL_SPAN_PROCESSOR` | `span_processor` | Default `"batch"` (matches `Settings::default()`). |
| `OTEL_TRACES_SAMPLER` | `traces_sampler` | Default `""` (let SDK read its own env var). |
| `OTEL_TRACES_SAMPLER_ARG` | `traces_sampler_arg` | Default `""`. |

These defaults are cross-checked against
`crates/lib/src/config.rs::Settings::default()` lines 644–651 (the
`-- Observability --` block). Any future change to those defaults must
be mirrored in `EnvSettingsView::from_env()`'s fallback constants — a
unit test in `cognee-observability` should assert the two stay equal
by constructing both with empty env and comparing each field.

Then export the new type in `crates/observability/src/lib.rs`
alongside the existing public re-exports:

```rust
pub use settings::{EnvSettingsView, SettingsView};
```

### 2. Add `telemetry_guard` field to `AppState`

Edit
[`crates/http-server/src/state.rs`](../../../crates/http-server/src/state.rs).

The current struct (line 29) is

```rust
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<HttpServerConfig>,
    pub pipelines: Arc<dyn PipelineRunRegistry>,
    pub lib: Option<Arc<ComponentHandles>>,
    pub auth: Option<Arc<AuthContext>>,
    pub mailer: Arc<dyn Mailer>,
    pub health: Option<Arc<dyn crate::routers::health::HealthChecker>>,
    pub spans: Arc<SpanBuffer>,
    pub sync: Arc<SyncRegistry>,
}
```

Two complications:

- **`AppState` derives `Clone`.** axum requires `S: Clone` for
  `with_state(state)` and clones the state map per request handler
  invocation. A bare `TelemetryGuard` cannot be `Clone` — its `Drop`
  is the entire point of its existence; cloning it would call
  `provider.shutdown()` twice. Wrap it.
- **Drop semantics.** We want **exactly one** `Drop` to run, and we
  want it to run when the server stops. The simplest wrapper that
  satisfies both is `Arc<TelemetryGuard>`: clones share the inner
  guard, the `Drop` fires once when the *last* `Arc` is dropped, and
  no `Mutex` is needed because we never mutate the guard. The catch
  is "last `Arc` dropped" — see the Risks section about background
  tasks.

Because task 03 keeps `cognee-observability` strictly **optional**
behind the `telemetry` feature, the import and the field itself must
be cfg-gated:

```rust
// new import — only when the telemetry feature is on
#[cfg(feature = "telemetry")]
use cognee_observability::TelemetryGuard;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<HttpServerConfig>,
    pub pipelines: Arc<dyn PipelineRunRegistry>,
    pub lib: Option<Arc<ComponentHandles>>,
    pub auth: Option<Arc<AuthContext>>,
    pub mailer: Arc<dyn Mailer>,
    pub health: Option<Arc<dyn crate::routers::health::HealthChecker>>,
    pub spans: Arc<SpanBuffer>,
    pub sync: Arc<SyncRegistry>,

    /// Flush-on-drop guard for the OpenTelemetry exporter. Held only for its
    /// `Drop` side effect — calling `provider.force_flush()` then
    /// `provider.shutdown()` when the last `Arc` is released.
    ///
    /// Only present when the `telemetry` feature is enabled; under
    /// `not(feature = "telemetry")` `AppState` does not carry this field
    /// at all (and `cognee-observability` is not in the dep graph).
    /// `None` when built from `AppState::build` without an explicit
    /// guard (test paths, library embedders that manage telemetry
    /// themselves).
    #[cfg(feature = "telemetry")]
    pub telemetry_guard: Option<Arc<TelemetryGuard>>,
}
```

Update both `AppState::build` (line 88) and `AppState::build_with_db`
(line 188) to default `telemetry_guard: None` under the same cfg gate
(see the diff in step 5). Code paths that match on `AppState` literally
(currently zero in-tree) must mirror the cfg-gated initializer.

### 3. Refactor `init_tracing` to compose the OTEL layer

Replace the body of `init_tracing` in
[`crates/http-server/src/main.rs:100`](../../../crates/http-server/src/main.rs#L100).
The new function takes a `&dyn SettingsView` (constructed in `main`
via `EnvSettingsView::from_env()`) and returns
`Option<Arc<TelemetryGuard>>` so `main()` can attach the result to
`AppState`:

```rust
/// Build the layered subscriber:
///
/// ```
/// tracing-opentelemetry::layer  (real OTEL bridge or Identity)  ← must sit directly above Registry
///   → EnvFilter
///   → fmt::layer (stdout)
///   → SpanBufferLayer               (in-memory ring for /api/v1/activity/spans)
/// ```
///
/// Returns the `TelemetryGuard` produced by `cognee_observability::init_telemetry`
/// so the caller can install it on `AppState` (decision 9). When the OTEL
/// stack is disabled (feature off, or env vars unset — decision 2), the OTEL
/// layer collapses to `Identity` and the guard is a noop.
///
/// The OTEL layer's position is forced by the boxed `Layer<Registry>`
/// type returned by `init_telemetry::<Registry>` — see task 06's
/// Implementation notes for the full type-stacking explanation.
#[cfg(feature = "telemetry")]
fn init_tracing(
    settings: &cognee_observability::EnvSettingsView,
    spans: Arc<SpanBuffer>,
) -> Option<Arc<cognee_observability::TelemetryGuard>> {
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let span_buffer_layer = SpanBufferLayer::new((*spans).clone());

    let (telemetry_layer, telemetry_guard) =
        cognee_observability::init_telemetry::<Registry>(settings)
            .unwrap_or_else(|err| {
                tracing::warn!(?err, "telemetry init failed; continuing without OTEL");
                (
                    Box::new(tracing_subscriber::layer::Identity::new()),
                    cognee_observability::TelemetryGuard::noop(),
                )
            });

    // Composition order is forced by the boxed Layer<Registry> type:
    // telemetry_layer must sit directly above Registry. SpanBufferLayer
    // is a concrete generic layer and stacks on top freely.
    Registry::default()
        .with(telemetry_layer)
        .with(env_filter)
        .with(fmt_layer)
        .with(span_buffer_layer)
        .try_init()
        .map_err(|e| tracing::warn!(?e, "failed to install global tracing subscriber"))
        .ok();

    Some(Arc::new(telemetry_guard))
}
```

Two notes:

- The current `init_tracing` swallows the `try_init` error (`let _ =
  ...`) because tests may install a subscriber first. The refactor
  preserves that softness via `.map_err(...).ok()` so the binary
  startup path is best-effort consistent with today's behaviour.
- `init_telemetry`'s `Result<(_, TelemetryGuard), _>` is unwrapped via
  `unwrap_or_else` so a misconfigured OTLP endpoint cannot crash the
  server — we log and fall back to `Identity` + `TelemetryGuard::noop()`.
  The `noop()` constructor lives in `cognee-observability` (decision 1
  / task 02).

When the `telemetry` feature is **off**, define a stub `init_tracing`
that mirrors today's body (no OTEL layer, no guard, no return value):

```rust
#[cfg(not(feature = "telemetry"))]
fn init_tracing(spans: Arc<SpanBuffer>) {
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let span_buffer_layer = SpanBufferLayer::new((*spans).clone());

    let _ = Registry::default()
        .with(env_filter)
        .with(fmt_layer)
        .with(span_buffer_layer)
        .try_init();
}
```

### 4. Wire the guard into `main()`

Modify `main` in
[`crates/http-server/src/main.rs:48`](../../../crates/http-server/src/main.rs#L48)
to:

1. Construct `EnvSettingsView::from_env()` *before* installing the
   subscriber (decision 11). This reads `COGNEE_TRACING_ENABLED`,
   `OTEL_SERVICE_NAME`, `OTEL_EXPORTER_OTLP_ENDPOINT`,
   `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_PROTOCOL`,
   `OTEL_SPAN_PROCESSOR`, `OTEL_TRACES_SAMPLER`,
   `OTEL_TRACES_SAMPLER_ARG`.
2. Call `init_tracing(&settings, spans.clone())` (telemetry-on) or
   `init_tracing(spans.clone())` (telemetry-off) and bind the
   returned guard.
3. After `AppState::build`, set `state.telemetry_guard = guard`
   (telemetry-on path only).
4. Pass `state` to `cognee_http_server::run(addr, state)`.

The full revised `main.rs`:

```rust
//! Standalone `cognee-http-server` binary entry point.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use cognee_http_server::observability::{BufferConfig, SpanBuffer, SpanBufferLayer};
use cognee_http_server::{AppState, HttpServerConfig};

#[derive(Parser, Debug)]
#[command(
    name = "cognee-http-server",
    about = "Cognee HTTP server (FastAPI-compatible)",
    version
)]
struct Args {
    #[arg(long, env = "HTTP_API_HOST", default_value = "0.0.0.0")]
    host: String,
    #[arg(long, env = "HTTP_API_PORT", default_value_t = 8000)]
    port: u16,
    #[arg(long, env = "COGNEE_HTTP_CONFIG")]
    config: Option<std::path::PathBuf>,
    #[arg(long, env = "CORS_ALLOWED_ORIGINS")]
    cors_allowed_origins: Option<String>,
    #[arg(long, env = "ENV", default_value = "prod")]
    env: String,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let _ = dotenv::dotenv();

    // 1. Build the in-memory span buffer (still needed by both feature paths).
    let spans = Arc::new(SpanBuffer::new(BufferConfig::from_env()));

    // 2. Compose subscriber. With `telemetry` on, also install the OTEL
    //    bridge and capture the flush-on-drop guard.
    #[cfg(feature = "telemetry")]
    let telemetry_guard = {
        let settings = cognee_observability::EnvSettingsView::from_env();
        init_tracing(&settings, spans.clone())
    };
    #[cfg(not(feature = "telemetry"))]
    init_tracing(spans.clone());

    // 3. Parse CLI args.
    let args = Args::parse();

    // 4. Build HTTP config with CLI-flag overrides.
    let mut cfg = HttpServerConfig::from_env()
        .context("failed to load config from environment")?;
    cfg.host = args.host;
    cfg.port = args.port;
    if let Some(origins) = args.cors_allowed_origins {
        cfg.cors_allowed_origins = origins
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Ok(env_val) = args.env.parse() {
        cfg.env = env_val;
    }

    // 5. Build application state and attach the guard (decision 9).
    let mut state = AppState::build(cfg.clone())
        .await
        .context("failed to build AppState")?;
    state.spans = spans;
    #[cfg(feature = "telemetry")]
    {
        state.telemetry_guard = telemetry_guard;
    }

    // 6. Bind and serve. axum's graceful shutdown (already wired in
    //    `run` via `with_graceful_shutdown(shutdown_signal(state))`) drops
    //    the state when the future completes; that drops the last Arc<
    //    TelemetryGuard>, which calls provider.force_flush() + shutdown()
    //    so the final batch reaches the collector.
    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .context("invalid bind address")?;

    cognee_http_server::run(addr, state)
        .await
        .context("server error")?;

    Ok(())
}

#[cfg(feature = "telemetry")]
fn init_tracing(
    settings: &cognee_observability::EnvSettingsView,
    spans: Arc<SpanBuffer>,
) -> Option<Arc<cognee_observability::TelemetryGuard>> {
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let span_buffer_layer = SpanBufferLayer::new((*spans).clone());

    let (telemetry_layer, telemetry_guard) =
        cognee_observability::init_telemetry::<Registry>(settings)
            .unwrap_or_else(|err| {
                tracing::warn!(?err, "telemetry init failed; continuing without OTEL");
                (
                    Box::new(tracing_subscriber::layer::Identity::new()),
                    cognee_observability::TelemetryGuard::noop(),
                )
            });

    let _ = Registry::default()
        .with(telemetry_layer)
        .with(env_filter)
        .with(fmt_layer)
        .with(span_buffer_layer)
        .try_init();

    Some(Arc::new(telemetry_guard))
}

#[cfg(not(feature = "telemetry"))]
fn init_tracing(spans: Arc<SpanBuffer>) {
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let span_buffer_layer = SpanBufferLayer::new((*spans).clone());

    let _ = Registry::default()
        .with(env_filter)
        .with(fmt_layer)
        .with(span_buffer_layer)
        .try_init();
}
```

### 5. Confirm graceful shutdown drops the guard

[`crates/http-server/src/lib.rs:129`](../../../crates/http-server/src/lib.rs#L129)
already implements `shutdown_signal(state: AppState)` and
[`run` line 172](../../../crates/http-server/src/lib.rs#L172) wires it
into `axum::serve(...).with_graceful_shutdown(...)` under the `bin`
feature. The flow:

1. SIGINT/SIGTERM arrives → `shutdown_signal` resolves.
2. `lifecycle::on_shutdown(&state).await` runs.
3. axum's `serve` future completes; it owns no further references to
   `AppState`.
4. `run` returns to `main`, where `state` was moved into `run` via
   `cognee_http_server::run(addr, state)`. Both copies (the one held
   by `with_state(state)` inside `build_router`, and the one passed to
   `with_graceful_shutdown(shutdown_signal(state))`) drop together
   when `axum::serve` returns.
5. The last `Arc<TelemetryGuard>` is released, `Drop` runs,
   `provider.force_flush()` + `provider.shutdown()` complete before
   `main` returns and the runtime exits.

**Risk: clones held by background tasks.** If any handler does
`tokio::spawn(async move { ... use_state ... })` and the spawned task
outlives the server, that task holds a clone of `Arc<TelemetryGuard>`
and the guard's `Drop` is delayed until the task finishes. For
short-lived tasks this is fine; for the cloud-sync background loop
(`SyncRegistry`) and the pipeline-run background workers we should
**explicitly extract and drop the guard** during `lifecycle::on_shutdown`.
Per task 04, the actual flush-on-drop is owned by `TelemetryGuard::Drop`
itself — the explicit `Arc::into_inner` + `drop()` step is purely a
synchronisation aid to make the flush land *inside* `on_shutdown`
rather than whenever the last clone goes away:

```rust
// inside on_shutdown, after awaiting other shutdown work
#[cfg(feature = "telemetry")]
if let Some(guard) = state.telemetry_guard.clone() {
    if let Some(inner) = Arc::into_inner(guard) {
        // inner: TelemetryGuard — Drop runs here, synchronously.
        drop(inner);
    } else {
        tracing::warn!(
            "OTEL TelemetryGuard still has outstanding clones at shutdown; \
             the final span batch will flush whenever the last task drops"
        );
    }
}
```

There is no separate "graceful shutdown / flush" task: the flush is a
side effect of `TelemetryGuard::Drop` (task 04). This task's
responsibility is just to make sure the field exists, is populated,
and is dropped at the right moment.

### 6. Show the diff to `AppState`

```diff
--- a/crates/http-server/src/state.rs
+++ b/crates/http-server/src/state.rs
@@ -1,6 +1,9 @@
 use std::sync::Arc;

+#[cfg(feature = "telemetry")]
+use cognee_observability::TelemetryGuard;
+
 use cognee_core::PipelineRunRegistry;
 use cognee_core::pipeline_run_registry::DefaultPipelineRunRegistry;
 use cognee_database::{DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository};
@@ -29,6 +32,15 @@
 #[derive(Clone)]
 pub struct AppState {
     pub config: Arc<HttpServerConfig>,
     pub pipelines: Arc<dyn PipelineRunRegistry>,
     pub lib: Option<Arc<ComponentHandles>>,
     pub auth: Option<Arc<AuthContext>>,
     pub mailer: Arc<dyn Mailer>,
     pub health: Option<Arc<dyn crate::routers::health::HealthChecker>>,
     pub spans: Arc<SpanBuffer>,
     pub sync: Arc<SyncRegistry>,
+
+    /// Flush-on-drop guard for the OpenTelemetry exporter (decision 9).
+    /// Held only for its `Drop` side effect: the last `Arc` released calls
+    /// `provider.force_flush()` + `provider.shutdown()`. `None` when built
+    /// without explicit telemetry init (test paths, library embedders).
+    /// Only present when the `telemetry` feature is enabled (cognee-observability
+    /// is `optional = true` in Cargo.toml — see task 03).
+    #[cfg(feature = "telemetry")]
+    pub telemetry_guard: Option<Arc<TelemetryGuard>>,
 }
@@ -98,6 +112,8 @@
         Ok(Self {
             config: Arc::new(config),
             pipelines,
             lib: None,
             auth: None,
             mailer: Arc::new(crate::auth::LoggingMailer),
             health: None,
             spans: Arc::new(SpanBuffer::new(BufferConfig::from_env())),
             sync: Arc::new(SyncRegistry::new()),
+            #[cfg(feature = "telemetry")]
+            telemetry_guard: None,
         })
     }
@@ -211,6 +227,8 @@
         Ok(Self {
             config: Arc::new(config),
             pipelines,
             lib: None,
             auth: None,
             mailer: Arc::new(crate::auth::LoggingMailer),
             health: None,
             spans: Arc::new(SpanBuffer::new(BufferConfig::from_env())),
             sync: Arc::new(SyncRegistry::new()),
+            #[cfg(feature = "telemetry")]
+            telemetry_guard: None,
         })
     }
 }
```

The cfg-gating means callers that only ever build with the default
feature set (no `telemetry`) never see the field and never need to
mention it in `AppState { ... }` literals. Callers that build with
`--features telemetry` initialise it to `None` in tests; the binary
path in `main.rs` (also gated under `cfg(feature = "telemetry")`)
overwrites it with the real `Some(Arc::new(guard))` after subscriber
init.

## Verification

After landing this task:

1. **Default-off compile** — `cargo check -p cognee-http-server`
   succeeds. `cognee-observability` is *not* in the dep graph;
   `AppState` does not have a `telemetry_guard` field. Confirm with
   `cargo tree -p cognee-http-server | grep opentelemetry` returning
   empty.
2. **Telemetry-on compile** —
   `cargo check -p cognee-http-server --features telemetry`
   succeeds. `cargo tree` now shows `opentelemetry`,
   `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-opentelemetry`.
3. **No-default-features compile** —
   `cargo check -p cognee-http-server --no-default-features` still
   passes (decision 1 contract: telemetry is never silently activated
   by an unrelated feature).
4. **Existing test suite green** —
   `cargo test -p cognee-http-server` (default features) and
   `cargo test -p cognee-http-server --features telemetry` both pass.
   The `telemetry_guard: None` default keeps every existing
   constructor site source-compatible under the telemetry-on build.
5. **`/api/v1/activity/spans` regression check** — start the server
   without OTEL env vars, hit `/`, then
   `GET /api/v1/activity/spans` and confirm the request span still
   appears (proves `SpanBufferLayer` still receives spans after the
   composition change).
6. **`EnvSettingsView` parity** — a unit test in
   `cognee-observability` constructs both `EnvSettingsView::from_env()`
   (with empty env) and the relevant subset of
   `cognee_lib::Settings::default()` (mirrored manually if no
   `cognee-lib` dev-dep) and asserts every field matches. Drift is a
   bug.
7. **Manual OTEL smoke test** —
   ```bash
   docker run --rm -p 4317:4317 otel/opentelemetry-collector:latest
   OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
   COGNEE_TRACING_ENABLED=true \
   cargo run -p cognee-http-server --features telemetry -- --port 8000
   curl http://localhost:8000/
   # send SIGTERM (Ctrl-C) — observe the collector logs receiving spans
   # in the final batch flushed by TelemetryGuard::drop.
   ```
   Cross-check: the same `curl /api/v1/activity/spans` still works
   (both sinks observed the request span — composition order verified).

## Files modified

| File | Crate | Change |
|---|---|---|
| [`crates/observability/src/settings.rs`](../../../crates/observability/src/settings.rs) | `cognee-observability` | Add `EnvSettingsView` struct + `SettingsView` impl (env-backed view used by callers that don't depend on `cognee-lib`). |
| [`crates/observability/src/lib.rs`](../../../crates/observability/src/lib.rs) | `cognee-observability` | Add `pub use settings::EnvSettingsView;` to the public re-exports. |
| [`crates/http-server/src/state.rs`](../../../crates/http-server/src/state.rs) | `cognee-http-server` | Add `#[cfg(feature = "telemetry")]`-gated `telemetry_guard: Option<Arc<TelemetryGuard>>` field, default to `None` in both `AppState::build` and `AppState::build_with_db`. |
| [`crates/http-server/src/main.rs`](../../../crates/http-server/src/main.rs) | `cognee-http-server` | Construct `EnvSettingsView::from_env()` before subscriber init (decision 11), refactor `init_tracing` into telemetry-on (composes OTEL bridge layer at the bottom of the layered subscriber) and telemetry-off variants, return the `TelemetryGuard` to `main`, attach it to `AppState` before `run`. |

(No changes to `crates/http-server/src/lib.rs` for the basic flow;
the explicit `Arc::into_inner` + `drop()` step inside
`lifecycle::on_shutdown` is left to a follow-up if/when background
tasks start to retain `AppState` clones past the server stop. The
flush itself is owned by `TelemetryGuard::Drop` per task 04, so there
is no separate flush-task to schedule.)

## Risks

- **`AppState: Clone` interaction.** `TelemetryGuard` cannot derive
  `Clone` (its `Drop` is not idempotent in the strict sense — calling
  `provider.shutdown()` twice is undefined behaviour per the
  `opentelemetry_sdk` docs). The `Arc<TelemetryGuard>` wrap solves
  this: clones share the inner guard; `Drop` fires exactly once when
  the *last* `Arc` is released. Mitigation: documented in the field
  rustdoc and exercised by the manual OTEL smoke test above.
- **Background tasks delaying guard drop.** If a handler spawns a task
  that retains `Arc<AppState>` (e.g. cloud-sync, long pipeline runs),
  the guard's `Drop` is postponed until that task completes. For a
  graceful SIGTERM-then-exit flow this means the final batch is
  flushed *eventually* but the binary may exit before the flush
  completes — losing spans. **Mitigation:** the explicit
  `Arc::into_inner` + `drop()` step inside `lifecycle::on_shutdown`
  (sketched in step 5) makes the guard's `Drop` fire synchronously
  inside `on_shutdown` and the flush completes before `axum::serve`
  returns. The `tracing::warn!` log when `Arc::into_inner` returns
  `None` makes operator-visible the case where background tasks held
  the guard past shutdown.
- **No `with_graceful_shutdown` in the library code path.**
  [`run` line 181](../../../crates/http-server/src/lib.rs#L181) shows
  the `not(feature = "bin")` branch uses bare `axum::serve(...).await`
  with no shutdown signal. Library embedders who use `cognee_http_server::run`
  without `bin` therefore never see a "graceful stop" — when their
  caller drops the future, `state` is dropped immediately, and the
  guard's `Drop` runs synchronously inside the embedder's runtime.
  This is acceptable but worth documenting.
- **Layer-ordering type constraint.** The boxed `Layer<Registry>`
  returned by `init_telemetry::<Registry>` only typechecks when slotted
  directly above `Registry`. The composition order in this doc
  (`Registry → telemetry_layer → env_filter → fmt_layer → span_buffer_layer`)
  is the only valid arrangement; reordering would surface as a
  compile-time `Layer<Layered<...>>` bound failure. See task 06's
  Implementation notes for the full type-stacking explanation. Callbacks
  on each layer are independent reads of the same span data, so the
  observable behaviour remains order-agnostic for current layers; if a
  future layer mutates shared state, re-examine.
- **`EnvSettingsView` / `Settings::default()` drift.** Because
  `EnvSettingsView::from_env()` mirrors the defaults from
  `crates/lib/src/config.rs::Settings::default()` (lines 644–651) but
  does not import the type, a future change to `Settings::default()`
  could silently desynchronise the two. Mitigation: a unit test in
  `cognee-observability` (added in step 1, exercised by task 09)
  asserts field-by-field equality between `EnvSettingsView::from_env()`
  with empty env and the expected default constants — drift makes the
  test fail loudly.
- **Settings load coupling.** Calling `EnvSettingsView::from_env()`
  before subscriber init means any tracing emitted during that call
  itself goes to a default subscriber (or is dropped). Decision 11
  accepts this trade-off — the alternative (two-stage init: temp
  subscriber → load settings → swap to real subscriber) introduces a
  global-subscriber-replace path that `tracing` does not natively
  support without `with_default` scoping. `EnvSettingsView::from_env()`
  is just a series of `std::env::var` reads, so it emits no spans in
  practice.
- **Test build with `bin` but without `telemetry`.** The `bin` feature
  is required for `main.rs` to compile (it gates `clap` and `dotenv`).
  Tests typically build the library, not the bin, so the new
  `telemetry_guard` field needs to be settable from
  `#[cfg(test)]` `AppState` literals — but only under
  `#[cfg(feature = "telemetry")]`, since the field does not exist
  otherwise. Setting it to `None` is always valid; verify all in-tree
  test fixtures that build `AppState` manually (search for
  `AppState {` literal — currently zero hits; tests use
  `AppState::build` so this risk is mostly theoretical).

## Implementation notes

Recording deviations from the plan as landed in commit 56433e5:

1. **`EnvSettingsView` added to `cognee-observability`.** Per the design
   decision approved during sub-doc revision, step 1 of this doc
   introduced a new env-backed implementation of `SettingsView` inside
   `cognee-observability` so that `cognee-http-server` (which
   intentionally does not depend on `cognee-lib`) can drive
   `init_telemetry` directly from environment variables. Shipped as
   designed.
2. **14 `AppState` test fixtures touched.** The "Risks" section of this
   doc claimed `AppState { ... }` literals had "currently zero hits" —
   that turned out to be stale. The implementation added
   `#[cfg(feature = "telemetry")] telemetry_guard: None,` to 14 test
   fixture sites that construct `AppState` literally. No production-code
   sites needed similar updates beyond the two `AppState::build*`
   constructors already enumerated in step 6.
3. **Graceful-shutdown `Arc::into_inner` step deliberately deferred.**
   `AppState` moves into `run()` and drops when `axum::serve` returns,
   so the guard's `Drop` runs there. No background tasks currently
   retain `AppState` past server stop, so the explicit
   `Arc::into_inner` + `drop()` synchronisation aid sketched in step 5
   was not added in this commit. Acceptable per the "Files modified"
   note in this sub-doc; revisit when a background task starts
   retaining `AppState` clones beyond server shutdown.

## References

- Parent doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md)
  (especially [Subscriber composition](../01-otel-otlp-export.md#subscriber-composition)
  and [Design decisions 1, 9, 10, 11](../01-otel-otlp-export.md#design-decisions-locked))
- Sibling sub-docs:
  - [`02-observability-crate-scaffold.md`](./02-observability-crate-scaffold.md)
  - [`03-cognee-lib-feature-wiring.md`](./03-cognee-lib-feature-wiring.md)
  - [`04-init-telemetry-implementation.md`](./04-init-telemetry-implementation.md)
  - [`05-cognee-lib-reexports.md`](./05-cognee-lib-reexports.md)
  - [`06-cli-subscriber-refactor.md`](./06-cli-subscriber-refactor.md)
  - [`09-observability-unit-tests.md`](./09-observability-unit-tests.md)
  - [`10-otel-export-integration-test.md`](./10-otel-export-integration-test.md)
- Source files referenced:
  - [`crates/observability/src/settings.rs`](../../../crates/observability/src/settings.rs)
  - [`crates/observability/src/lib.rs`](../../../crates/observability/src/lib.rs)
  - [`crates/http-server/src/main.rs`](../../../crates/http-server/src/main.rs)
  - [`crates/http-server/src/lib.rs`](../../../crates/http-server/src/lib.rs)
  - [`crates/http-server/src/state.rs`](../../../crates/http-server/src/state.rs)
  - [`crates/http-server/src/observability/mod.rs`](../../../crates/http-server/src/observability/mod.rs)
  - [`crates/http-server/src/observability/span_buffer_layer.rs`](../../../crates/http-server/src/observability/span_buffer_layer.rs)
  - [`crates/http-server/Cargo.toml`](../../../crates/http-server/Cargo.toml)
  - [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs) (default values for the OTEL settings, mirrored in `EnvSettingsView`)
- External:
  - [`tracing_subscriber::Layer` trait docs](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/trait.Layer.html)
  - [`tracing_subscriber::layer::SubscriberExt::with`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/trait.SubscriberExt.html#method.with)
  - [`tracing_subscriber::layer::Identity`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/struct.Identity.html)
  - [`tracing-opentelemetry::layer`](https://docs.rs/tracing-opentelemetry/latest/tracing_opentelemetry/fn.layer.html)
  - [`axum::serve::with_graceful_shutdown`](https://docs.rs/axum/latest/axum/serve/struct.Serve.html#method.with_graceful_shutdown)
