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
use cognee_http_server::{AppState, HttpServerConfig};

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
    // 1. Load .env from the current directory (binary-only concern).
    let _ = dotenv::dotenv();

    // 2. Install the tracing subscriber (the library does NOT do this).
    init_tracing();

    // 3. Parse CLI args.
    let args = Args::parse();

    // 4. Build config: start from env, then overlay CLI flags.
    let mut cfg = HttpServerConfig::from_env().context("failed to load config from environment")?;
    // CLI flags override env vars (highest precedence).
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

    // 5. Build application state.
    let state = AppState::build(cfg.clone())
        .await
        .context("failed to build AppState")?;

    // 6. Bind and serve.
    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port)
        .parse()
        .context("invalid bind address")?;

    cognee_http_server::run(addr, state)
        .await
        .context("server error")?;

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));

    // Ignore the error — a subscriber may already be installed in tests.
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}
