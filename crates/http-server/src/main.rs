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
use cognee_http_server::{AppState, HttpServerConfig, wiring};
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

    let logging_cfg = match cognee_logging::LoggingConfig::from_env() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("warning: invalid logging env var: {err}; falling back to defaults");
            cognee_logging::LoggingConfig::defaults()
        }
    };

    let span_buffer_layer: cognee_logging::BoxedLayer =
        Box::new(SpanBufferLayer::new((*spans).clone()));

    #[cfg(not(feature = "telemetry"))]
    let _log_guards = cognee_logging::init_logging(logging_cfg, std::iter::once(span_buffer_layer));

    #[cfg(feature = "telemetry")]
    let (_log_guards, telemetry_guard) = {
        use tracing_subscriber::Registry;
        use tracing_subscriber::layer::Identity;

        let settings = cognee_observability::EnvSettingsView::from_env();
        let (telemetry_layer, telemetry_guard) =
            match cognee_observability::init_telemetry::<Registry>(&settings) {
                Ok(pair) => pair,
                Err(err) => {
                    eprintln!("warning: failed to initialise OTEL telemetry: {err}");
                    (
                        Box::new(Identity::new())
                            as cognee_observability::BoxedTelemetryLayer<Registry>,
                        cognee_observability::TelemetryGuard::noop(),
                    )
                }
            };

        let extras: Vec<cognee_logging::BoxedLayer> = vec![telemetry_layer, span_buffer_layer];
        let guards = cognee_logging::init_logging(logging_cfg, extras);
        (guards, Some(Arc::new(telemetry_guard)))
    };

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

    let handles = if cfg.disable_default_backends {
        tracing::warn!(
            "COGNEE_DISABLE_DEFAULT_BACKENDS is enabled; skipping default backend wiring"
        );
        None
    } else {
        Some(
            wiring::wire_default_backends(&cfg)
                .await
                .context("failed to wire default backend handles")?,
        )
    };

    let mut state = match handles {
        Some(handles) => {
            let mut state = AppState::build_with_db(cfg.clone(), handles.database.clone())
                .await
                .context("failed to build AppState with database")?;
            state.lib = Some(Arc::new(handles));
            state.install_real_health_checker();
            state
        }
        None => AppState::build(cfg.clone())
            .await
            .context("failed to build AppState")?,
    };
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
