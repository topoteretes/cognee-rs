# Action item 7 — Refactor `cognee-http-server` subscriber composition and store `TelemetryGuard` on `AppState`

- **Status:** Planned (not yet implemented)
- **Owner / Dependencies:**
  - **Depends on:**
    - Task [`02` — bootstrap the `cognee-observability` crate](./02-cognee-observability-crate.md) (provides `TelemetryGuard`, `init_telemetry`, the noop fallback layer)
    - Task [`03` — `telemetry` feature wiring across crates](./03-cognee-lib-feature-wiring.md) (adds the `telemetry` feature to `crates/http-server/Cargo.toml` plus the optional `cognee-observability` dependency)
    - Task [`04` — OTLP exporter + `SdkTracerProvider` builder inside `cognee-observability`](./04-otlp-exporter-builder.md) *(once written)* (the actual OTEL provider this task installs)
    - Task [`05` — `cognee-lib` `init_telemetry` re-exports & subscriber helper](./05-cognee-lib-public-api.md) *(once written)* (defines the public API surface the http-server calls into; if the helper lives in `cognee-observability` directly, this task imports from there instead — see task 03 note about avoiding the `cognee-lib` cycle)
  - **Sibling references:**
    - Task [`06` — CLI subscriber refactor](./06-cli-subscriber-refactor.md) — same composition pattern, but the CLI keeps the guard in a local `main()` binding rather than on a long-lived state struct.
    - Task [`09` — graceful shutdown / flush hook](./09-graceful-shutdown-flush.md) *(once written)* — depends on this task because it reaches into `AppState` to extract and drop the guard explicitly.
    - Task [`10` — integration test against a fake OTLP collector](./10-integration-test-fake-collector.md) *(once written)* — exercises the composition this task installs.
