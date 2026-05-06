# Task 01-04: `init_telemetry` core implementation

## Status

Not started.

## Owner / dependencies

- **Depends on**:
  - [Task 01-01 — workspace OTEL dependencies](01-workspace-otel-deps.md)
    must have added `opentelemetry`, `opentelemetry_sdk`,
    `opentelemetry-otlp`, `opentelemetry-semantic-conventions`, and
    `tracing-opentelemetry` to `[workspace.dependencies]`.
  - Task 01-02 — `cognee-observability` crate scaffold must exist with
    a `[features] telemetry = [...]` flag wiring the optional OTEL deps
    (per locked Decision 6).
  - Task 01-03 — `Settings` integration glue (the crate must already be
    able to depend on `cognee-lib` `Settings` either directly or through
    a thin `SettingsView` trait).
- **Blocks**:
  - Task 01-05 — re-export from `cognee-lib::prelude`.
  - Task 01-06 — CLI subscriber refactor (consumes `init_telemetry` +
    `TelemetryGuard`).
  - Task 01-07 — HTTP server subscriber refactor / `AppState` ownership
    (per locked Decision 9).
  - Task 01-09 — unit tests in this crate.
  - Task 01-11 — example binary (`examples/otel_smoke.rs`).
- **Owner**: TBD.

## Rationale

This task implements the heart of the OTEL gap: the function that
turns a populated `Settings` value into a live OTLP export pipeline,
plus the helpers that surround it. Several structural choices are
worth calling out up front so the implementer does not relitigate them:

- **Trait-object layer return.** `init_telemetry` returns
  `BoxedTelemetryLayer` (a type alias for
  `Box<dyn Layer<Registry> + Send + Sync + 'static>`) rather than a
  concrete `OpenTelemetryLayer<...>`. The concrete type leaks the
  `tracing_opentelemetry` 0.32 generic parameters into every binary
  call site (`cognee-cli`, `cognee-http-server`, examples). Boxing
  hides the type, keeps call sites clean, and makes the noop fallback
  (when `feature = "telemetry"` is off, or when OTEL is disabled at
  runtime) symmetric — both branches return the same boxed shape.
  Erasure cost is one virtual call per span event, which is
  negligible compared to OTLP serialization.
- **RAII guard.** Mirrors the locked Decision 9 / 10 contract: the
  guard owns the provider; dropping it flushes and shuts down. CLI
  code holds it for the lifetime of `main()`; the HTTP server holds
  it on `AppState`. There is no global mutable state for callers to
  manage. This also gives us a clean way to express "OTEL is off" —
  return a guard whose inner provider is `None`.
