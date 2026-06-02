# OpenTelemetry Tracing

Cognee-Rust ships built-in OpenTelemetry (OTEL) tracing. Every pipeline stage is
instrumented with `#[tracing::instrument]` (60+ span sites across the workspace).
The [`cognee-observability`](../../crates/observability/) crate bridges that
`tracing` instrumentation into an OTLP exporter so spans flow to a collector.

This mirrors Python's lazy-init tracing semantics
(`cognee/modules/observability/trace_context.py`).

## Enabling tracing

Tracing requires the `telemetry` cargo feature **and** runtime activation.

Build with the feature:

```bash
cargo build --release --features telemetry
```

> The umbrella `cognee-lib`, the CLI (`cognee-cli`), and the HTTP server enable
> `telemetry` through their default feature sets, so a plain release build of
> those binaries already includes the OTLP exporter code paths.

Then activate at runtime by setting **either**:

- `OTEL_EXPORTER_OTLP_ENDPOINT` to a non-empty collector URL, **or**
- `COGNEE_TRACING_ENABLED=true`

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.your-collector:4317 \
  cognee-cli search --query "what did we ingest yesterday?"
```

### Feature-state contract

`init_telemetry` always compiles and is safe to call unconditionally. It returns
a no-op tracing layer plus a no-op guard whenever the process is not configured
to export spans — that is, when **either** the `telemetry` feature is off at
compile time, **or** neither activation env var is set at runtime. Embedders can
therefore wire it in without branching on build configuration.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `COGNEE_TRACING_ENABLED` | `false` | Master toggle. `true` activates tracing even without an endpoint set. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | _(unset)_ | Collector URL. A non-empty value also activates tracing on its own. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc` | Transport: `grpc` or `http/protobuf`. |
| `OTEL_EXPORTER_OTLP_HEADERS` | _(unset)_ | Comma-separated `key=value` pairs (e.g. auth headers). |
| `OTEL_SERVICE_NAME` | `cognee` | `service.name` resource attribute. |
| `OTEL_SPAN_PROCESSOR` | `batch` | `batch` (recommended for production) or `simple`. |
| `OTEL_TRACES_SAMPLER` | `parentbased_always_on` | Sampler selection. Ratio-based samplers require `OTEL_TRACES_SAMPLER_ARG`. |
| `OTEL_TRACES_SAMPLER_ARG` | _(unset)_ | Sample ratio `0.0..=1.0` for ratio-based samplers. |

Unrecognized values for the protocol, span-processor, or sampler variables are
rejected at startup with a descriptive error rather than being silently ignored.

## Programmatic initialization

Drive `init_telemetry` from any `SettingsView`. The `EnvSettingsView` adapter
reads the env vars above directly, so callers that don't depend on `cognee-lib`
can still bring up the pipeline. Hold the returned `TelemetryGuard` for the
lifetime of the process — dropping it calls `force_flush()` then `shutdown()` on
the OTEL provider so no spans are lost on exit.

```rust,ignore
use cognee_observability::{init_telemetry, EnvSettingsView, TelemetryGuard};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Registry};

let settings = EnvSettingsView::from_env();
let (otel_layer, _guard): (_, TelemetryGuard) =
    init_telemetry::<Registry>(&settings).expect("telemetry init");

Registry::default()
    .with(otel_layer)
    .with(tracing_subscriber::EnvFilter::from_default_env())
    .with(tracing_subscriber::fmt::layer())
    .init();
```

`cognee_lib::config::Settings` implements `SettingsView`, so embedders that
already hold a `Settings` can pass it directly instead of `EnvSettingsView`.

## Deployment recipes

### Grafana Tempo (gRPC)

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=http://tempo:4317
export OTEL_EXPORTER_OTLP_PROTOCOL=grpc
```

### Honeycomb (HTTP)

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=https://api.honeycomb.io
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
export OTEL_EXPORTER_OTLP_HEADERS="x-honeycomb-team=YOUR_API_KEY"
```

### Dash0

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=https://ingress.dash0.com
export OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer YOUR_TOKEN"
```

### In-cluster OpenTelemetry Collector

Point at the collector's OTLP receiver service and let the collector fan out to
your backends:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector.observability.svc:4317
export OTEL_EXPORTER_OTLP_PROTOCOL=grpc
export OTEL_SPAN_PROCESSOR=batch
# Sample 10% of traces under high load:
export OTEL_TRACES_SAMPLER=traceidratio
export OTEL_TRACES_SAMPLER_ARG=0.1
```

## Product analytics vs. tracing

OTEL tracing (this document) is **distinct** from the opt-out product-analytics
client (`send_telemetry`). The latter posts anonymous aggregate usage events to
`https://test.prometh.ai` and is documented in
[`send_telemetry.md`](send_telemetry.md). The two are configured independently.
