# Task 11 — User-facing OpenTelemetry documentation

**Status**: Implemented in commit 06ece23
**Owner:** Observability working group
**Depends on:** Tasks [02 — observability crate scaffold](02-observability-crate-scaffold.md), [03 — `cognee-lib` feature wiring](03-cognee-lib-feature-wiring.md), 04 — config field additions, [05 — `cognee-lib` re-exports](05-cognee-lib-reexports.md), [06 — CLI subscriber refactor](06-cli-subscriber-refactor.md), 07 — HTTP-server subscriber refactor, 08 — no-deps fallback. The doc must describe the public API and env vars **as shipped**, so it lands after the implementation tasks settle.
**Referenced by:** Task 12 (CI) — the no-default-features lane should also build the rustdoc snippets that this task adds.

---

## Rationale

A separate user-facing document under `docs/observability/` (rather than only inline rustdoc or the gap-analysis doc under `docs/telemetry/`) is the right home because:

1. **Audience separation.** `docs/telemetry/` is the engineering gap-analysis space — Python parity tables, design decisions, action items. Operators wiring Tempo/Honeycomb should not have to read implementation history. `docs/observability/` becomes the operator-facing namespace, leaving room for future `docs/observability/logs.md` and `docs/observability/metrics.md` (see [Future Work](../gap-analysis.md#future-work--out-of-scope)).
2. **Recipe-driven format.** The most common request is "how do I point this at Tempo / Honeycomb / Dash0 / a Collector". Recipes with copy-pasteable snippets are far more useful than reference prose, and they don't fit in rustdoc.
3. **Discoverable from the README.** A single bullet under "Observability" in the project README that links here is enough — operators don't grep cargo docs, they read the repo.
4. **Rustdoc complements, doesn't duplicate.** `crates/lib/src/lib.rs` and `crates/observability/src/lib.rs` get short examples (5–15 lines) that link out to the operational doc for the full story.

The doc is **recipe-driven**: each downstream destination gets a self-contained section with the env vars, headers, and (where useful) a `docker-compose.yml` snippet.

---

## Pre-conditions

- Tasks 02–08 are merged; the public API surface (`cognee_observability::init_telemetry`, `TelemetryGuard`, `Settings.otel_*` fields) is stable and re-exported from `cognee_lib::telemetry` (the module is gated on the `telemetry` cargo feature).
- The `telemetry` cargo feature on `cognee-lib`, `cognee-cli`, and `cognee-http-server` is wired and gated as decided in [01-otel-otlp-export.md §Design decisions (locked)](../01-otel-otlp-export.md#design-decisions-locked) (OFF by default, opt-in).
- Env vars are finalised: `COGNEE_TRACING_ENABLED`, `OTEL_SERVICE_NAME`, `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_SPAN_PROCESSOR`, `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG`, `ENV`.

---

## Step-by-step

### 1. Create the directory

```bash
mkdir -p docs/observability
```

The folder does not currently exist (verified — `docs/` contains `api-gaps`, `api-v2`, `cli`, `delete-gaps`, `e2e-test-gaps`, `http-api-v2`, `http-server`, `tasks`, `telemetry`, `temporal`, etc., but no `observability`). Creating it now leaves room for sibling docs (logs, metrics) added by future Future-Work initiatives.

### 2. Author `docs/observability/opentelemetry.md`

Full content of the file is in the [Resulting content](#resulting-content) section below. The structure follows the 10-section sketch from the task brief: overview → quick start → env-var table → settings fields → programmatic recipe → destination recipes (Tempo, Honeycomb, Dash0, in-cluster Collector) → span catalog summary → `/api/v1/activity/spans` reference → troubleshooting → future work.

### 3. Update `crates/lib/src/lib.rs` rustdoc

Append a short OpenTelemetry section to the crate-level rustdoc (currently only six lines, lines 1–6). Suggested patch:

```rust
//! Unified public API for Cognee-Rust.
//!
//! This crate provides a single entry point by re-exporting the core operations:
//! - add (`AddPipeline`)
//! - cognify (`cognify()` free function and related types)
//! - search (`SearchBuilder`/`SearchOrchestrator` and related types)
//!
//! ## OpenTelemetry support
//!
//! Cognee emits structured spans for every pipeline stage, search retriever,
//! and HTTP route. To export them to an OTLP collector (Grafana Tempo,
//! Honeycomb, Dash0, in-cluster `otel-collector`, ...), enable the
//! `telemetry` cargo feature and set `OTEL_EXPORTER_OTLP_ENDPOINT`:
//!
//! ```no_run
//! # #[cfg(feature = "telemetry")] {
//! use cognee_lib::telemetry::{init_telemetry, TelemetryGuard};
//! use cognee_lib::config::Settings;
//! use tracing_subscriber::Registry;
//!
//! let settings = Settings::from_env();
//! let (_layer, _guard) = init_telemetry::<Registry>(&settings)
//!     .expect("telemetry init");
//! // ... compose `_layer` onto your subscriber; spans are flushed when
//! // `_guard` is dropped.
//! # }
//! ```
//!
//! See [`docs/observability/opentelemetry.md`](https://github.com/topoteretes/cognee-rust/blob/main/docs/observability/opentelemetry.md)
//! for the full operator guide, env-var reference, and deployment recipes.
```

The `# [cfg(feature = "telemetry")]` guard ensures the doctest still parses when the feature is off (no-default-features CI lane from task 12).

### 4. Update `crates/observability/src/lib.rs` rustdoc

The new `cognee-observability` crate (created by task 02) gets a longer crate-level rustdoc with a complete recipe — this is the place for "how do I use it from Rust" to live, while `docs/observability/opentelemetry.md` covers env vars and deployment topology.

Suggested addition (head of `crates/observability/src/lib.rs`):

```rust
//! # cognee-observability
//!
//! OpenTelemetry tracing pipeline for Cognee-Rust. Bridges the existing
//! `tracing` instrumentation (62+ `#[tracing::instrument]` sites across
//! the workspace) into an OTLP exporter so spans flow to a collector.
//!
//! ## Activation
//!
//! Tracing is activated when **either** of:
//! - `Settings.cognee_tracing_enabled == true`
//!   (env: `COGNEE_TRACING_ENABLED=true`)
//! - `Settings.otel_exporter_otlp_endpoint` is non-empty
//!   (env: `OTEL_EXPORTER_OTLP_ENDPOINT=https://...`)
//!
//! Either path triggers the same provider setup. This mirrors Python's
//! `is_tracing_enabled()` lazy-init semantics in
//! `cognee/modules/observability/trace_context.py`.
//!
//! ## Programmatic init
//!
//! ```no_run
//! use cognee_observability::{init_telemetry, TelemetryGuard};
//! use cognee_lib::config::Settings;
//! use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Registry};
//!
//! let mut settings = Settings::from_env();
//! settings.otel_service_name = "my-cognee-service".into();
//! settings.otel_exporter_otlp_endpoint = "http://localhost:4317".into();
//!
//! let (otel_layer, guard): (_, TelemetryGuard) =
//!     init_telemetry::<Registry>(&settings).expect("telemetry init");
//!
//! Registry::default()
//!     .with(tracing_subscriber::EnvFilter::from_default_env())
//!     .with(tracing_subscriber::fmt::layer())
//!     .with(otel_layer)
//!     .init();
//!
//! // Hold `guard` for the lifetime of your process; dropping it
//! // calls `force_flush()` then `shutdown()` on the OTEL provider.
//! drop(guard);
//! ```
//!
//! ## Configuration
//!
//! See [`docs/observability/opentelemetry.md`](https://github.com/topoteretes/cognee-rust/blob/main/docs/observability/opentelemetry.md)
//! for the full env-var reference and deployment recipes (Tempo, Honeycomb,
//! Dash0, in-cluster Collector).
```

### 5. README addition

The project root [`README.md`](../../../README.md) currently has no observability section. Add a short bullet immediately after the "Running Tests" section (around line 96):

```markdown
## Observability

Cognee emits OpenTelemetry traces from every pipeline stage. To export them
to an OTLP collector:

```bash
cargo build --release --features telemetry
OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.your-collector:4317 \
  cognee-cli search --query "what did we ingest yesterday?"
```

See [`docs/observability/opentelemetry.md`](docs/observability/opentelemetry.md)
for the full guide (env vars, recipes for Grafana Tempo, Honeycomb, Dash0,
and in-cluster Collectors).
```

### 6. Cross-link from the gap analysis

Edit [`docs/telemetry/gap-analysis.md`](../gap-analysis.md), specifically the "Future work / out of scope" section. After the bullet about OTEL metrics export, insert a forward pointer indicating that the operational documentation for the *implemented* OTEL traces lives at the new doc:

```markdown
> **Operator reference:** Once [01-otel-otlp-export.md](01-otel-otlp-export.md)
> ships, the canonical operator-facing documentation for tracing
> configuration and recipes is at
> [`docs/observability/opentelemetry.md`](../observability/opentelemetry.md).
> This `docs/telemetry/` folder remains the engineering gap-analysis space.
```

---

## Resulting content

Below is the complete, ready-to-commit content of `docs/observability/opentelemetry.md`. Author this file verbatim (modulo final wording polish) at PR time.

````markdown
# OpenTelemetry tracing in Cognee-Rust

Cognee-Rust emits OpenTelemetry-compatible traces from every pipeline stage,
search retriever, ingestion task, deletion cascade, and HTTP route. This
document is the operator's guide to wiring those traces into your
observability backend.

> **Audience:** Operators and embedders deploying Cognee-Rust who want
> spans to land in Grafana Tempo, Honeycomb, Dash0, an in-cluster
> OpenTelemetry Collector, or any other OTLP-speaking destination.

## Overview

- **What we emit:** Distributed traces composed of nested spans. 60+
  `#[tracing::instrument]` sites across the workspace are bridged into
  the OpenTelemetry SDK via `tracing-opentelemetry` when the `telemetry`
  cargo feature is enabled.
- **When:** Whenever a function on the instrumented hot paths runs —
  ingestion, cognify, search, delete, HTTP request handling.
- **Why:** End-to-end latency breakdowns, error attribution, capacity
  planning, and parity with the Python SDK's
  `cognee.modules.observability.tracing.setup_tracing()`.

Cognee additionally maintains an in-process **ring buffer** of the last
50 traces (configurable) for the `/api/v1/activity/spans` HTTP endpoint —
this is independent of OTLP and continues to work whether or not OTLP
export is enabled.

## Quick start

1. **Build with the `telemetry` feature.** The feature is opt-in (off by
   default to keep the binary small):
   ```bash
   cargo build --release --features telemetry           # cognee-lib
   cargo install cognee-cli --features telemetry        # CLI
   ```

2. **Set the OTLP endpoint.** Either env var below activates the exporter:
   ```bash
   export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
   # or, equivalently:
   export COGNEE_TRACING_ENABLED=true
   ```

3. **Run.** Spans flow to your collector:
   ```bash
   cognee-cli search --query "hello world"
   ```

That's it. The default exporter is OTLP/gRPC over `:4317`. To switch to
HTTP/protobuf set `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`.

## Environment variables

Cognee honours both the cognee-specific and the standard OpenTelemetry SDK
variables. Cognee-specific ones override the SDK defaults; the SDK reads
its own variables directly when not overridden.

| Variable | Default | Purpose |
|---|---|---|
| `COGNEE_TRACING_ENABLED` | `false` | Activates OTLP export when `true`/`1`/`yes`. Mirrors Python's `BaseConfig.cognee_tracing_enabled`. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | (unset) | OTLP endpoint URL. Setting this also activates export, even without `COGNEE_TRACING_ENABLED`. Examples: `http://localhost:4317` (gRPC), `http://localhost:4318` (HTTP). |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc` | Choose `grpc` or `http/protobuf`. |
| `OTEL_EXPORTER_OTLP_HEADERS` | (unset) | Comma-separated `key=value` pairs added to every export request. Used for vendor auth headers (Honeycomb, Dash0). |
| `OTEL_SERVICE_NAME` | `cognee` | The `service.name` resource attribute on every emitted span. |
| `OTEL_SPAN_PROCESSOR` | `batch` | `batch` (default, async) or `simple` (sync, one request per span). Use `simple` only when debugging a misbehaving collector. |
| `OTEL_TRACES_SAMPLER` | `parentbased_always_on` | OTEL standard. Common alternatives: `traceidratio`, `parentbased_traceidratio`. |
| `OTEL_TRACES_SAMPLER_ARG` | — | Sampler argument, e.g. `0.1` for 10% sampling with `traceidratio`. |
| `ENV` | `development` | Populates the `deployment.environment` resource attribute. |
| `COGNEE_SPAN_BUFFER_MAX_TRACES` | `50` | Max traces retained in the in-memory ring buffer (independent of OTLP). |
| `COGNEE_SPAN_BUFFER_MAX_SPANS_PER_TRACE` | `1024` | Max spans per trace in the in-memory ring buffer. |
| `RUST_LOG` | `info,ort=warn` | `tracing_subscriber::EnvFilter` directive. Set `RUST_LOG=debug` to see span lifecycle events on stdout alongside OTLP export. |

The OpenTelemetry Rust SDK additionally reads its own standard variables
(`OTEL_EXPORTER_OTLP_TIMEOUT`, `OTEL_EXPORTER_OTLP_COMPRESSION`,
`OTEL_RESOURCE_ATTRIBUTES`, etc.) — see the
[OTLP exporter spec](https://opentelemetry.io/docs/specs/otel/protocol/exporter/).

## Settings API (embedders)

Programmatic configuration mirrors the env vars on `cognee_lib::config::Settings`:

| Field | Type | Env var |
|---|---|---|
| `cognee_tracing_enabled` | `bool` | `COGNEE_TRACING_ENABLED` |
| `otel_service_name` | `String` | `OTEL_SERVICE_NAME` |
| `otel_exporter_otlp_endpoint` | `String` | `OTEL_EXPORTER_OTLP_ENDPOINT` |
| `otel_exporter_otlp_headers` | `String` | `OTEL_EXPORTER_OTLP_HEADERS` |
| `otel_exporter_otlp_protocol` | `String` | `OTEL_EXPORTER_OTLP_PROTOCOL` |
| `otel_span_processor` | `String` | `OTEL_SPAN_PROCESSOR` |
| `otel_traces_sampler` | `String` | `OTEL_TRACES_SAMPLER` |
| `otel_traces_sampler_arg` | `String` | `OTEL_TRACES_SAMPLER_ARG` |

`Settings::from_env()` overlays values from the environment.

## Programmatic init

For embedders building their own subscriber stack:

```rust
use cognee_lib::config::Settings;
use cognee_lib::telemetry::{init_telemetry, TelemetryGuard};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Registry};

fn main() -> anyhow::Result<()> {
    let mut settings = Settings::from_env();
    settings.otel_service_name = "my-cognee".into();
    settings.otel_exporter_otlp_endpoint = "http://localhost:4317".into();

    let (otel_layer, guard): (_, TelemetryGuard) = init_telemetry::<Registry>(&settings)?;

    Registry::default()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer) // identity layer when telemetry is disabled
        .init();

    // ... your application ...

    drop(guard); // force_flush() then shutdown(); usually at end of main()
    Ok(())
}
```

`TelemetryGuard` is an RAII handle: dropping it calls
`SdkTracerProvider::force_flush()` followed by `shutdown()` so the last
batch always reaches the collector before the process exits.

When the `telemetry` feature is **off** on `cognee-lib`, the
`cognee_lib::telemetry` module is not compiled and the
`cognee-observability` crate is not linked at all. Embedders that want a
single uniform call site can depend on `cognee-observability` directly
(its own `telemetry` feature controls the OTEL deps): with the feature
off, `init_telemetry` still compiles and returns an identity layer plus
a noop guard, so call sites need not be feature-gated.

## Recipes

### Local Grafana Tempo via OTEL Collector

Run a Collector that forwards to Tempo locally with Docker Compose:

```yaml
# docker-compose.yml
services:
  otel-collector:
    image: otel/opentelemetry-collector-contrib:0.106.0
    command: ["--config=/etc/otel/config.yml"]
    volumes:
      - ./otel-collector-config.yml:/etc/otel/config.yml
    ports:
      - "4317:4317"   # OTLP gRPC
      - "4318:4318"   # OTLP HTTP
  tempo:
    image: grafana/tempo:2.5.0
    command: ["-config.file=/etc/tempo.yml"]
    volumes:
      - ./tempo.yml:/etc/tempo.yml
    ports: ["3200:3200"]
  grafana:
    image: grafana/grafana:11.0.0
    ports: ["3000:3000"]
    environment:
      GF_AUTH_ANONYMOUS_ENABLED: "true"
      GF_AUTH_ANONYMOUS_ORG_ROLE: Admin
```

```yaml
# otel-collector-config.yml
receivers:
  otlp:
    protocols:
      grpc: { endpoint: 0.0.0.0:4317 }
      http: { endpoint: 0.0.0.0:4318 }
exporters:
  otlp/tempo:
    endpoint: tempo:4317
    tls: { insecure: true }
service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [otlp/tempo]
```

Run Cognee:

```bash
docker compose up -d
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
OTEL_SERVICE_NAME=cognee-local \
COGNEE_TRACING_ENABLED=true \
  cognee-cli search --query "test"
```

Open Grafana at `http://localhost:3000`, add Tempo (`http://tempo:3200`)
as a datasource, and search for `service.name = cognee-local`.

### Honeycomb

Honeycomb accepts OTLP directly. Authenticate via header:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=https://api.honeycomb.io
export OTEL_EXPORTER_OTLP_HEADERS=x-honeycomb-team=<YOUR_API_KEY>
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
export OTEL_SERVICE_NAME=cognee-prod
cognee-cli search --query "test"
```

Use HTTP/protobuf (port 443 effectively) for Honeycomb's managed
endpoint; gRPC also works on `api.honeycomb.io:443`.

### Dash0

Dash0 follows the same pattern with a different auth header:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=https://ingress.<region>.aws.dash0.com
export OTEL_EXPORTER_OTLP_HEADERS=Authorization=Bearer%20<DASH0_TOKEN>,Dash0-Dataset=production
export OTEL_SERVICE_NAME=cognee-prod
```

URL-encode the space between `Bearer` and the token (`%20`) per the OTEL
spec for `OTEL_EXPORTER_OTLP_HEADERS`.

### In-cluster OpenTelemetry Collector (Kubernetes)

Deploy the Collector as a sidecar or DaemonSet, point Cognee at the
service:

```yaml
# Pod spec snippet
env:
  - name: OTEL_EXPORTER_OTLP_ENDPOINT
    value: http://otel-collector.observability.svc.cluster.local:4317
  - name: OTEL_SERVICE_NAME
    valueFrom:
      fieldRef: { fieldPath: metadata.labels['app.kubernetes.io/name'] }
  - name: ENV
    value: production
  - name: COGNEE_TRACING_ENABLED
    value: "true"
```

The Collector handles the heavy lifting (batching, retries, failover,
multi-backend fan-out) so the Cognee process stays simple.

## Span catalog

A non-exhaustive map of where spans originate:

| Area | Crate / file | Example spans |
|---|---|---|
| Ingestion | `cognee-ingestion` | `add_pipeline.run`, `process_input`, `persist_data` |
| Chunking | `cognee-chunking` | `chunk_text`, `extract_chunks` |
| Cognify | `cognee-cognify` | `cognify`, `extract_graph`, `summarize`, `add_data_points` |
| Search | `cognee-search` | `search.{graph_completion,rag_completion,chunks,...}` |
| Delete | `cognee-delete` | `delete.preview`, `delete.cascade` |
| LLM | `cognee-llm` | `llm.api_call`, `llm.transcription_api_call` |
| HTTP | `cognee-http-server` | one span per route handler |

Every span carries semantic attributes from
[`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/tracing_keys.rs)
and [`crates/search/src/observability.rs`](../../crates/search/src/observability.rs)
(e.g. `cognee.dataset_id`, `cognee.search.type`, `cognee.llm.model`).

For the full list of currently-instrumented sites and the design rationale
behind it, see the engineering gap analysis at
[`../telemetry/01-otel-otlp-export.md`](../telemetry/01-otel-otlp-export.md).

## In-process span endpoint

Cognee additionally exposes a live view of the last N traces via the HTTP
server:

```bash
curl http://localhost:8000/api/v1/activity/spans
```

This endpoint is backed by an in-memory ring buffer
(`SpanBufferLayer`) and is **independent** of OTLP export — both can run
simultaneously. Useful for development and for in-product UIs that want
to render recent activity without a collector dependency.

Buffer size is bounded by `COGNEE_SPAN_BUFFER_MAX_TRACES` (default 50)
and `COGNEE_SPAN_BUFFER_MAX_SPANS_PER_TRACE` (default 1024).

## Troubleshooting

**"I configured the endpoint but no spans show up."**

1. Verify the feature is enabled: `cognee-cli --version` should mention
   `telemetry` in the build features (or run `cargo build --features
   telemetry` explicitly).
2. Verify activation: either `COGNEE_TRACING_ENABLED=true` or
   `OTEL_EXPORTER_OTLP_ENDPOINT` non-empty.
3. Set `RUST_LOG=debug,opentelemetry=trace,opentelemetry_sdk=trace` and
   look for `BatchSpanProcessor` lines — they log batch size and export
   results.
4. Confirm the collector is reachable from the host: `nc -vz <host> 4317`
   for gRPC, `curl http://<host>:4318/v1/traces` for HTTP.
5. Check firewalls — corporate egress often blocks `:4317`.
6. Try `OTEL_SPAN_PROCESSOR=simple` to bypass batching when debugging
   end-to-end connectivity.

**"Spans are missing fields I expect."**

Cognee uses `tracing-opentelemetry`'s default field translation: every
`tracing::field` becomes a span attribute. If a field is missing, check
that the call site uses `tracing::info!(field = ..., ...)` rather than
just formatting the value into the message.

**"I want to drop spans below my latency floor."**

Use `OTEL_TRACES_SAMPLER=traceidratio` with `OTEL_TRACES_SAMPLER_ARG=0.1`
to keep 10% uniformly. For latency-aware sampling, configure tail-based
sampling on the Collector rather than the SDK.

**"The collector falls over under load."**

Switch to a Collector deployment (DaemonSet or per-namespace) and have
your apps point at the local Collector. Don't export directly to a
managed vendor from every pod.

## Future work

OpenTelemetry **metrics** and **logs** export are out of scope for the
current initiative — see the
[Future Work section of the gap analysis](../telemetry/gap-analysis.md#future-work--out-of-scope)
for the planned scope. Cross-SDK OTEL parity testing (Python ↔ Rust)
against a shared collector is also tracked there.
````

---

## Verification

- [ ] `cargo doc --no-deps --features telemetry -p cognee-lib -p cognee-observability` builds without warnings; the rustdoc snippets in `crates/lib/src/lib.rs` and `crates/observability/src/lib.rs` compile.
- [ ] `cargo doc --no-deps -p cognee-lib` (no telemetry feature) also succeeds — the `# [cfg(feature = "telemetry")]` guard around the doctest must hold.
- [ ] All relative-path links in `docs/observability/opentelemetry.md` resolve. Run a markdown-link-check (e.g. `npx markdown-link-check docs/observability/opentelemetry.md`) or click through manually.
- [ ] The README bullet renders correctly on the GitHub front page (relative link to `docs/observability/opentelemetry.md` works).
- [ ] The `gap-analysis.md` cross-link points to a now-existing file.
- [ ] Manual smoke test: a fresh operator follows the Quick Start with a local Collector + Tempo and sees `service.name=cognee` traces appear in Grafana within 30 s.
- [ ] Honeycomb recipe verified by a maintainer with a sandbox API key (one-shot, not in CI).

---

## Files modified / created

| Path | Change |
|---|---|
| [`docs/observability/opentelemetry.md`](../../observability/opentelemetry.md) | **New** — full operator-facing guide (content above). |
| [`crates/lib/src/lib.rs`](../../../crates/lib/src/lib.rs) | Append `## OpenTelemetry support` rustdoc section with 5-line example. |
| `crates/observability/src/lib.rs` | New crate-level rustdoc with longer programmatic-init example. (Created in task 02; this task fills its `//!` header.) |
| [`README.md`](../../../README.md) | Add `## Observability` section after "Running Tests". |
| [`docs/telemetry/gap-analysis.md`](../gap-analysis.md) | Add cross-link in "Future work / out of scope" pointing at `docs/observability/opentelemetry.md` as the canonical operator reference once shipped. |

---

## Risks

- **Doc rot as env-var names evolve.** If task 04 (`Settings` field additions) renames any of `otel_span_processor` / `otel_traces_sampler` before merge, the env-var table here goes stale. Mitigation: this task lands *after* tasks 02–08 are merged, so the contract is frozen. A `// keep in sync with docs/observability/opentelemetry.md` comment near the `Settings` overlay code in `crates/lib/src/config.rs` is a good cheap insurance.
- **Rustdoc examples reference APIs that don't yet exist.** If this task is done out of order (before 02/05), the doctests in `lib.rs` / `observability/src/lib.rs` won't compile. Mitigation: explicit dependency on tasks 02–08 in the header. CI catches it via the existing `cargo doc` lane (gap analysis confirms it runs in `lib-tests.yml`).
- **Recipe drift.** Third-party endpoints change (Honeycomb URL pattern, Dash0 ingress hostnames). Mitigation: keep recipes minimal and link to vendor docs; revisit annually.
- **Quick Start oversimplification.** Running `cognee-cli search` with no prior `add` will produce zero spans on the search path that operators expect. Consider expanding the Quick Start with a one-line `cognee-cli add` first — but keep the doc focused on telemetry, not pipeline mechanics.
- **README expansion.** A new top-level `## Observability` section may invite scope creep (logs, metrics, ...). Keep the README bullet to 3–5 lines and link out.

---

## References

- Parent gap doc: [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md)
- Sibling sub-doc that depends on this one: task 12 — CI lanes that build `cargo doc` with and without `--features telemetry`.
- Telemetry gap-analysis root: [`../gap-analysis.md`](../gap-analysis.md)
- Python reference: [`cognee/modules/observability/tracing.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py),
  [`trace_context.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/trace_context.py)
- OTLP exporter env vars: <https://opentelemetry.io/docs/specs/otel/protocol/exporter/>
- OTEL semantic conventions for resources: <https://opentelemetry.io/docs/specs/semconv/resource/>
- `tracing-opentelemetry` 0.32 docs: <https://docs.rs/tracing-opentelemetry/0.32.1/tracing_opentelemetry/>
- Existing in-process span endpoint: [`crates/http-server/src/observability/span_buffer_layer.rs`](../../../crates/http-server/src/observability/span_buffer_layer.rs)
