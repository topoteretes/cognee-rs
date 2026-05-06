//! Integration-style tests for `init_telemetry` activation paths.
//!
//! These tests mutate process-global OTEL state via
//! `opentelemetry::global::set_tracer_provider`, which is install-once.
//! In addition, `cognee_observability` records its own installation in a
//! module-private `OnceLock` (see `crates/observability/src/init.rs`)
//! so that `already_instrumented()` flips to `true` for the rest of the
//! process. Because of this once-per-process state, splitting the
//! activation paths across separate `#[test]` functions in the same
//! binary makes ordering load-bearing — and `#[serial_test::serial]`
//! only enforces non-overlap, not a stable order across functions.
//!
//! To keep the assertions deterministic and self-contained, all
//! activation-path checks live in a single async test function below.
//! The function exercises:
//!   1. `already_instrumented()` is `false` on a fresh process.
//!   2. The first `init_telemetry` call with valid settings installs an
//!      SDK provider; the returned guard owns the provider; the
//!      `OUR_PROVIDER_INSTALLED` `OnceLock` flips so
//!      `already_instrumented()` is now `true`.
//!   3. A second `init_telemetry` call is idempotent — it returns
//!      `Ok((bridge_layer, noop_guard))` rather than overwriting the
//!      installed provider.
//!   4. After installation, `init_telemetry` calls with otherwise
//!      invalid inputs (empty endpoint, unknown protocol) still return
//!      `Ok` via the bridge branch, because the bridge path does not
//!      consult endpoint/protocol settings.
//!
//! The `UnknownProtocol` error path can only be exercised on a fresh
//! `OnceLock`, which would require a separate test binary; we leave
//! that uncovered here and document the gap.

#![cfg(feature = "telemetry")]

use cognee_observability::{SettingsView, already_instrumented, init_telemetry};
use serial_test::serial;
use tracing_subscriber::Registry;

struct StaticSettings {
    tracing_enabled: bool,
    service_name: String,
    otlp_endpoint: String,
    otlp_headers: String,
    otlp_protocol: String,
    span_processor: String,
    traces_sampler: String,
    traces_sampler_arg: String,
}

impl StaticSettings {
    fn new(tracing_enabled: bool, endpoint: &str) -> Self {
        Self {
            tracing_enabled,
            service_name: "cognee-test".to_string(),
            otlp_endpoint: endpoint.to_string(),
            otlp_headers: String::new(),
            otlp_protocol: "grpc".to_string(),
            span_processor: "batch".to_string(),
            traces_sampler: String::new(),
            traces_sampler_arg: String::new(),
        }
    }

    fn with_protocol(mut self, protocol: &str) -> Self {
        self.otlp_protocol = protocol.to_string();
        self
    }
}

impl SettingsView for StaticSettings {
    fn tracing_enabled(&self) -> bool {
        self.tracing_enabled
    }
    fn service_name(&self) -> &str {
        &self.service_name
    }
    fn otlp_endpoint(&self) -> &str {
        &self.otlp_endpoint
    }
    fn otlp_headers(&self) -> &str {
        &self.otlp_headers
    }
    fn otlp_protocol(&self) -> &str {
        &self.otlp_protocol
    }
    fn span_processor(&self) -> &str {
        &self.span_processor
    }
    fn traces_sampler(&self) -> &str {
        &self.traces_sampler
    }
    fn traces_sampler_arg(&self) -> &str {
        &self.traces_sampler_arg
    }
}

/// Single combined activation-path test — see the module docstring for
/// rationale. Splitting into multiple `#[test]` functions would make
/// ordering load-bearing, since the first successful `init_telemetry`
/// call in this binary installs a provider that all subsequent calls
/// bridge to via the `OUR_PROVIDER_INSTALLED` `OnceLock`.
#[tokio::test]
#[serial]
async fn init_telemetry_full_activation_lifecycle() {
    // 1. Default state on a fresh process: nothing installed.
    assert!(
        !already_instrumented(),
        "OUR_PROVIDER_INSTALLED must default to unset on a fresh process"
    );

    // 2. First successful init installs our SDK provider.
    let settings = StaticSettings::new(true, "http://127.0.0.1:1");
    let (_layer, guard) = init_telemetry::<Registry>(&settings)
        .expect("first init_telemetry call must succeed and install our SDK provider");

    assert!(
        guard.has_provider(),
        "the first installation must return a guard that owns the SDK provider"
    );
    assert!(
        already_instrumented(),
        "OUR_PROVIDER_INSTALLED must be set after a successful init_telemetry call"
    );

    // 3a. Second call with the same settings is idempotent — bridge.
    let (_layer2, guard2) = init_telemetry::<Registry>(&settings)
        .expect("second init_telemetry call must succeed via the bridge branch");
    assert!(
        !guard2.has_provider(),
        "second init_telemetry call must return a noop guard (bridge mode)"
    );

    // 3b. Endpoint-only activation flag (`tracing_enabled=false`,
    // endpoint set) still bridges, returns Ok.
    let endpoint_only = StaticSettings::new(false, "http://127.0.0.1:1");
    let (_layer3, guard3) = init_telemetry::<Registry>(&endpoint_only)
        .expect("endpoint-only activation bridges to the installed provider");
    assert!(
        !guard3.has_provider(),
        "bridge branch returns TelemetryGuard::noop"
    );

    // 3c. Flag-only with no endpoint: bridges (the bridge branch never
    // touches the endpoint). On a fresh `OnceLock` this would instead
    // try to construct an OTLP exporter against the SDK default
    // endpoint (`http://localhost:4317`); we don't cover that here.
    let flag_only = StaticSettings::new(true, "");
    let (_layer4, guard4) = init_telemetry::<Registry>(&flag_only)
        .expect("flag-only activation bridges to the installed provider");
    assert!(
        !guard4.has_provider(),
        "bridge branch returns TelemetryGuard::noop"
    );

    // 3d. Unknown protocol: the protocol is read only inside
    // `build_exporter`, which the bridge branch never reaches. So this
    // succeeds. The `UnknownProtocol` error path requires a fresh
    // `OnceLock` (separate test binary) and is not covered here.
    let bad_protocol = StaticSettings::new(true, "http://127.0.0.1:1").with_protocol("nonsense");
    let (_layer5, guard5) = init_telemetry::<Registry>(&bad_protocol)
        .expect("bridge branch ignores the configured protocol");
    assert!(
        !guard5.has_provider(),
        "bridge branch returns TelemetryGuard::noop regardless of protocol"
    );
}