- **Anchor in parent:** action item 7 of [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md#action-items) ("Refactor the HTTP server subscriber"), bound by [Design decisions 1, 9, 10, 11](../01-otel-otlp-export.md#design-decisions-locked).

## Rationale

Today, the http-server subscriber in
[`crates/http-server/src/main.rs:100`](../../../crates/http-server/src/main.rs#L100)
composes only `EnvFilter → fmt → SpanBufferLayer`. After tasks 02–05 land
we have a `cognee_observability::init_telemetry(&Settings)` helper that
returns `(OtelLayer, TelemetryGuard)`, where the layer is either a real
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
   `Option<TelemetryGuard>` for the noop+real symmetry described
   below.

### Subscriber composition order

Per the parent doc's [Subscriber composition](../01-otel-otlp-export.md#subscriber-composition)
section the order is

```
EnvFilter → fmt::layer (stdout) → tracing-opentelemetry::layer → SpanBufferLayer
```

`tracing_subscriber::Layer` runs registered layers in the order they
were added: `Registry::default().with(A).with(B).with(C)` invokes
`A`'s callbacks first, then `B`'s, then `C`'s, for each event. So
adding the OTEL bridge before `SpanBufferLayer` means the OTEL layer
observes spans first and `SpanBufferLayer` runs last, which is what
we want: the OTEL layer reads span metadata and translates it into
OTEL semantic conventions; `SpanBufferLayer` then independently reads
the same metadata into its in-memory ring. Neither layer mutates
shared span state, so the order is functionally equivalent for both
sinks today, but keeping the OTEL layer first matches the Python
ordering (Python attaches `SimpleSpanProcessor(otlp_exporter)` to
the same provider as `SimpleSpanProcessor(in_memory_exporter)`, so
both observe spans on commit; we mirror that with the OTEL bridge
running first).

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

- Task [`02`](./02-cognee-observability-crate.md) merged: the
  `cognee-observability` crate exists with `init_telemetry(&Settings)
  -> Result<(OtelLayer, TelemetryGuard), TelemetryInitError>` and a
  `TelemetryGuard` whose `Drop` calls
  `provider.force_flush()` followed by `provider.shutdown()`. Under
  `not(feature = "telemetry")`, `OtelLayer = tracing_subscriber::layer::Identity`
  and `TelemetryGuard` is a zero-sized noop.
- Task [`03`](./03-cognee-lib-feature-wiring.md) merged: the
  `crates/http-server/Cargo.toml` manifest now contains
  ```toml
  cognee-observability = { path = "../observability", optional = true }
  ```
  and a forwarding feature
  ```toml
  telemetry = ["dep:cognee-observability", "cognee-observability/telemetry", "cognee-core/telemetry"]
  ```
- Task [`04`](./04-otlp-exporter-builder.md): `init_telemetry` actually
  builds an `SdkTracerProvider` with an OTLP exporter when the
  `telemetry` feature is on and the relevant env vars / settings are
  present.
- Task [`05`](./05-cognee-lib-public-api.md) optional: the
  `cognee_lib::observability` re-export exists. If task 05 chose to
  expose `TelemetryGuard` only via `cognee_lib`, this task would import
  from there; per task 03's manifest note ("the HTTP server
  intentionally does not depend on `cognee-lib`"), the http-server
  imports the symbol directly from `cognee_observability` instead.

## Step-by-step

### 1. Add `telemetry_guard` field to `AppState`

Edit
[`crates/http-server/src/state.rs`](../../../crates/http-server/src/state.rs).

The current struct (line 28) is

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

Add the field as `Option<Arc<TelemetryGuard>>` (Option so the noop /
testing constructions can pass `None`):

```rust
// new import
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
    /// `None` when built from `AppState::build` without an explicit guard
    /// (test paths, library embedders that manage telemetry themselves) and
    /// also when the `telemetry` feature is off — the field still exists
    /// under both feature configurations to keep `AppState`'s shape stable
    /// (decision 1).
    #[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
    pub telemetry_guard: Option<Arc<TelemetryGuard>>,
}
```

For the off-feature build, declare a stub `TelemetryGuard` type alias
so the field type still resolves. Either:

- Re-export the noop `TelemetryGuard` from `cognee-observability`
  unconditionally (decision: do this — task 02 already covers it; the
  optional dep is the *gateway*, not the type itself, for the noop
  variant. Concretely: `cognee-observability` defines `TelemetryGuard`
  in its always-compiled API surface; the `telemetry` feature only
  controls whether `Drop` actually shuts down a real provider). In
  that case the optional-dep gating in task 03 is wrong — revisit
  there. **Recommended option.**
- Or, define a local zero-sized `TelemetryGuard` struct in `state.rs`
  under `#[cfg(not(feature = "telemetry"))]` and use the
  `cognee_observability` import only under `#[cfg(feature = "telemetry")]`.
  This keeps task 03 unchanged but adds a small surface duplication.

This sub-doc assumes the **first** approach: `cognee-observability` is
a non-optional zero-cost dep that exposes `TelemetryGuard` always, and
the `telemetry` feature inside that crate gates the *real* OTEL stack.
If task 03 keeps `cognee-observability` optional, fall back to the
second approach and gate the field with `#[cfg(feature = "telemetry")]`.

Update both `AppState::build` (line 88) and `AppState::build_with_db`
(line 188) to default `telemetry_guard: None`.

### 2. Refactor `init_tracing` to compose the OTEL layer

Replace the body of `init_tracing` in
[`crates/http-server/src/main.rs:100`](../../../crates/http-server/src/main.rs#L100).
The new function takes `&Settings` (loaded from `cognee_lib::Settings`
or via the http-server's `HttpServerConfig` if it already wraps the
relevant fields) and returns `Result<Option<Arc<TelemetryGuard>>,
anyhow::Error>` so `main()` can attach the result to `AppState`:

```rust
/// Build the layered subscriber:
///
/// ```
/// EnvFilter
///   → fmt::layer (stdout)
///   → tracing-opentelemetry::layer  (real OTEL bridge or Identity)
///   → SpanBufferLayer               (in-memory ring for /api/v1/activity/spans)
/// ```
///
/// Returns the `TelemetryGuard` produced by `cognee_observability::init_telemetry`
/// so the caller can install it on `AppState` (decision 9). When the OTEL
/// stack is disabled (feature off, or env vars unset — decision 2), the OTEL
/// layer collapses to `Identity` and the guard is a noop.
fn init_tracing(
    settings: &cognee_lib::Settings,
    spans: Arc<SpanBuffer>,
) -> anyhow::Result<Option<Arc<cognee_observability::TelemetryGuard>>> {
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let buffer_layer = SpanBufferLayer::new((*spans).clone());

    let (otel_layer, guard) = cognee_observability::init_telemetry(settings)
        .map_err(|e| anyhow::anyhow!("failed to initialise OTEL bridge: {e}"))?;

    // Composition order matches docs/telemetry/01-otel-otlp-export.md
    // ("Subscriber composition"): EnvFilter → fmt → otel → SpanBufferLayer.
    // tracing-subscriber runs layers in registration order, so the OTEL
    // bridge sees spans before SpanBufferLayer does. Both layers read span
    // metadata; neither mutates shared state, so this ordering is correct
    // and matches the CLI subscriber (task 06).
    Registry::default()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .with(buffer_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to install global tracing subscriber: {e}"))?;

    Ok(Some(Arc::new(guard)))
}
```

Two notes:

- The current `init_tracing` swallows the `try_init` error (`let _ =
  ...`) because tests may install a subscriber first. The refactor
  promotes the error: at binary startup we *expect* nobody else has
  installed one. If a future test embeds `cognee-http-server` in-process
  with its own subscriber, that test should call `build_router` and
  `run` instead of `main`.
- `init_telemetry`'s `Result<(_, TelemetryGuard), _>` is unwrapped via
  `?`; the noop variant returns `Ok((Identity, NoopGuard))` so the
  `?` is a guaranteed-no-error path when telemetry is off (decision 1).

### 3. Wire the guard into `main()`

Modify `main` in
[`crates/http-server/src/main.rs:48`](../../../crates/http-server/src/main.rs#L48)
to:

1. Load settings *before* installing the subscriber (decision 11).
   `cognee_lib::Settings::load()` (or the appropriate
   `cognee_observability::Settings` accessor) reads
   `COGNEE_TRACING_ENABLED`, `OTEL_SERVICE_NAME`,
   `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`.
2. Call `init_tracing(&settings, spans.clone())` and bind the returned
   guard.
3. After `AppState::build`, set `state.telemetry_guard = guard`.
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

    // 1. Load settings BEFORE installing the subscriber (decision 11).
    //    Subscribed OTEL must see correct config on the first span emitted.
    let settings = cognee_lib::Settings::load()
        .context("failed to load cognee settings")?;

    // 2. Build the in-memory span buffer.
    let spans = Arc::new(SpanBuffer::new(BufferConfig::from_env()));

    // 3. Compose subscriber: EnvFilter → fmt → OTEL bridge → SpanBufferLayer.
    //    Returns the flush-on-drop guard produced by cognee-observability.
    let telemetry_guard = init_tracing(&settings, spans.clone())?;

    // 4. Parse CLI args.
    let args = Args::parse();

    // 5. Build HTTP config with CLI-flag overrides.
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

    // 6. Build application state and attach the guard (decision 9).
    let mut state = AppState::build(cfg.clone())
        .await
        .context("failed to build AppState")?;
    state.spans = spans;
    state.telemetry_guard = telemetry_guard;

    // 7. Bind and serve. axum's graceful shutdown (already wired in
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

fn init_tracing(
    settings: &cognee_lib::Settings,
    spans: Arc<SpanBuffer>,
) -> anyhow::Result<Option<Arc<cognee_observability::TelemetryGuard>>> {
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let buffer_layer = SpanBufferLayer::new((*spans).clone());

    let (otel_layer, guard) = cognee_observability::init_telemetry(settings)
        .map_err(|e| anyhow::anyhow!("failed to initialise OTEL bridge: {e}"))?;

    Registry::default()
        .with(filter)
        .with(fmt_layer)
        .with(otel_layer)
        .with(buffer_layer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to install global tracing subscriber: {e}"))?;

    Ok(Some(Arc::new(guard)))
}
```

### 4. Confirm graceful shutdown drops the guard

[`crates/http-server/src/lib.rs:128`](../../../crates/http-server/src/lib.rs#L128)
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
**explicitly extract and drop the guard** during `lifecycle::on_shutdown`:

```rust
// inside on_shutdown, after awaiting other shutdown work
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

Wiring this explicit extraction is task 09's responsibility; this task
just makes sure the field exists and is populated.

### 5. Show the diff to `AppState`

```diff
--- a/crates/http-server/src/state.rs
+++ b/crates/http-server/src/state.rs
@@ -1,6 +1,8 @@
 use std::sync::Arc;

+use cognee_observability::TelemetryGuard;
+
 use cognee_core::PipelineRunRegistry;
 use cognee_core::pipeline_run_registry::DefaultPipelineRunRegistry;
 use cognee_database::{DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository};
@@ -28,6 +30,18 @@
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
+    /// Under `not(feature = "telemetry")`, `TelemetryGuard` is a zero-sized
+    /// noop type re-exported by `cognee-observability`.
+    #[cfg_attr(not(feature = "telemetry"), allow(dead_code))]
+    pub telemetry_guard: Option<Arc<TelemetryGuard>>,
 }
@@ -98,6 +112,7 @@
         Ok(Self {
             config: Arc::new(config),
             pipelines,
             lib: None,
             auth: None,
             mailer: Arc::new(crate::auth::LoggingMailer),
             health: None,
             spans: Arc::new(SpanBuffer::new(BufferConfig::from_env())),
             sync: Arc::new(SyncRegistry::new()),
+            telemetry_guard: None,
         })
     }
@@ -212,6 +227,7 @@
         Ok(Self {
             config: Arc::new(config),
             pipelines,
             lib: None,
             auth: None,
             mailer: Arc::new(crate::auth::LoggingMailer),
             health: None,
             spans: Arc::new(SpanBuffer::new(BufferConfig::from_env())),
             sync: Arc::new(SyncRegistry::new()),
+            telemetry_guard: None,
         })
     }
 }
```

If task 02 chose to keep `cognee-observability` strictly optional, wrap
the import and the field in `#[cfg(feature = "telemetry")]` instead and
mirror the wrap on every `AppState { ... }` literal in tests. Prefer the
unconditional approach (described above) — it keeps `AppState`'s shape
stable across feature flags and avoids `cfg`-gated test scaffolding.

## Verification

After landing this task:

1. **Default-off compile** — `cargo check -p cognee-http-server`
   succeeds. `init_telemetry` returns `(Identity, NoopGuard)`; no OTEL
   crates are linked. Confirm with
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
   `cargo test -p cognee-http-server` (uses real and mock state). The
   `telemetry_guard: None` default keeps every existing constructor
   site source-compatible.
5. **`/api/v1/activity/spans` regression check** — start the server
   without OTEL env vars, hit `/`, then
   `GET /api/v1/activity/spans` and confirm the request span still
   appears (proves `SpanBufferLayer` still receives spans after the
   composition change).
6. **Manual OTEL smoke test** —
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

- [`crates/http-server/src/state.rs`](../../../crates/http-server/src/state.rs)
  — add `telemetry_guard: Option<Arc<TelemetryGuard>>` field, default
  to `None` in both constructors.
- [`crates/http-server/src/main.rs`](../../../crates/http-server/src/main.rs)
  — load settings before subscriber init (decision 11), refactor
  `init_tracing` to compose the OTEL bridge layer between `fmt` and
  `SpanBufferLayer`, return the `TelemetryGuard` to `main`, attach it
  to `AppState` before `run`.
- (No changes to `crates/http-server/src/lib.rs` for the basic flow;
  task 09 will add explicit guard extraction in `lifecycle::on_shutdown`.)

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
  completes — losing spans. **Mitigation:** task 09 adds an explicit
  `Arc::into_inner` + `drop()` step inside `lifecycle::on_shutdown`,
  after awaiting other shutdown work, so the guard's `Drop` fires
  synchronously inside `on_shutdown` and the flush completes before
  `axum::serve` returns. This is the design path; document it in
  task 09 with a `tracing::warn!` log when `Arc::into_inner` returns
  `None` so operators see when background tasks held the guard past
  shutdown.
- **No `with_graceful_shutdown` in the library code path.**
  [`run` line 181](../../../crates/http-server/src/lib.rs#L181) shows
  the `not(feature = "bin")` branch uses bare `axum::serve(...).await`
  with no shutdown signal. Library embedders who use `cognee_http_server::run`
  without `bin` therefore never see a "graceful stop" — when their
  caller drops the future, `state` is dropped immediately, and the
  guard's `Drop` runs synchronously inside the embedder's runtime.
  This is acceptable but worth documenting; flag in task 09 whether to
  extend the library-only branch with a `tokio::sync::CancellationToken`
  parameter so embedders can opt in to the same flush behaviour.
- **Composition-order assumption.** This task assumes
  `Registry::default().with(A).with(B).with(C)` runs A's callbacks
  before B's before C's. That is the documented behaviour of
  `tracing_subscriber::layer::SubscriberExt`: `Layered<L, S>::on_event`
  invokes `self.inner.on_event` (the prior subscriber) and then
  `self.layer.on_event` (the newly added layer). So *literally* layers
  run **last-added-first** at the function-call level — but each
  layer's callbacks are independent reads of the same span data, so
  the observable behaviour is order-agnostic. The order in this doc
  matches what the parent doc 01 specifies; both are valid because
  no layer mutates shared state. If a future layer *does* mutate
  shared state (e.g. attribute redaction before OTEL export), the
  order documented here may need re-examination. Add a unit test in
  task 04 that asserts both layers receive every span in a fixture
  pipeline.
- **Settings load coupling.** Calling `cognee_lib::Settings::load()`
  before subscriber init means any tracing emitted by `Settings::load`
  itself goes to a default subscriber (or is dropped). Decision 11
  accepts this trade-off — the alternative (two-stage init: temp
  subscriber → load settings → swap to real subscriber) introduces a
  global-subscriber-replace path that `tracing` does not natively
  support without `with_default` scoping.
- **Test build with `bin` but without `telemetry`.** The `bin` feature
  is required for `main.rs` to compile (it gates `clap` and `dotenv`).
  Tests typically build the library, not the bin, so the new
  `telemetry_guard` field needs to be settable from
  `#[cfg(test)]` `AppState` literals. Setting it to `None` is always
  valid; verify all in-tree test fixtures that build `AppState`
  manually (search for `AppState {` literal — currently zero hits;
  tests use `AppState::build` so this risk is mostly theoretical).

## References

- Parent doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md)
  (especially [Subscriber composition](../01-otel-otlp-export.md#subscriber-composition)
  and [Design decisions 1, 9, 10, 11](../01-otel-otlp-export.md#design-decisions-locked))
- Sibling sub-docs:
  - [`02-cognee-observability-crate.md`](./02-cognee-observability-crate.md)
  - [`03-cognee-lib-feature-wiring.md`](./03-cognee-lib-feature-wiring.md)
  - [`06-cli-subscriber-refactor.md`](./06-cli-subscriber-refactor.md)
- Source files referenced:
  - [`crates/http-server/src/main.rs`](../../../crates/http-server/src/main.rs)
  - [`crates/http-server/src/lib.rs`](../../../crates/http-server/src/lib.rs)
  - [`crates/http-server/src/state.rs`](../../../crates/http-server/src/state.rs)
  - [`crates/http-server/src/observability/mod.rs`](../../../crates/http-server/src/observability/mod.rs)
  - [`crates/http-server/src/observability/span_buffer_layer.rs`](../../../crates/http-server/src/observability/span_buffer_layer.rs)
  - [`crates/http-server/Cargo.toml`](../../../crates/http-server/Cargo.toml)
- External:
  - [`tracing_subscriber::Layer` trait docs](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/trait.Layer.html)
  - [`tracing_subscriber::layer::SubscriberExt::with`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/trait.SubscriberExt.html#method.with)
  - [`tracing_subscriber::layer::Identity`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/struct.Identity.html)
  - [`tracing-opentelemetry::layer`](https://docs.rs/tracing-opentelemetry/latest/tracing_opentelemetry/fn.layer.html)
  - [`axum::serve::with_graceful_shutdown`](https://docs.rs/axum/latest/axum/serve/struct.Serve.html#method.with_graceful_shutdown)
