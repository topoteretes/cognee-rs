//! Public entry point for OTEL bring-up.
//!
//! [`init_telemetry`] returns `(BoxedTelemetryLayer, TelemetryGuard)`.
//! Always succeeds in a usable way: when telemetry is disabled or the
//! build does not include the `telemetry` feature, returns a noop layer
//! plus a noop guard so call sites never need cfg-gating.

use crate::TelemetryInitError;
use crate::guard::TelemetryGuard;
use crate::settings::SettingsView;
use tracing::Subscriber;
use tracing_subscriber::{layer::Layer, registry::LookupSpan};

/// Type-erased layer compatible with any `tracing` registry that supports
/// `LookupSpan`. Boxing is what lets the disabled and enabled paths return
/// the same shape — the caller composes it onto a subscriber via
/// `.with(layer)` without seeing the underlying generic parameters.
pub type BoxedTelemetryLayer<S> = Box<dyn Layer<S> + Send + Sync + 'static>;

/// Python-parity check: should we initialize and emit OTEL spans?
///
/// Returns `true` when the operator has explicitly opted in via
/// `COGNEE_TRACING_ENABLED`, *or* implicitly opted in by setting
/// `OTEL_EXPORTER_OTLP_ENDPOINT` (Decision 2 in
/// `01-otel-otlp-export.md` — implicit activation).
pub fn is_tracing_enabled(settings: &dyn SettingsView) -> bool {
    settings.tracing_enabled() || !settings.otlp_endpoint().is_empty()
}

/// Mirror of Python `_is_auto_instrumented`: detect whether something
/// else has already installed a non-noop global tracer provider.
///
/// Without `telemetry` this can never be true (no OTEL deps to look at).
#[cfg(feature = "telemetry")]
pub fn already_instrumented() -> bool {
    // FIXME(otel-0.32+): the API crate does not expose a stable way to
    // identify the default noop provider, so we sniff the Debug repr.
    // The 0.31 default Debug repr contains "Noop"/"NoopTracerProvider";
    // an SDK provider installed by Datadog or Dash0 prints its own
    // type name. The dangerous failure mode (real provider classified
    // as noop) would let us overwrite it; we err on the side of NOT
    // installing — see the rationale in 04-init-telemetry-implementation.md.
    let provider = opentelemetry::global::tracer_provider();
    let dbg = format!("{provider:?}");
    !(dbg.contains("Noop") || dbg.contains("NoopTracerProvider"))
}

/// Stub used when `telemetry` is off — no provider can exist.
#[cfg(not(feature = "telemetry"))]
pub fn already_instrumented() -> bool {
    false
}

fn noop_layer<S>() -> BoxedTelemetryLayer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    Box::new(tracing_subscriber::layer::Identity::new())
}

/// Build the OTEL `tracing` layer and an RAII guard.
///
/// On success returns `(layer, guard)`. The layer must be added to the
/// subscriber via `.with(layer)`. The guard must be held until the
/// process is ready to exit; dropping it flushes pending spans.
///
/// # Sampler precedence
///
/// When `Settings.otel_traces_sampler` is set, it overrides the
/// `OTEL_TRACES_SAMPLER` env var. When it is empty, the OpenTelemetry
/// SDK's internal env-var reader picks up `OTEL_TRACES_SAMPLER` /
/// `OTEL_TRACES_SAMPLER_ARG` directly.
///
/// # Errors
///
/// Returns [`TelemetryInitError`] when sampler/protocol/processor names
/// are unrecognised or when the OTLP exporter fails to build. Callers
/// are free to log-and-continue: this function never panics.
pub fn init_telemetry<S>(
    settings: &dyn SettingsView,
) -> Result<(BoxedTelemetryLayer<S>, TelemetryGuard), TelemetryInitError>
where
    S: Subscriber + for<'span> LookupSpan<'span> + Send + Sync + 'static,
{
    if !is_tracing_enabled(settings) {
        return Ok((noop_layer::<S>(), TelemetryGuard::noop()));
    }

    #[cfg(not(feature = "telemetry"))]
    {
        // Reference settings to silence unused-warning on the noop path.
        let _ = settings.service_name();
        tracing::warn!(
            target: "cognee.observability",
            "tracing requested but cognee-observability was built without `telemetry` feature; spans stay local"
        );
        Ok((noop_layer::<S>(), TelemetryGuard::noop()))
    }

    #[cfg(feature = "telemetry")]
    {
        if already_instrumented() {
            // External tool installed a provider already — bridge to the
            // global tracer instead of installing our own (Decision 9
            // safety: never overwrite an externally-set provider).
            let tracer = opentelemetry::global::tracer("cognee");
            let layer = tracing_opentelemetry::layer().with_tracer(tracer);
            return Ok((Box::new(layer), TelemetryGuard::noop()));
        }

        let provider = telemetry_real::build_provider(settings)?;

        opentelemetry::global::set_tracer_provider(provider.clone());

        // 0.31 removed `tracer_builder("cognee")` in favour of building
        // an `InstrumentationScope` and passing it to `tracer_with_scope`.
        use opentelemetry::InstrumentationScope;
        use opentelemetry::trace::TracerProvider as _;
        let scope = InstrumentationScope::builder("cognee")
            .with_version(env!("CARGO_PKG_VERSION"))
            .build();
        let tracer = provider.tracer_with_scope(scope);
        let layer = tracing_opentelemetry::layer().with_tracer(tracer);

        Ok((Box::new(layer), TelemetryGuard::from_provider(provider)))
    }
}