- **`is_tracing_enabled` lives in `cognee-observability`, not in
  `cognee-lib`.** The Python equivalent
  ([`trace_context.py:34–62`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/trace_context.py#L34))
  is the public answer to "should I bother emitting a span?". Putting
  it in the observability crate means HTTP middleware and other
  upstream callers do not need to depend on the umbrella library just
  to check the flag — this matches Decision 6 ("new workspace crate
  `cognee-observability`, sibling of `cognee-core`, allows reuse from
  `cognee-http-server` without going through `cognee-lib`").
- **`already_instrumented` matters in dev/test environments.**
  Auto-instrumentation tools (Datadog, Dash0 agents, the test harness
  for task 01-09 itself) install their own `TracerProvider` via
  `opentelemetry::global::set_tracer_provider`. If we then install
  ours we silently overwrite theirs (last writer wins) and the user
  sees their dashboards go dark. Mirroring Python's
  `_is_auto_instrumented()`
  ([`tracing.py:241–249`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py#L241))
  is the same defensive check.
- **Header parsing must not be left to `opentelemetry-otlp`'s
  built-in env reader.** The crate does pick up
  `OTEL_EXPORTER_OTLP_HEADERS` automatically, but Decision 11 (CLI
  load order) means we want headers expressed in code/config too — so
  programmatic users (HTTP server embedders, tests) get the same
  results as env-driven users. We keep our own parser; if both the env
  var and `Settings.otel_exporter_otlp_headers` are populated, our
  overlay (in `Settings::overlay_with_env`) writes the env value into
  the field, and we forward the field value to the exporter builder.
  The SDK then does **not** double-add (we choose one source).

## Pre-conditions

- Tasks 01-01, 01-02, 01-03 are merged.
- `cognee-observability` crate exists at `crates/observability/` with
  `lib.rs`, a `Cargo.toml` declaring the optional OTEL deps under
  `feature = "telemetry"`, and the basic `TelemetryInitError` thiserror
  enum scaffolded in 01-02.
- The locked design decisions in
  [`../01-otel-otlp-export.md` § Design decisions (locked)](../01-otel-otlp-export.md#design-decisions-locked)
  are still binding (re-read Decisions 2, 3, 4, 5, 10).

## Step-by-step

### 1. `Settings` extensions

Add four fields to the `Settings` struct in
[`crates/lib/src/config.rs:135-139`](../../../crates/lib/src/config.rs#L135),
adjacent to the existing observability block. These cover Decision 3
(protocol), Decision 4 (span processor), and Decision 5 (sampler
config).

```rust
// -- Observability -----------------------------------------------------------
pub cognee_tracing_enabled: bool,
pub otel_service_name: String,
pub otel_exporter_otlp_endpoint: String,
pub otel_exporter_otlp_headers: String,

/// OTLP transport: `"grpc"` (default) or `"http/protobuf"`.
/// Mirrors the OTEL spec env var `OTEL_EXPORTER_OTLP_PROTOCOL`.
pub otel_exporter_otlp_protocol: String,

/// Span processor mode: `"batch"` (default) or `"simple"`.
/// `simple` is synchronous-per-span and intended only for
/// debugging or for collectors known to misbehave with batches.
pub otel_span_processor: String,

/// Sampler name passed through to the OTEL SDK.
/// Empty string means: do not override; let the SDK read
/// `OTEL_TRACES_SAMPLER` itself (default `parentbased_always_on`).
/// Recognised values follow the OTEL spec:
/// `always_on`, `always_off`, `traceidratio`, `parentbased_always_on`,
/// `parentbased_always_off`, `parentbased_traceidratio`.
pub otel_traces_sampler: String,

/// Argument for the sampler. Currently only meaningful for the
/// `traceidratio` / `parentbased_traceidratio` samplers, which expect
/// a 0.0–1.0 ratio. Empty string means: do not override.
pub otel_traces_sampler_arg: String,
```

In `Settings::default()` (search the same file), set:

```rust
otel_exporter_otlp_protocol: "grpc".to_string(),
otel_span_processor: "batch".to_string(),
otel_traces_sampler: String::new(),
otel_traces_sampler_arg: String::new(),
```

(Defaults `"grpc"` and `"batch"` lock in Decisions 3 and 4. Sampler
defaults are empty so the SDK env-var path remains authoritative when
the operator does not set them programmatically — Decision 5.)

In `Settings::overlay_from_env`
([`config.rs:462-475`](../../../crates/lib/src/config.rs#L462)),
extend the Observability block:

```rust
// -- Observability -------------------------------------------------------
if let Some(v) = str_var("COGNEE_TRACING_ENABLED") {
    let v = v.to_lowercase();
    self.cognee_tracing_enabled = v == "true" || v == "1" || v == "yes";
}
if let Some(v) = str_var("OTEL_SERVICE_NAME") {
    self.otel_service_name = v;
}
if let Some(v) = str_var("OTEL_EXPORTER_OTLP_ENDPOINT") {
    self.otel_exporter_otlp_endpoint = v;
}
if let Some(v) = str_var("OTEL_EXPORTER_OTLP_HEADERS") {
    self.otel_exporter_otlp_headers = v;
}
if let Some(v) = str_var("OTEL_EXPORTER_OTLP_PROTOCOL") {
    self.otel_exporter_otlp_protocol = v;
}
if let Some(v) = str_var("OTEL_SPAN_PROCESSOR") {
    self.otel_span_processor = v;
}
if let Some(v) = str_var("OTEL_TRACES_SAMPLER") {
    self.otel_traces_sampler = v;
}
if let Some(v) = str_var("OTEL_TRACES_SAMPLER_ARG") {
    self.otel_traces_sampler_arg = v;
}
```

The protocol/processor variants are normalized at the consumption
site (`init_telemetry`) so unrecognized values yield a typed error
rather than silently degrading. Sampler env vars that the OTEL SDK
already reads are still mirrored into `Settings` so programmatic
callers can introspect them (Decision 5: "both env-driven and
code-driven users covered").

### 2. `TelemetryGuard`

New file `crates/observability/src/guard.rs`:

```rust
//! RAII guard that flushes and shuts down the OTEL pipeline on drop.
//!
//! The guard always exists, even when telemetry is disabled at compile
//! time (via the absent `telemetry` feature) or at runtime (no endpoint
//! configured). The disabled variant is a no-op so callers do not need
//! cfg-gating around the call site.

use std::time::Duration;
#[cfg(feature = "telemetry")]
use opentelemetry_sdk::trace::SdkTracerProvider;

/// Default budget for `force_flush` + `shutdown` combined. Matches
/// what an interactive CLI is willing to wait at exit; the HTTP server
/// holds the guard on `AppState` and accepts the same budget at
/// shutdown.
const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub struct TelemetryGuard {
    #[cfg(feature = "telemetry")]
    provider: Option<SdkTracerProvider>,
    timeout: Duration,
}

impl TelemetryGuard {
    /// Construct a noop guard. Drop is free.
    pub fn noop() -> Self {
        Self {
            #[cfg(feature = "telemetry")]
            provider: None,
            timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    #[cfg(feature = "telemetry")]
    pub(crate) fn from_provider(provider: SdkTracerProvider) -> Self {
        Self {
            provider: Some(provider),
            timeout: DEFAULT_SHUTDOWN_TIMEOUT,
        }
    }

    /// Override the flush+shutdown budget (mostly useful in tests).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        #[cfg(feature = "telemetry")]
        {
            if let Some(provider) = self.provider.take() {
                // `force_flush` and `shutdown_with_timeout` both return
                // `OTelSdkResult` (alias for `Result<(), TraceError>`).
                // We swallow errors — at this point we're tearing down
                // and there is no caller to surface the failure to.
                if let Err(err) = provider.force_flush() {
                    tracing::warn!(
                        target: "cognee.observability",
                        ?err,
                        "OTEL force_flush failed during TelemetryGuard drop"
                    );
                }
                if let Err(err) = provider.shutdown_with_timeout(self.timeout) {
                    tracing::warn!(
                        target: "cognee.observability",
                        ?err,
                        "OTEL shutdown_with_timeout failed during TelemetryGuard drop"
                    );
                }
            }
        }
    }
}
```

Two implementation notes:

1. The `provider: Option<...>` is only present under the `telemetry`
   feature. Without the feature, the guard is a unit-equivalent that
   stores only the unused `timeout` (kept so the `with_timeout` method
   has the same signature in both builds — easier on downstream
   tests).
2. `SdkTracerProvider::shutdown_with_timeout(&self, Duration)` is the
   0.31 API
   (see [docs.rs](https://docs.rs/opentelemetry_sdk/0.31.0/opentelemetry_sdk/trace/struct.SdkTracerProvider.html)).
   Earlier minor versions had a no-arg `shutdown` returning
   `TraceResult<()>`; if the workspace ever bumps OTEL, the call sites
   in this file are the only places that need changing.

### 3. Header parsing

New file `crates/observability/src/headers.rs`:

```rust
//! Parser for the `OTEL_EXPORTER_OTLP_HEADERS` comma-separated
//! `key=value` form, mirroring Python OTLP exporter behaviour.

/// Parse `"k1=v1,k2=v2"` into a list of `(key, value)` pairs.
///
/// - Surrounding whitespace on each pair and around `=` is trimmed.
/// - Empty pairs (e.g. trailing comma) are skipped.
/// - Pairs without an `=` are skipped (logged at WARN).
/// - Empty keys are skipped (a value with no key is meaningless).
/// - Empty values are kept (some collectors expect literal empty
///   headers, e.g. for clearing a default).
/// - Duplicate keys are kept in insertion order — the OTLP exporter
///   decides whether to overwrite or merge; we don't second-guess it.
pub fn parse_otlp_headers(input: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if input.trim().is_empty() {
        return out;
    }
    for pair in input.split(',') {
        let trimmed = pair.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            tracing::warn!(
                target: "cognee.observability",
                pair = trimmed,
                "OTLP header pair missing `=`; skipping"
            );
            continue;
        };
        let key = k.trim();
        let value = v.trim();
        if key.is_empty() {
            tracing::warn!(
                target: "cognee.observability",
                "OTLP header pair has empty key; skipping"
            );
            continue;
        }
        out.push((key.to_string(), value.to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert!(parse_otlp_headers("").is_empty());
        assert!(parse_otlp_headers("   ").is_empty());
    }

    #[test]
    fn single_pair() {
        assert_eq!(
            parse_otlp_headers("authorization=Bearer abc"),
            vec![("authorization".into(), "Bearer abc".into())]
        );
    }

    #[test]
    fn multiple_pairs_with_whitespace() {
        assert_eq!(
            parse_otlp_headers("  a = 1 , b=2,c=3  "),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "3".into()),
            ]
        );
    }

    #[test]
    fn malformed_pairs_skipped() {
        assert_eq!(parse_otlp_headers("nopair,=novalue,k=v"), vec![("k".into(), "v".into())]);
    }

    #[test]
    fn empty_value_kept() {
        assert_eq!(parse_otlp_headers("k="), vec![("k".into(), "".into())]);
    }

    #[test]
    fn trailing_comma() {
        assert_eq!(parse_otlp_headers("k=v,"), vec![("k".into(), "v".into())]);
    }
}
```

### 4. Resource construction

Inside `crates/observability/src/init.rs` (see step 9 for the file
overall), the resource builder is a private helper:

```rust
#[cfg(feature = "telemetry")]
fn build_resource(service_name: &str) -> opentelemetry_sdk::Resource {
    use opentelemetry::KeyValue;
    use opentelemetry_sdk::Resource;
    use opentelemetry_semantic_conventions::resource as semres;

    let env = std::env::var("ENV").unwrap_or_else(|_| "development".to_string());

    Resource::builder()
        .with_attributes([
            KeyValue::new(semres::SERVICE_NAME, service_name.to_string()),
            KeyValue::new(semres::SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
            KeyValue::new(semres::DEPLOYMENT_ENVIRONMENT_NAME, env),
        ])
        .build()
}
```

Notes:

- `env!("CARGO_PKG_VERSION")` resolves to the `cognee-observability`
  crate version. Since the workspace pins all crates to the same
  version (per existing pattern in `crates/lib/Cargo.toml`), this is
  the cognee version. If the workspace later splits versions per
  crate, swap to a constant exposed from `cognee-lib`.
- `DEPLOYMENT_ENVIRONMENT_NAME` (in 0.31) is the renamed semantic
  convention for what Python calls `deployment.environment`. The
  string value is the same.
- `Resource::builder()` is the 0.31 entry point; `Resource::new(...)`
  was deprecated in 0.30 and removed in 0.31.

### 5. Exporter selection

```rust
#[cfg(feature = "telemetry")]
fn build_exporter(
    settings: &SettingsView,
) -> Result<opentelemetry_otlp::SpanExporter, TelemetryInitError> {
    use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig, WithHttpConfig, WithTonicConfig};

    let endpoint = settings.otlp_endpoint();
    let headers = crate::headers::parse_otlp_headers(settings.otlp_headers());

    match settings.otlp_protocol() {
        "grpc" | "" => {
            // gRPC takes headers as a tonic MetadataMap.
            let mut metadata = tonic::metadata::MetadataMap::new();
            for (k, v) in &headers {
                if let (Ok(name), Ok(value)) = (
                    tonic::metadata::MetadataKey::from_bytes(k.as_bytes()),
                    v.parse::<tonic::metadata::MetadataValue<_>>(),
                ) {
                    metadata.insert(name, value);
                } else {
                    tracing::warn!(
                        target: "cognee.observability",
                        header = %k,
                        "OTLP gRPC metadata header rejected (invalid name or value)"
                    );
                }
            }
            SpanExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .with_metadata(metadata)
                .build()
                .map_err(TelemetryInitError::ExporterBuild)
        }
        "http/protobuf" | "http" => {
            SpanExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(Protocol::HttpBinary)
                .with_headers(headers.into_iter().collect())
                .build()
                .map_err(TelemetryInitError::ExporterBuild)
        }
        other => Err(TelemetryInitError::UnknownProtocol(other.to_string())),
    }
}
```

`SettingsView` is a small read-only adapter trait introduced in task
01-03 to avoid coupling `cognee-observability` to the full
`cognee-lib::Settings` type. Its methods are
`otlp_endpoint`, `otlp_headers`, `otlp_protocol`, `span_processor`,
`service_name`, `traces_sampler`, `traces_sampler_arg`,
`tracing_enabled` — all `&str`/`bool` return types, no allocation.

`with_metadata` (gRPC) and `with_headers` (HTTP) are different APIs
in `opentelemetry-otlp` 0.31 — `with_metadata` consumes a
`tonic::metadata::MetadataMap`, while `with_headers` consumes a
`HashMap<String, String>`. The two branches above are explicit about
this asymmetry rather than abstracting it.

### 6. Span processor selection

The OTEL 0.31 builder exposes `with_batch_exporter` and
`with_simple_exporter`. We pass them directly:

```rust
#[cfg(feature = "telemetry")]
fn install_exporter_on_builder(
    builder: opentelemetry_sdk::trace::TracerProviderBuilder,
    exporter: opentelemetry_otlp::SpanExporter,
    mode: &str,
) -> Result<opentelemetry_sdk::trace::TracerProviderBuilder, TelemetryInitError> {
    match mode {
        "batch" | "" => Ok(builder.with_batch_exporter(exporter)),
        "simple" => Ok(builder.with_simple_exporter(exporter)),
        other => Err(TelemetryInitError::UnknownSpanProcessor(other.to_string())),
    }
}
```

Both methods take `T: SpanExporter + 'static` and return `Self`, so
chaining is trivial. `with_batch_exporter` wraps the exporter in the
default `BatchSpanProcessor`; `with_simple_exporter` wraps it in
`SimpleSpanProcessor`. There is no public way to tune the batch
processor's queue size / scheduling delay through the builder in
0.31 — those would need a manual `BatchSpanProcessor::builder()`
chain, which is out of scope for this task.

### 7. Provider construction

Pulling steps 4–6 together:

```rust
#[cfg(feature = "telemetry")]
fn build_provider(
    settings: &dyn SettingsView,
) -> Result<opentelemetry_sdk::trace::SdkTracerProvider, TelemetryInitError> {
    use opentelemetry_sdk::trace::SdkTracerProvider;

    let resource = build_resource(settings.service_name());
    let exporter = build_exporter(settings)?;

    let mut builder = SdkTracerProvider::builder().with_resource(resource);
    builder = install_exporter_on_builder(builder, exporter, settings.span_processor())?;
    builder = apply_sampler(builder, settings)?;

    Ok(builder.build())
}
```

After `build`, install globally so external libraries that call
`opentelemetry::global::tracer(...)` see our provider:

```rust
opentelemetry::global::set_tracer_provider(provider.clone());
```

Cloning is cheap — `SdkTracerProvider` is `Clone` and internally
ref-counted. The clone we hand to the global registry shares state
with the one stored in `TelemetryGuard`. `Drop` on the guard
flushes/shuts down the underlying provider; the global handle is
left dangling but harmless because the OTEL SDK's noop fallback kicks
in once the inner state is shut down.

### 8. Sampler wiring

Decision 5 says the SDK env vars must keep working *and* programmatic
config must be exposed. Since `opentelemetry_sdk` 0.31 reads
`OTEL_TRACES_SAMPLER` automatically on
`SdkTracerProvider::builder().build()` only when no sampler is set
explicitly, the precedence becomes:

1. If `Settings.otel_traces_sampler` is non-empty, parse it into a
   `Sampler` and call `.with_sampler(...)` — this overrides the env.
2. Otherwise, do **not** call `.with_sampler` and let the SDK's
   internal env-reading path apply.

```rust
#[cfg(feature = "telemetry")]
fn apply_sampler(
    builder: opentelemetry_sdk::trace::TracerProviderBuilder,
    settings: &dyn SettingsView,
) -> Result<opentelemetry_sdk::trace::TracerProviderBuilder, TelemetryInitError> {
    use opentelemetry_sdk::trace::Sampler;

    let name = settings.traces_sampler();
    if name.is_empty() {
        return Ok(builder); // SDK reads OTEL_TRACES_SAMPLER itself.
    }

    let arg = settings.traces_sampler_arg();
    let sampler = match name {
        "always_on" => Sampler::AlwaysOn,
        "always_off" => Sampler::AlwaysOff,
        "traceidratio" => Sampler::TraceIdRatioBased(parse_ratio(arg)?),
        "parentbased_always_on" => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
        "parentbased_always_off" => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
        "parentbased_traceidratio" => {
            Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(parse_ratio(arg)?)))
        }
        other => return Err(TelemetryInitError::UnknownSampler(other.to_string())),
    };
    Ok(builder.with_sampler(sampler))
}

fn parse_ratio(arg: &str) -> Result<f64, TelemetryInitError> {
    if arg.is_empty() {
        return Err(TelemetryInitError::SamplerArgRequired);
    }
    arg.parse::<f64>()
        .map_err(|_| TelemetryInitError::InvalidSamplerArg(arg.to_string()))
        .and_then(|f| {
            if (0.0..=1.0).contains(&f) {
                Ok(f)
            } else {
                Err(TelemetryInitError::InvalidSamplerArg(arg.to_string()))
            }
        })
}
```

Document the precedence in the rustdoc on `init_telemetry`:

> When `Settings.otel_traces_sampler` is set, it overrides the
> `OTEL_TRACES_SAMPLER` env var. When it is empty, the OpenTelemetry
> SDK's internal env-var reader picks up `OTEL_TRACES_SAMPLER` /
> `OTEL_TRACES_SAMPLER_ARG` directly.

### 9. `init_telemetry` top-level function

New file `crates/observability/src/init.rs`:

```rust
//! Public entry point for OTEL bring-up.
//!
//! `init_telemetry` returns `(BoxedTelemetryLayer, TelemetryGuard)`.
//! Always succeeds in a usable way: when telemetry is disabled or the
//! build does not include the `telemetry` feature, returns a noop
//! layer + noop guard so call sites never need cfg-gating.

use crate::guard::TelemetryGuard;
use crate::settings::SettingsView;
use crate::TelemetryInitError;
use tracing::Subscriber;
use tracing_subscriber::{
    layer::Layer,
    registry::LookupSpan,
};

/// Type-erased layer compatible with any `tracing` registry that
/// supports `LookupSpan`. Boxing is what lets the disabled and
/// enabled paths return the same shape.
pub type BoxedTelemetryLayer<S> = Box<dyn Layer<S> + Send + Sync + 'static>;

/// Build the OTEL `tracing` layer and an RAII guard.
///
/// On success returns `(layer, guard)`. The layer must be added to
/// the subscriber via `.with(layer)`. The guard must be held until
/// the process is ready to exit; dropping it flushes pending spans.
///
/// On error the function still returns a usable noop layer + noop
/// guard, after logging the error at WARN. This means:
/// **the caller never has to handle a Result from this function for
/// process startup to succeed**. The `Result` shape is reserved for
/// tests that want to assert successful initialization.
pub fn init_telemetry<S>(
    settings: &dyn SettingsView,
) -> Result<(BoxedTelemetryLayer<S>, TelemetryGuard), TelemetryInitError>
where
    S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync + 'static,
{
    if !crate::is_tracing_enabled(settings) {
        return Ok((noop_layer::<S>(), TelemetryGuard::noop()));
    }

    #[cfg(not(feature = "telemetry"))]
    {
        tracing::warn!(
            target: "cognee.observability",
            "tracing requested but cognee-observability was built without `telemetry` feature; spans stay local"
        );
        Ok((noop_layer::<S>(), TelemetryGuard::noop()))
    }

    #[cfg(feature = "telemetry")]
    {
        if crate::already_instrumented() {
            // External tool installed a provider already — bridge to
            // the global tracer instead of installing our own.
            let tracer = opentelemetry::global::tracer("cognee");
            let layer = tracing_opentelemetry::layer().with_tracer(tracer);
            return Ok((Box::new(layer), TelemetryGuard::noop()));
        }

        let provider = match crate::init::build_provider(settings) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(
                    target: "cognee.observability",
                    ?err,
                    "OTEL provider build failed; continuing without remote export"
                );
                return Ok((noop_layer::<S>(), TelemetryGuard::noop()));
            }
        };

        opentelemetry::global::set_tracer_provider(provider.clone());

        // Use the cloned provider for our own tracer so the bridge
        // does not depend on the global state being set.
        //
        // 0.31 removed `tracer_builder("cognee")` in favour of
        // building an `InstrumentationScope` and passing it to
        // `tracer_with_scope`.
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry::InstrumentationScope;
        let scope = InstrumentationScope::builder("cognee")
            .with_version(env!("CARGO_PKG_VERSION"))
            .build();
        let tracer = provider.tracer_with_scope(scope);
        let layer = tracing_opentelemetry::layer().with_tracer(tracer);

        Ok((Box::new(layer), TelemetryGuard::from_provider(provider)))
    }
}

fn noop_layer<S>() -> BoxedTelemetryLayer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    // `tracing_subscriber::layer::Identity` has a blanket
    // `Layer<S>` impl that does nothing.
    Box::new(tracing_subscriber::layer::Identity::new())
}
```

The choice to **return Ok for the error path** is deliberate:
observability failures must never crash the host process. Tests that
want to assert success use the regular `Result` and check that the
guard's inner provider is `Some` via a debug-only inspector method
(added in task 01-09).

### 10. `is_tracing_enabled`

Lives at the crate root (`crates/observability/src/lib.rs`). Mirrors
[Python `is_tracing_enabled`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/trace_context.py#L34):

```rust
/// Python-parity check: should we initialize and emit OTEL spans?
///
/// Returns `true` when the operator has explicitly opted in via
/// `COGNEE_TRACING_ENABLED` *or* implicitly opted in by setting
/// `OTEL_EXPORTER_OTLP_ENDPOINT` (Decision 2 in
/// `01-otel-otlp-export.md` — implicit activation).
pub fn is_tracing_enabled(settings: &dyn SettingsView) -> bool {
    settings.tracing_enabled() || !settings.otlp_endpoint().is_empty()
}
```

This is the one piece of public surface that does NOT depend on the
`telemetry` feature — a build without OTEL deps can still answer
"is tracing wanted here?" so binding glue (Python wrapper, JS
wrapper) can decide whether to surface a warning.

### 11. `already_instrumented`

Mirror of Python `_is_auto_instrumented`. Lives next to
`is_tracing_enabled` and is also feature-independent (returns `false`
when the feature is off, since without OTEL deps we cannot see a
provider anyway):

```rust
#[cfg(feature = "telemetry")]
pub fn already_instrumented() -> bool {
    // The default global before anyone calls `set_tracer_provider`
    // is the "noop" provider exposed by the API crate. Its concrete
    // type does not have a stable public name we can compare against,
    // so we match on the Debug repr — Python does the same with
    // `type(current).__name__ == "ProxyTracerProvider"`.
    //
    // The 0.31 default Debug repr contains either "NoopTracerProvider"
    // or "GlobalTracerProvider { /* noop inner */ ... }". Anything
    // else (e.g. an SDK provider installed by Datadog) will print
    // its own type name.
    let provider = opentelemetry::global::tracer_provider();
    let dbg = format!("{provider:?}");
    !(dbg.contains("Noop") || dbg.contains("NoopTracerProvider"))
}

#[cfg(not(feature = "telemetry"))]
pub fn already_instrumented() -> bool {
    false
}
```

The Debug-sniffing approach is fragile against future OTEL releases.
Document this in a `// FIXME(otel-0.32+):` comment so a future
upgrade reviews the assumption. The alternative — downcasting via
`Any` — does not work because `opentelemetry::global::tracer_provider`
returns a `GlobalTracerProvider` wrapper that erases the inner type.

### 12. Module exports

`crates/observability/src/lib.rs`:

```rust
//! Cognee observability primitives: OTEL bring-up, telemetry guard,
//! and tracing helpers shared between `cognee-cli`, `cognee-http-server`,
//! and `cognee-lib::api`.

mod guard;
mod headers;
mod init;
mod settings;

#[cfg(feature = "telemetry")]
mod error;

pub use guard::TelemetryGuard;
pub use headers::parse_otlp_headers;
pub use init::{init_telemetry, BoxedTelemetryLayer};
pub use settings::SettingsView;

#[cfg(feature = "telemetry")]
pub use error::TelemetryInitError;

// When the feature is off, `TelemetryInitError` is a unit-variant stub
// so the public signatures do not change shape.
#[cfg(not(feature = "telemetry"))]
#[derive(Debug, thiserror::Error)]
pub enum TelemetryInitError {
    #[error("cognee-observability built without `telemetry` feature")]
    FeatureDisabled,
}

pub use init::is_tracing_enabled;
pub use init::already_instrumented;
```

`error.rs` (feature-gated) — the variants used above:

```rust
#[derive(Debug, thiserror::Error)]
pub enum TelemetryInitError {
    #[error("OTLP exporter build failed: {0}")]
    ExporterBuild(#[source] opentelemetry_otlp::ExporterBuildError),

    #[error("unknown OTEL_EXPORTER_OTLP_PROTOCOL: {0} (expected `grpc` or `http/protobuf`)")]
    UnknownProtocol(String),

    #[error("unknown OTEL_SPAN_PROCESSOR: {0} (expected `batch` or `simple`)")]
    UnknownSpanProcessor(String),

    #[error("unknown OTEL_TRACES_SAMPLER: {0}")]
    UnknownSampler(String),

    #[error("OTEL_TRACES_SAMPLER_ARG required for ratio-based samplers")]
    SamplerArgRequired,

    #[error("invalid OTEL_TRACES_SAMPLER_ARG: {0} (expected 0.0..=1.0)")]
    InvalidSamplerArg(String),
}
```

`settings.rs`:

```rust
//! Read-only view of the observability-relevant subset of `Settings`.
//! Defined here (not in `cognee-lib`) to avoid a hard dependency on
//! the umbrella crate. `cognee-lib::Settings` implements this trait
//! in task 01-03.

pub trait SettingsView: Send + Sync {
    fn tracing_enabled(&self) -> bool;
    fn service_name(&self) -> &str;
    fn otlp_endpoint(&self) -> &str;
    fn otlp_headers(&self) -> &str;
    fn otlp_protocol(&self) -> &str;
    fn span_processor(&self) -> &str;
    fn traces_sampler(&self) -> &str;
    fn traces_sampler_arg(&self) -> &str;
}
```

## Resulting code

The five new files together. (`error.rs` and `settings.rs` are
small; the substantive ones are `init.rs`, `guard.rs`, `headers.rs`.)
See the snippets above — they are intentionally complete and
copy-pasteable into the eventual PR. The only piece that is *not*
inlined here is the `cognee-lib::Settings` `impl SettingsView` block,
which belongs to task 01-03.

## Verification

A self-contained smoke example, scaffolded by task 01-11 (this task
only describes what it should cover):

`examples/otel_smoke.rs`:

- Spawns a tonic gRPC server on `127.0.0.1:0` that records every
  `ExportTraceServiceRequest` it receives.
- Builds a `Settings` with
  `cognee_tracing_enabled = true`,
  `otel_exporter_otlp_endpoint = format!("http://127.0.0.1:{port}")`,
  `otel_service_name = "cognee-otel-smoke"`.
- Calls `init_telemetry::<Registry>(&settings)`.
- Composes `Registry::default().with(layer)`.
- Inside `tracing::subscriber::with_default(...)`, runs a function
  decorated with
  `#[tracing::instrument(name = "smoke.test_span", fields(answer = 42))]`.
- Drops the guard.
- Asserts the server received exactly one batch with a span named
  `smoke.test_span`, attribute `answer == 42`, resource attribute
  `service.name == "cognee-otel-smoke"`, and resource attribute
  `service.version == env!("CARGO_PKG_VERSION")`.

Beyond that, run inside the workspace:

```bash
cargo check -p cognee-observability --features telemetry
cargo check -p cognee-observability --no-default-features
cargo test -p cognee-observability --features telemetry
scripts/check_all.sh
```

The `--no-default-features` lane confirms the noop path compiles
without OTEL deps (required by Decision 1).

## Files modified

- [`crates/lib/src/config.rs`](../../../crates/lib/src/config.rs) —
  add four new `Settings` fields and their env-overlay block.
- `crates/observability/src/lib.rs` (new) — module wiring,
  `is_tracing_enabled`, `already_instrumented`, re-exports.
- `crates/observability/src/init.rs` (new) — `init_telemetry`,
  `build_provider`, `build_exporter`, `apply_sampler`,
  `install_exporter_on_builder`, `build_resource`.
- `crates/observability/src/guard.rs` (new) — `TelemetryGuard` +
  `Drop`.
- `crates/observability/src/headers.rs` (new) —
  `parse_otlp_headers` + unit tests.
- `crates/observability/src/error.rs` (new, feature-gated) —
  `TelemetryInitError`.
- `crates/observability/src/settings.rs` (new) — `SettingsView`
  trait.

## Risks

- **`opentelemetry_sdk` API churn.** Between minor versions the SDK
  has renamed types (`TracerProvider` → `SdkTracerProvider`),
  changed `force_flush` / `shutdown` signatures (now both return
  `Result`), and shifted resource construction
  (`Resource::new` → `Resource::builder`). Locking
  `opentelemetry_sdk = "=0.31"` (per task 01-01) is the mitigation;
  whenever the workspace bumps the version, this file is the
  primary review surface. The risk is amplified because the
  underlying APIs are still pre-1.0.
- **Double-shutdown panics.** If a future edit to the CLI puts the
  guard inside an `Arc` and another `tokio::spawn` task holds a
  clone, dropping in two threads can race. `SdkTracerProvider`'s
  `shutdown_with_timeout` is documented to be safe under repeated
  calls (returns `Err(AlreadyShutdown)`), but the unit tests should
  pin this with a deliberate-double-drop case (task 01-09).
- **Transport dependency conflicts.** `opentelemetry-otlp` with
  `grpc-tonic` pulls in `tonic` and `prost`; `http-proto` pulls in
  `reqwest` (rustls-tls in our workspace). The cognee workspace
  already uses `reqwest = { features = ["rustls-tls"] }` and `tonic`
  in some sub-crates. A version mismatch (e.g. `tonic = "0.10"`
  elsewhere vs `tonic = "0.12"` from `opentelemetry-otlp` 0.31)
  produces compile errors that are slow to triage. Mitigation:
  during task 01-01, bump every `tonic`/`prost` direct dep to the
  versions OTEL needs; if a transitive crate (e.g. `qdrant`) pins an
  older `tonic`, accept dual-versions in `Cargo.lock` rather than
  forcing a workspace lockstep.
- **Sampler parsing edge cases.** Operators may pass values the OTEL
  spec recognises but our parser does not (e.g. trailing whitespace
  in `OTEL_TRACES_SAMPLER`, mixed case). Decision: trim and
  lowercase before matching. Document the canonical names in the
  rustdoc and reject unknown ones loudly so misconfigurations
  surface in CI rather than at runtime.
- **`already_instrumented` Debug-string fragility.** Reviewed
  inline. The fallback (a noop provider mistakenly classified as
  "instrumented") is harmless — the bridge layer simply attaches to
  the global noop tracer and emits nothing. The opposite fallback
  (a real external provider classified as "noop" and overwritten by
  our own) is the dangerous case, so when in doubt err on the side
  of NOT installing our own provider.

## Open / clarifying questions

None — the design decisions in
[`../01-otel-otlp-export.md` § Design decisions (locked)](../01-otel-otlp-export.md#design-decisions-locked)
fully cover this task. The one borderline item, "should
`already_instrumented` use Debug-sniffing or downcasting?", is
documented inline as a `// FIXME(otel-0.32+):` review note rather
than escalated.

## References

- Python source — `setup_tracing`, `_try_add_otlp_exporter`,
  `_is_auto_instrumented`:
  [`tracing.py:241–345`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py#L241).
- Python source — `is_tracing_enabled`:
  [`trace_context.py:34–62`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/trace_context.py#L34).
- OTEL Rust SDK 0.31 — `SdkTracerProvider`:
  [docs.rs](https://docs.rs/opentelemetry_sdk/0.31.0/opentelemetry_sdk/trace/struct.SdkTracerProvider.html).
- OTEL Rust SDK 0.31 — `TracerProviderBuilder`:
  [docs.rs](https://docs.rs/opentelemetry_sdk/0.31.0/opentelemetry_sdk/trace/struct.TracerProviderBuilder.html).
- OTEL Rust SDK 0.31 — `Sampler`:
  [docs.rs](https://docs.rs/opentelemetry_sdk/0.31.0/opentelemetry_sdk/trace/enum.Sampler.html).
- `opentelemetry-otlp` 0.31 — `SpanExporter`,
  `WithExportConfig`/`WithTonicConfig`/`WithHttpConfig`:
  [docs.rs](https://docs.rs/opentelemetry-otlp/0.31.0/opentelemetry_otlp/).
- `tracing-opentelemetry` 0.32 — `layer()`:
  [docs.rs](https://docs.rs/tracing-opentelemetry/0.32.0/tracing_opentelemetry/fn.layer.html).
- OTEL semantic conventions (resource attributes):
  [opentelemetry.io](https://opentelemetry.io/docs/specs/semconv/resource/).
- OTLP env-var spec (`OTEL_EXPORTER_OTLP_*`):
  [opentelemetry.io](https://opentelemetry.io/docs/specs/otel/protocol/exporter/).
- Parent design doc, full proposal:
  [`../01-otel-otlp-export.md`](../01-otel-otlp-export.md) — see
  especially "Proposed design" and "Design decisions (locked)".
- Sibling task — workspace deps:
  [`01-workspace-otel-deps.md`](01-workspace-otel-deps.md).
