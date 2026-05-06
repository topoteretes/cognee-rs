//! Standalone `cognee-http-server` binary entry point.
//!
//! Compiled only when the `bin` feature is enabled.  The binary is a thin shell:
//! parse args → build config → build state → call `cognee_http_server::run`.
//!
//! Usage:
//!   cognee-http-server [--host 0.0.0.0] [--port 8000] [--env prod]
//!                      [--cors-allowed-origins "http://a.test,http://b.test"]
//!   Every flag falls back to its env var so containerized deployments work
//!   without flags.

use std::net::SocketAddr;

use anyhow::Context as _;
use clap::Parser;
use cognee_http_server::observability::{BufferConfig, SpanBuffer, SpanBufferLayer};
use cognee_http_server::{AppState, HttpServerConfig};
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "cognee-http-server",
    about = "Cognee HTTP server (FastAPI-compatible)",
    version
)]
struct Args {
    /// Bind host. Env: HTTP_API_HOST.
    #[arg(long, env = "HTTP_API_HOST", default_value = "0.0.0.0")]
    host: String,

    /// Bind port. Env: HTTP_API_PORT.
    #[arg(long, env = "HTTP_API_PORT", default_value_t = 8000)]
    port: u16,

    /// Path to a JSON config file (optional; env vars override).
    #[arg(long, env = "COGNEE_HTTP_CONFIG")]
    config: Option<std::path::PathBuf>,

    /// Comma-separated CORS allowed origins. Env: CORS_ALLOWED_ORIGINS.
    #[arg(long, env = "CORS_ALLOWED_ORIGINS")]
    cors_allowed_origins: Option<String>,

    /// Deployment environment (dev|prod|test). Env: ENV.
    #[arg(long, env = "ENV", default_value = "prod")]
    env: String,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let _ = dotenv::dotenv();

    let spans = Arc::new(SpanBuffer::new(BufferConfig::from_env()));

    // Decision 11: read OTEL settings from env *before* installing the
    // subscriber so the OTEL bridge layer can be composed in one shot.
    #[cfg(feature = "telemetry")]
    let telemetry_guard = {
        let settings = cognee_observability::EnvSettingsView::from_env();
        init_tracing(&settings, spans.clone())
    };
    #[cfg(not(feature = "telemetry"))]
    init_tracing(spans.clone());

    let args = Args::parse();

    let mut cfg = HttpServerConfig::from_env().context("failed to load config from environment")?;
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

    let mut state = AppState::build(cfg.clone())
        .await
        .context("failed to build AppState")?;
    state.spans = spans;
    #[cfg(feature = "telemetry")]
    {
        state.telemetry_guard = telemetry_guard;
    }

    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .context("invalid bind address")?;

    cognee_http_server::run(addr, state)
        .await
        .context("server error")?;

    Ok(())
}

/// Build the layered subscriber:
///
/// `tracing-opentelemetry::layer` → `EnvFilter` → `fmt::layer (stdout)` →
/// `SpanBufferLayer`.
///
/// The OTEL layer must sit directly above `Registry` because the boxed
/// `Layer<Registry>` returned by `init_telemetry::<Registry>` does not
/// satisfy `Layer<Layered<...>>` for nested subscriber types.
#[cfg(feature = "telemetry")]
fn init_tracing(
    settings: &cognee_observability::EnvSettingsView,
    spans: Arc<SpanBuffer>,
) -> Option<Arc<cognee_observability::TelemetryGuard>> {
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let span_buffer_layer = SpanBufferLayer::new((*spans).clone());

    let (telemetry_layer, telemetry_guard) =
        match cognee_observability::init_telemetry::<Registry>(settings) {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(?err, "telemetry init failed; continuing without OTEL");
                (
                    Box::new(tracing_subscriber::layer::Identity::new())
                        as cognee_observability::BoxedTelemetryLayer<Registry>,
                    cognee_observability::TelemetryGuard::noop(),
                )
            }
        };

    // Tests may install a subscriber first; treat install failure as soft.
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

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let span_buffer_layer = SpanBufferLayer::new((*spans).clone());

    let _ = Registry::default()
        .with(env_filter)
        .with(fmt_layer)
        .with(span_buffer_layer)
        .try_init();
}
