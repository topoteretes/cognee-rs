# OpenTelemetry SDK + OTLP Export Wiring

**Status: this gap is closed.** All 12 sub-tasks shipped between commits
`8cc50bb` and `0fc9adb` (plus one rename fixup `27c2bb2`). The Rust port
now wires the OpenTelemetry SDK end-to-end: `init_telemetry` builds a
`SdkTracerProvider` with OTLP exporter, the `tracing-opentelemetry`
bridge feeds all 51+ `#[instrument]` sites into the OTEL pipeline, the
CLI and HTTP server compose the bridge into their subscriber stacks,
and CI exercises both the `--features telemetry` and noop-fallback
arms. See [Closure summary](#closure-summary) at the bottom of this
document for the full commit list.

## Overview

The Rust port emits structured spans through the `tracing` ecosystem (51+
`#[tracing::instrument]` sites across `storage`, `ingestion`, `search`,
`delete`, `cognify`, `http-server`, etc.) and stores them locally via
[`SpanBufferLayer`](../../crates/http-server/src/observability/span_buffer_layer.rs)
for the `/api/v1/activity/spans` endpoint. However, none of those spans
ever leave the process: although `Settings` parses the four `OTEL_*`
environment variables (`COGNEE_TRACING_ENABLED`, `OTEL_SERVICE_NAME`,
`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`), the values
are never read anywhere downstream — there is no `TracerProvider`, no
OTLP exporter, no resource attributes, no shutdown handling. This
document describes the gap and a concrete plan for wiring the
OpenTelemetry SDK so the existing `tracing` instrumentation flows to a
collector (Dash0, Grafana Tempo, Honeycomb, an in-cluster
`otel-collector`, etc.), matching the behaviour of the Python SDK's
`cognee.modules.observability.tracing.setup_tracing()`.

## Python implementation

### `setup_tracing()` and OTLP export

The full OTEL bring-up lives in
[`cognee/modules/observability/tracing.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py).
Key pieces (line numbers refer to `/tmp/cognee-python/cognee/modules/observability/tracing.py`):

- `setup_tracing(console_output=False)` (lines 290–345) is the entry
  point. It:
  1. Creates a process-wide `CogneeSpanExporter` (in-memory ring buffer
     of the last 50 traces, lines 100–166) — analogous to our
     `SpanBufferLayer`.
  2. Detects external auto-instrumentation via `_is_auto_instrumented()`
     (lines 241–249): if `trace.get_tracer_provider()` returns anything
     other than the default `ProxyTracerProvider`, an external tool
     (e.g. `opentelemetry-instrument`, Datadog, Dash0 agent) has
     already configured a provider, so cognee just attaches its
     in-memory exporter to it.
  3. Otherwise, builds its own `Resource` (lines 325–331) with
     `service.name` (from `BaseConfig.otel_service_name`),
     `service.version` (from `cognee.version.get_cognee_version()`),
     and `deployment.environment` (from the `ENV` env var, default
     `development`).
  4. Constructs `TracerProvider(resource=resource)`, attaches the
     in-memory exporter via `SimpleSpanProcessor`, then calls
     `_try_add_otlp_exporter()` (lines 252–287).
  5. Optionally adds a `ConsoleSpanExporter` when `console_output=True`.
  6. Calls `trace.set_tracer_provider(_provider)` to install globally.

- `_try_add_otlp_exporter(provider)` (lines 252–287) reads
  `BaseConfig.otel_exporter_otlp_endpoint`. If set, it tries gRPC first
  (`opentelemetry.exporter.otlp.proto.grpc.trace_exporter`), falling
  back to HTTP (`opentelemetry.exporter.otlp.proto.http.trace_exporter`).
  It wraps the exporter in a `SimpleSpanProcessor` and adds it to the
  provider. Standard `OTEL_EXPORTER_OTLP_*` env vars (headers,
  compression, timeout) are honoured by the exporters themselves.

- `shutdown_tracing()` (lines 363–372) calls `_provider.force_flush()`
  followed by `_provider.shutdown()`.

### Public API

[`cognee/modules/observability/trace_context.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/trace_context.py)
wraps the SDK calls:

- `enable_tracing(console_output=False)` (lines 16–24) — calls
  `setup_tracing` and flips `_tracing_enabled = True`.
- `is_tracing_enabled()` (lines 34–62) — checks the module flag, then
  `BaseConfig.cognee_tracing_enabled`, then the `COGNEE_TRACING_ENABLED`
  env var directly. **Lazy-initialises** OTEL when enabled but not yet
  set up — this is the path most users hit (no explicit
  `enable_tracing()` call required).
- `disable_tracing()`, `get_last_trace()`, `get_all_traces()`,
  `clear_traces()` round out the public surface.

### Config fields

In `/tmp/cognee-python/cognee/base_config.py:49–57`:

```python
cognee_tracing_enabled: bool = os.getenv("COGNEE_TRACING_ENABLED", "false").lower() in ("true", "1", "yes")
otel_service_name: str = os.getenv("OTEL_SERVICE_NAME", "cognee")
otel_exporter_otlp_endpoint: Optional[str] = os.getenv("OTEL_EXPORTER_OTLP_ENDPOINT")
otel_exporter_otlp_headers: Optional[str] = os.getenv("OTEL_EXPORTER_OTLP_HEADERS")
```

### Optional dependency extras

`/tmp/cognee-python/pyproject.toml:180–194` exposes two extras that
both pull in the same OTEL stack:

```toml
tracing = [
    "opentelemetry-api>=1.20.0,<2",
    "opentelemetry-sdk>=1.20.0,<2",
    "opentelemetry-exporter-otlp-proto-grpc>=1.20.0,<2",
    "opentelemetry-exporter-otlp-proto-http>=1.20.0,<2",
]

monitoring = [
    "sentry-sdk[fastapi]>=2.9.0,<3",
    "langfuse>=2.32.0,<3",
    "opentelemetry-api>=1.20.0,<2",
    ...
]
```

OTEL is therefore strictly opt-in on the Python side — `pip install
cognee` does not pull the OTEL stack.

## Rust current state

### Config fields (parsed but unused)

[`crates/lib/src/config.rs:135–139`](../../crates/lib/src/config.rs#L135) declares
the four observability fields:

```rust
// -- Observability -----------------------------------------------------------
pub cognee_tracing_enabled: bool,
pub otel_service_name: String,
pub otel_exporter_otlp_endpoint: String,
pub otel_exporter_otlp_headers: String,
```

[`config.rs:462–475`](../../crates/lib/src/config.rs#L462) overlays them
from environment variables. **Nothing reads these fields after the
overlay**: a workspace-wide grep for `cognee_tracing_enabled`,
`otel_service_name`, and `otel_exporter_otlp_endpoint` returns only the
`config.rs` declarations and overlay code.

### Subscriber initialisation sites

There are two existing subscriber init paths and both ignore OTEL.

[`crates/cli/src/main.rs:50–58`](../../crates/cli/src/main.rs#L50):

```rust
fn main() {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .try_init();
    ...
}
```

[`crates/http-server/src/main.rs:100–118`](../../crates/http-server/src/main.rs#L100):

```rust
fn init_tracing(spans: Arc<SpanBuffer>) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let buffer_layer = SpanBufferLayer::new((*spans).clone());

    let _ = Registry::default()
        .with(filter)
        .with(fmt_layer)
        .with(buffer_layer)
        .try_init();
}
```

Neither installs a `tracing-opentelemetry` bridge, so even if a user
manually configured a global `TracerProvider` via `opentelemetry::global`,
no `tracing` spans would reach it.

### `telemetry` cargo feature

[`crates/lib/Cargo.toml:37–41`](../../crates/lib/Cargo.toml#L37):

```toml
# External telemetry event export (opt-in). When enabled, the high-level API
# functions emit `tracing` events on the `cognee.telemetry` target so
# downstream subscribers (OTEL log exporter, tracing_subscriber::Layer, etc.)
# can capture them. Mirrors Python's `send_telemetry()` calls.
telemetry = []
```

Currently the feature only gates *event emission* in the high-level
API (e.g.
[`crates/lib/src/api/forget.rs:103–115`](../../crates/lib/src/api/forget.rs#L103)).
There is also a parallel `telemetry = []` feature on
[`crates/core/Cargo.toml:7`](../../crates/core/Cargo.toml#L7) that is
analogous. Neither feature pulls in any OTEL dependency.

### Existing `SpanBufferLayer`

[`crates/http-server/src/observability/span_buffer_layer.rs`](../../crates/http-server/src/observability/span_buffer_layer.rs)
is a hand-rolled `tracing_subscriber::Layer` that captures every span
into an in-memory ring buffer. It is independent of OTEL — it uses its
own randomly-generated 32-char hex `trace_id` per root span and
propagates it to children via `extensions_mut()`. This layer is
roughly equivalent to the Python `CogneeSpanExporter` (in-memory buffer
behind `get_last_trace_spans()`) and **must continue to work** after
OTEL wiring is added.

### Instrumentation surface

`grep -rn '#\[instrument\|tracing::instrument' crates --include='*.rs' | wc -l`
reports 62 instrument sites today (sample: storage I/O,
ingestion pipeline stages, search retrievers, delete cascades, HTTP
route handlers). Every one of these will become an OTEL span the
moment a `tracing-opentelemetry::layer()` is attached — no per-site
changes required.

## Detailed gap analysis

Concrete missing pieces, in implementation order:

1. **No OTEL crate dependencies.** The workspace does not pull in
   `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, or
   `tracing-opentelemetry`.
2. **No `TracerProvider` setup.** The Rust analogue of Python's
   `TracerProvider(resource=resource)` does not exist anywhere.
3. **No OTLP `SpanExporter`.** Neither the gRPC exporter
   (`opentelemetry-otlp` + `tonic`) nor the HTTP/protobuf exporter is
   built.
4. **No `Resource` construction.** `service.name`, `service.version`
   (from `CARGO_PKG_VERSION`), and `deployment.environment` are not
   populated.
5. **No `tracing-opentelemetry::layer()` bridge.** Existing `tracing`
   spans cannot reach an OTEL pipeline because the bridge layer is not
   composed into the subscriber. (Even if a global
   `TracerProvider` is set, no spans flow without this layer.)
6. **No batch processor / shutdown.** Without a
   `BatchSpanProcessor` (or its successor in `opentelemetry-sdk`
   ≥ 0.30, where the API moved to `SdkTracerProvider::builder()
   .with_batch_exporter(...)`), spans would either drop on process
   exit (no flush) or block the hot path (`SimpleSpanProcessor`).
7. **No flush-on-drop guard.** The CLI runs to completion and exits;
   without an explicit `provider.shutdown()` (or RAII guard) the last
   batch is lost.
8. **No auto-instrumentation detection.** Mirror of
   `_is_auto_instrumented()` is missing — there is no equivalent of
   `opentelemetry::global::tracer_provider()` type-check, and no
   safeguard against double-installing a provider.
9. **No header parsing.** Python relies on the OTLP exporter's
   automatic `OTEL_EXPORTER_OTLP_HEADERS` parsing. In Rust, the
   `opentelemetry-otlp` crate also reads that env var, but the
   `Settings.otel_exporter_otlp_headers` field would still need to be
   forwarded to the exporter builder when callers configure it
   programmatically rather than via env.
10. **No public API.** Python exposes `enable_tracing()` /
    `disable_tracing()` / `is_tracing_enabled()` /
    `get_last_trace()` / `get_all_traces()` / `clear_traces()`. The
    Rust `prelude` exports none of these. The HTTP server exposes
    `/api/v1/activity/spans` (consuming `SpanBuffer`), but there is no
    equivalent SDK-level entry point on `cognee_lib`.
11. **No interaction with `SpanBufferLayer`.** When OTEL is enabled, we
    must compose the OTEL layer *and* `SpanBufferLayer` on the same
    registry so the existing `/api/v1/activity/spans` endpoint
    continues to work. Order matters: the OTEL layer should run before
    the buffer layer so they observe identical span lifecycles.
12. **No tests against an OTLP collector.** No fixture spawns a fake
    OTLP receiver to assert that spans actually leave the process.

## Proposed design

### Crate selection (versions current as of 2026-05)

Add (gated behind `telemetry`):

| Crate | Version | Purpose |
|---|---|---|
| `opentelemetry` | `0.31` | API: `KeyValue`, `global`, `trace::TracerProvider` |
| `opentelemetry_sdk` | `0.31` | `SdkTracerProvider`, `Resource`, batch processor |
| `opentelemetry-otlp` | `0.31` | gRPC + HTTP OTLP exporter |
| `opentelemetry-semantic-conventions` | `0.31` | `SERVICE_NAME`, `SERVICE_VERSION`, `DEPLOYMENT_ENVIRONMENT` constants |
| `tracing-opentelemetry` | `0.32` | `tracing::Span` → OTEL span bridge |

Compatibility note: `tracing-opentelemetry` is one minor version ahead
of the core OTEL crates (per the upstream README); 0.32 pairs with
`opentelemetry` 0.31. Pin both to `=0.31`/`=0.32` to keep CI
deterministic.

The OTEL Rust SDK API has **changed** between recent minor versions
(e.g. `TracerProvider` was renamed `SdkTracerProvider`; the global
shutdown API moved). Lock to a known-good pair and bump both together.

### Module placement

Create
`crates/lib/src/observability/mod.rs` and
`crates/lib/src/observability/otel.rs` (new):

```rust
// crates/lib/src/observability/otel.rs
#[cfg(feature = "telemetry")]
pub use real::{init_telemetry, OtelGuard, TelemetryInitError};

#[cfg(not(feature = "telemetry"))]
pub fn init_telemetry(_settings: &Settings) -> Result<OtelGuard, TelemetryInitError> {
    Ok(OtelGuard::noop())
}
```

`OtelGuard` is the RAII flush handle returned by `init_telemetry`. Dropping
it calls `provider.shutdown()`. This mirrors the
`tracing_appender::non_blocking::WorkerGuard` pattern.

The module is in `cognee-lib` (not `cognee-cli` or
`cognee-http-server`) because both binaries plus the library API
(`cognee_lib::api`) need it, and `cognee-lib` is the only crate every
embedder depends on.

### Public API

```rust
// crates/lib/src/observability/mod.rs
pub mod otel;

// re-exports in lib.rs
pub use observability::otel::{init_telemetry, OtelGuard};
```

Add helper functions matching Python:

```rust
pub fn is_tracing_enabled(settings: &Settings) -> bool;
pub fn shutdown_tracing();              // calls into global provider
```

Note: `get_last_trace()` / `get_all_traces()` already exist for the
HTTP server through `SpanBuffer`. The library could expose a thin
wrapper that returns `RecordedSpan`s from the buffer, but that is
separate from this OTLP gap (see Open questions).

### Subscriber composition

Refactor [`crates/cli/src/main.rs`](../../crates/cli/src/main.rs#L50)
and [`crates/http-server/src/main.rs`](../../crates/http-server/src/main.rs#L100)
to use a shared helper from `cognee_lib::observability`:

```rust
pub fn build_subscriber(
    settings: &Settings,
    extra_layers: impl Layer<Registry>,
) -> (impl Subscriber, OtelGuard) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt = tracing_subscriber::fmt::layer().with_target(false);

    let (otel_layer, guard) = otel::init_telemetry(settings)?;

    let subscriber = Registry::default()
        .with(filter)
        .with(fmt)
        .with(otel_layer)        // tracing → OTEL bridge (or noop layer)
        .with(extra_layers);     // SpanBufferLayer (http-server only)

    (subscriber, guard)
}
```

The composition is:

```
EnvFilter → fmt::layer (stdout) → tracing-opentelemetry::layer → SpanBufferLayer
```

`SpanBufferLayer` runs last so it observes the final span data after
OTEL semantic-conventions translation does not mutate it (it doesn't —
the OTEL layer reads, the buffer layer also reads independently).

### Auto-instrumentation detection

Mirror Python's `_is_auto_instrumented()`:

```rust
fn already_instrumented() -> bool {
    // The default global is a NoopTracerProvider. If anything else is set
    // (e.g. by a Datadog/Dash0 init or a test harness), reuse it.
    let provider = opentelemetry::global::tracer_provider();
    // Best-effort: NoopTracerProvider has a stable type name we can sniff
    // via Debug, or we can downcast through Any.
    !format!("{provider:?}").contains("NoopTracerProvider")
}
```

When `true`, skip building our own provider and just construct the
bridge layer against `opentelemetry::global::tracer("cognee")`.

### Resource attributes

```rust
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_semantic_conventions::resource as semres;

let resource = Resource::builder()
    .with_attributes([
        KeyValue::new(semres::SERVICE_NAME, settings.otel_service_name.clone()),
        KeyValue::new(semres::SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
        KeyValue::new(
            semres::DEPLOYMENT_ENVIRONMENT_NAME,
            std::env::var("ENV").unwrap_or_else(|_| "development".into()),
        ),
    ])
    .build();
```

### Exporter and processor selection

Default to **gRPC** (matches Python's `_try_add_otlp_exporter` order):

```rust
let exporter = opentelemetry_otlp::SpanExporter::builder()
    .with_tonic()
    .with_endpoint(&settings.otel_exporter_otlp_endpoint)
    .with_headers(parse_headers(&settings.otel_exporter_otlp_headers)?)
    .build()?;

let provider = SdkTracerProvider::builder()
    .with_resource(resource)
    .with_batch_exporter(exporter)   // BatchSpanProcessor under the hood
    .build();
```

Use **`BatchSpanProcessor`**, not `SimpleSpanProcessor`. Python uses
the simple processor (one synchronous gRPC call per span) which is
acceptable for low-volume CLIs but harmful in the HTTP server. We can
afford the better default everywhere because we always ship the
flush-on-drop guard.

Allow opt-in HTTP via env: when
`OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` (the OTEL standard env var),
swap `.with_tonic()` for `.with_http().with_protocol(Protocol::HttpBinary)`.

### Bridge layer

```rust
let tracer = provider.tracer_builder("cognee")
    .with_version(env!("CARGO_PKG_VERSION"))
    .build();
let layer = tracing_opentelemetry::layer().with_tracer(tracer);
```

`opentelemetry::global::set_tracer_provider(provider.clone())` so the
rest of the process (and any external library that uses
`opentelemetry::global`) sees the same provider.

### Shutdown handling

```rust
pub struct OtelGuard {
    provider: Option<SdkTracerProvider>,
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            // 5 s timeout matches Python's force_flush(timeout_millis=30000)
            // scaled down for CLI responsiveness.
            let _ = provider.force_flush();
            let _ = provider.shutdown();
        }
    }
}
```

The CLI holds the guard for the lifetime of `main()`; the HTTP server
holds it on `AppState`. Both call paths drop the guard before
`std::process::exit`.

### When is OTEL enabled?

`init_telemetry(settings)` returns the bridge layer + guard if **either** of
these is true:

- `settings.cognee_tracing_enabled == true` (from
  `COGNEE_TRACING_ENABLED`); **or**
- `settings.otel_exporter_otlp_endpoint` is non-empty (matches Python's
  behaviour where setting the endpoint silently activates the exporter
  even without an explicit `enable_tracing()` call — see
  `is_tracing_enabled()` lazy init in trace_context.py:54–61).

When OTEL is disabled, `init_telemetry` returns a noop layer (a
`tracing_subscriber::layer::Identity`) so the subscriber composition
stays the same shape.

## Action items

Each item below has a dedicated implementation sub-document under [`01/`](01/) with rationale, prerequisites, step-by-step source-level changes, verification commands, files modified, and risks. **The sub-docs are authoritative**: where they refine details based on the locked design decisions (especially decision 1 — `telemetry` is **off** by default — and decision 6 — code lives in a new `cognee-observability` crate, not inside `cognee-lib`), follow the sub-doc rather than the high-level summary here.

| # | Action item | Sub-doc | Depends on | Status |
|---|---|---|---|---|
| 1 | Add OpenTelemetry workspace dependencies (`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `opentelemetry-semantic-conventions`, `tracing-opentelemetry`) pinned to `=0.31` / `=0.32` with both `grpc-tonic` and `http-proto` features. | [01/01-workspace-otel-deps.md](01/01-workspace-otel-deps.md) | — | ✅ 8cc50bb |
| 2 | Create the new `cognee-observability` workspace crate (manifest, `lib.rs` skeleton, feature wiring, register in workspace `members`). Scaffold only — implementation lands in task 4. | [01/02-observability-crate-scaffold.md](01/02-observability-crate-scaffold.md) | 1 | ✅ c88df3d |
| 3 | Forward the `telemetry` feature through `cognee-lib`, `cognee-cli`, and `cognee-http-server` to `cognee-observability/telemetry` + `cognee-core/telemetry`. **Not** in any `default = [...]` per decision 1. | [01/03-cognee-lib-feature-wiring.md](01/03-cognee-lib-feature-wiring.md) | 2 | ✅ ef813b9 |
| 4 | Implement `init_telemetry`, `TelemetryGuard`, `is_tracing_enabled`, `already_instrumented`, `parse_otlp_headers`, plus four new `Settings` fields (`otel_exporter_otlp_protocol`, `otel_span_processor`, `otel_traces_sampler`, `otel_traces_sampler_arg`) with env overlay. | [01/04-init-telemetry-implementation.md](01/04-init-telemetry-implementation.md) | 1, 2, 3 | ✅ 9b99576 |
| 5 | Re-export the public observability API from `cognee_lib::observability` so embedders use it through the umbrella crate. | [01/05-cognee-lib-reexports.md](01/05-cognee-lib-reexports.md) | 2, 3, 4 | ✅ 10bf00d |
| 6 | Refactor `crates/cli/src/main.rs`: move `load_settings()` into `main()` ahead of subscriber init (decision 11), compose the OTEL bridge layer with the existing `fmt` layer, hold the `TelemetryGuard` for the lifetime of `main`. | [01/06-cli-subscriber-refactor.md](01/06-cli-subscriber-refactor.md) | 4, 5 | ✅ 5b64d7d |
| 7 | Refactor the HTTP server subscriber: compose `fmt` + OTEL bridge + `SpanBufferLayer`, store `TelemetryGuard` on `AppState` (decision 9), ensure flush on graceful shutdown. | [01/07-http-server-subscriber-refactor.md](01/07-http-server-subscriber-refactor.md) | 4, 5 | ✅ 56433e5 |
| 8 | Implement the noop fallback so the public API exists with identity-layer semantics when `--features telemetry` is **off** (the default per decision 1). | [01/08-noop-fallback.md](01/08-noop-fallback.md) | 2, 4 | ✅ 5b925c7 |
| 9 | Unit tests for the observability crate: header parsing, `is_tracing_enabled` parity table, `init_telemetry` noop/active paths, `already_instrumented`, `TelemetryGuard::drop`, plus `Settings` env-overlay tests in `cognee-lib`. | [01/09-observability-unit-tests.md](01/09-observability-unit-tests.md) | 4, 8 | ✅ 52c2be7 |
| 10 | End-to-end integration test against an in-process tonic fake `TraceService`: assert spans actually flow over OTLP. | [01/10-otel-export-integration-test.md](01/10-otel-export-integration-test.md) | 4, 6, 9 | ✅ 6f08918 |
| 11 | User-facing documentation: new `docs/observability/opentelemetry.md` with env-var table, recipes (Tempo, Honeycomb, Dash0, in-cluster collector), span catalog, troubleshooting. Plus rustdoc updates and a README pointer. | [01/11-user-facing-documentation.md](01/11-user-facing-documentation.md) | 2–8 | ✅ 06ece23 |
| 12 | CI updates: add `--features telemetry` lanes (`check`, `clippy`, `test`, optional `doc`) and a `--no-default-features` lane in `.github/workflows/ci.yml` and `scripts/check_all.sh`. | [01/12-ci-updates.md](01/12-ci-updates.md) | 4, 8, 9, 10 | ✅ 0fc9adb |

Note: an out-of-band rename fixup commit `27c2bb2` aligned identifier
names across the observability crate; no logic change.

### Suggested execution order

A clean PR sequence based on the dependency graph above:

1. **PR 1** (foundation): tasks 01 + 02 + 03 — workspace deps, new crate scaffold, feature wiring.
2. **PR 2** (core impl): tasks 04 + 08 — `init_telemetry`, `TelemetryGuard`, noop fallback. The noop must land with the real impl so default builds keep working.
3. **PR 3** (surface): tasks 05 + 06 + 07 — re-exports plus CLI and HTTP server subscriber refactors.
4. **PR 4** (tests + CI): tasks 09 + 10 + 12 — unit + integration tests and the CI lanes that exercise them.
5. **PR 5** (docs): task 11 — user-facing documentation. Lands last so env-var names are stable.

## Design decisions (locked)

These supersede the earlier "Open questions" — answers were obtained
from the project owner on 2026-05-06 and are the binding contract for
all per-task sub-docs under [`01/`](01/).

| # | Decision | Resolution | Implication |
|---|---|---|---|
| 1 | `telemetry` cargo feature default | **OFF** by default in `cognee-lib`, `cognee-cli`, `cognee-http-server` | Plain `cargo build` excludes the OTEL deps; users opt in with `--features telemetry`. |
| 2 | Implicit activation | **YES** — non-empty `OTEL_EXPORTER_OTLP_ENDPOINT` activates without `COGNEE_TRACING_ENABLED=true` | Mirrors Python `is_tracing_enabled()` lazy-init semantics. |
| 3 | Exporter protocol(s) | Ship **both** gRPC (`tonic`) and HTTP/protobuf (`reqwest`); default gRPC; choose via `OTEL_EXPORTER_OTLP_PROTOCOL` | Two `opentelemetry-otlp` features enabled (`grpc-tonic`, `http-proto`). |
| 4 | Span processor | **Configurable** — new `Settings.otel_span_processor` field with `"batch"` (default) or `"simple"`; env var `OTEL_SPAN_PROCESSOR` overlay | Operators can downgrade to simple-sync if a collector behaves badly with batches. |
| 5 | Sampling | OTEL SDK reads `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG` automatically **and** new `Settings.otel_traces_sampler` / `Settings.otel_traces_sampler_arg` fields are exposed for programmatic config | Both env-driven and code-driven users covered. |
| 6 | Module placement | New workspace crate **`cognee-observability`** (sibling of `cognee-core`, etc.) | Allows reuse from `cognee-http-server` without going through `cognee-lib`; matches the per-crate conventions of the workspace. |
| 7 | CLI feature wiring | Add `telemetry = ["cognee-lib/telemetry"]` to `crates/cli/Cargo.toml`; **NOT** in `cli/default` (per decision 1) | Users enable with `cargo install cognee-cli --features telemetry`. |
| 8 | `android-default` | Do **NOT** include `telemetry` | Keeps Android binary lean; can be revisited later. |
| 9 | HTTP server guard ownership | Store `TelemetryGuard` on `AppState` | Lives for the full server lifetime; dropped on shutdown by axum/tokio. |
| 10 | Guard type name | **`TelemetryGuard`** | Matches the cargo feature name; broader than `OtelGuard` so future log/metric shutdown can extend it. |
| 11 | CLI settings load order | **Move `load_settings()` into `main()` before subscriber init** (option `(a)` from the design questions) | Subscriber sees correct OTEL config on first span; no two-stage init. |
| 12 | Metrics / logs export & `SpanBufferLayer` replacement | **Out of scope** for this gap. Tracked in the [Future Work](../gap-analysis.md#future-work--out-of-scope) section of the root gap analysis. | Keeps this gap shippable in one initiative. |

## Implementation sub-docs

The 12 numbered action items below are each elaborated into a per-task
sub-document under [`01/`](01/) with rationale, dependencies on other
tasks, a step-by-step implementation sequence, verification checklist,
modified files, and risks. References are listed alongside each
action item.

## Testing strategy

### Unit tests

In `crates/lib/src/observability/otel.rs` (`#[cfg(test)] mod tests`):

- `parse_otlp_headers_empty` — empty input returns `vec![]`.
- `parse_otlp_headers_single` — `"k=v"` → `[("k", "v")]`.
- `parse_otlp_headers_multi` — `"a=1,b=2"` → two entries.
- `parse_otlp_headers_whitespace` — surrounding spaces trimmed.
- `init_telemetry_disabled_returns_noop` — no env vars → guard is noop,
  layer is identity.
- `init_telemetry_enabled_builds_provider` — set env vars, assert provider
  was set globally and its type is `SdkTracerProvider`.
- `already_instrumented_default_false` — fresh process; without
  calling `set_tracer_provider`, returns `false`.

### Integration test

`crates/lib/tests/otel_export.rs` (gated `#[cfg(feature = "telemetry")]`):

- Spawn a tonic gRPC server implementing
  `opentelemetry-proto`'s `TraceService` on `127.0.0.1:0`.
- Set `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:<port>`.
- Build a `Settings` with `cognee_tracing_enabled = true`.
- Call `init_telemetry(&settings)`, attach the bridge to a fresh
  `Registry`, install via `tracing::subscriber::with_default`.
- Inside, call a function decorated with
  `#[tracing::instrument(name = "test.span", fields(foo = "bar"))]`.
- Drop the guard; assert the server received exactly one batch with
  one span named `"test.span"`, attribute `foo == "bar"`,
  resource attribute `service.name == "cognee"`.

### Cross-SDK parity (out of scope, future)

A follow-up could extend
[`e2e-cross-sdk/`](../../e2e-cross-sdk) with an `otel-collector`
service in `docker-compose.yml`, pointed at by both Python and Rust,
asserting both SDKs emit comparable span sets for the same operation.

## References

- [Python `setup_tracing` source](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py)
- [Python `trace_context` source](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/trace_context.py)
- [`opentelemetry` crate (latest)](https://crates.io/crates/opentelemetry)
- [`opentelemetry_sdk` 0.31](https://docs.rs/opentelemetry_sdk/0.31.0/opentelemetry_sdk/)
- [`opentelemetry-otlp` 0.31 docs](https://docs.rs/opentelemetry-otlp/0.31.0/opentelemetry_otlp/)
- [`tracing-opentelemetry` 0.32](https://docs.rs/tracing-opentelemetry/0.32.1/tracing_opentelemetry/)
- [OpenTelemetry Rust GitHub releases](https://github.com/open-telemetry/opentelemetry-rust/releases)
- [OTEL semantic conventions: resource attributes](https://opentelemetry.io/docs/specs/semconv/resource/)
- [OTLP spec: env vars (`OTEL_EXPORTER_OTLP_*`)](https://opentelemetry.io/docs/specs/otel/protocol/exporter/)

## Closure summary

This gap is closed. The 13 commits below shipped the work, in the order
they landed on `main`:

| # | Commit | Subject |
|---|---|---|
| 01-01 | `8cc50bb` | add OTEL/OTLP workspace dependencies |
| fixup | `27c2bb2` | rename `init_otel` → `init_telemetry` across crate and docs |
| 01-02 | `c88df3d` | scaffold `cognee-observability` crate |
| 01-03 | `ef813b9` | wire `telemetry` feature through `cognee-lib` |
| 01-04 | `9b99576` | implement `init_telemetry` with OTLP exporter and RAII guard |
| 01-05 | `10bf00d` | re-export telemetry surface from `cognee-lib` |
| 01-06 | `5b64d7d` | wire `init_telemetry` into the CLI subscriber stack |
| 01-07 | `56433e5` | wire `init_telemetry` into HTTP server + add `EnvSettingsView` |
| 01-08 | `5b925c7` | pin noop `init_telemetry` contract with test and rustdoc |
| 01-09 | `52c2be7` | add observability unit tests + fix `already_instrumented` heuristic |
| 01-10 | `6f08918` | add OTLP gRPC export integration test |
| 01-11 | `06ece23` | add user-facing OTEL/OTLP documentation |
| 01-12 | `0fc9adb` | add CI lanes for `telemetry` feature |
