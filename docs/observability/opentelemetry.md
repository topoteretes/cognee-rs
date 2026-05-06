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