#[cfg(feature = "telemetry")]
mod telemetry_real {
    use super::SettingsView;
    use crate::TelemetryInitError;

    pub(super) fn build_provider(
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

    fn build_resource(service_name: &str) -> opentelemetry_sdk::Resource {
        use opentelemetry::KeyValue;
        use opentelemetry_sdk::Resource;
        use opentelemetry_semantic_conventions::resource as semres;

        let env = std::env::var("ENV").unwrap_or_else(|_| "development".to_string());

        // `deployment.environment.name` is gated behind the
        // `semconv_experimental` feature in 0.31; spell it out as a
        // literal so we don't have to enable that feature workspace-wide.
        Resource::builder()
            .with_attributes([
                KeyValue::new(semres::SERVICE_NAME, service_name.to_string()),
                KeyValue::new(semres::SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                KeyValue::new("deployment.environment.name", env),
            ])
            .build()
    }

    fn build_exporter(
        settings: &dyn SettingsView,
    ) -> Result<opentelemetry_otlp::SpanExporter, TelemetryInitError> {
        use opentelemetry_otlp::{
            Protocol, SpanExporter, WithExportConfig, WithHttpConfig, WithTonicConfig,
        };

        let endpoint = settings.otlp_endpoint();
        let headers = crate::headers::parse_otlp_headers(settings.otlp_headers());

        match settings.otlp_protocol() {
            "grpc" | "" => {
                // gRPC takes headers as a tonic MetadataMap, which is
                // built from an `http::HeaderMap` to avoid pulling in
                // tonic's `MetadataKey`/`MetadataValue` types directly.
                let mut http_headers = http::HeaderMap::new();
                for (k, v) in &headers {
                    match (
                        http::header::HeaderName::try_from(k.as_str()),
                        http::header::HeaderValue::try_from(v.as_str()),
                    ) {
                        (Ok(name), Ok(value)) => {
                            http_headers.insert(name, value);
                        }
                        _ => {
                            tracing::warn!(
                                target: "cognee.observability",
                                header = %k,
                                "OTLP gRPC metadata header rejected (invalid name or value)"
                            );
                        }
                    }
                }
                let metadata = tonic::metadata::MetadataMap::from_headers(http_headers);
                SpanExporter::builder()
                    .with_tonic()
                    .with_endpoint(endpoint)
                    .with_metadata(metadata)
                    .build()
                    .map_err(TelemetryInitError::ExporterBuild)
            }
            "http/protobuf" | "http" => SpanExporter::builder()
                .with_http()
                .with_endpoint(endpoint)
                .with_protocol(Protocol::HttpBinary)
                .with_headers(headers.into_iter().collect())
                .build()
                .map_err(TelemetryInitError::ExporterBuild),
            other => Err(TelemetryInitError::UnknownProtocol(other.to_string())),
        }
    }

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

    fn apply_sampler(
        builder: opentelemetry_sdk::trace::TracerProviderBuilder,
        settings: &dyn SettingsView,
    ) -> Result<opentelemetry_sdk::trace::TracerProviderBuilder, TelemetryInitError> {
        use opentelemetry_sdk::trace::Sampler;

        let name = settings.traces_sampler();
        if name.is_empty() {
            // Empty means: defer to the SDK's internal OTEL_TRACES_SAMPLER reader.
            return Ok(builder);
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
}
