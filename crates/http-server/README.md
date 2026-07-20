# Cognee HTTP Server

HTTP API server for Cognee-Rust, built on [`axum`]. Exposes the cognee pipeline
(`add` / `cognify` / `search`, auth, datasets, responses, etc.) over HTTP,
mirroring the Python `cognee` API.

The crate ships both a **library** (embed the router in your own runtime) and a
standalone **binary** (`cognee-http-server`).

## Library usage

`build_router` assembles the full `axum::Router` (all middleware and
sub-routers) and runs startup wiring; `run` binds a TCP listener and drives
`axum::serve`. Library embedders can call `build_router` directly and host the
returned `Router` in their own runtime.

```rust
use std::net::SocketAddr;
use cognee_http_server::{build_router, run, AppState};

async fn serve(state: AppState) -> Result<(), Box<dyn std::error::Error>> {
    // Either host the router yourself…
    let _router = build_router(state.clone()).await?;

    // …or let `run` bind and serve.
    let addr: SocketAddr = "0.0.0.0:8000".parse()?;
    run(addr, state).await?;
    Ok(())
}
```

Public surface: `build_router`, `run`, `AppState`, `HttpServerConfig`,
`ApiError`, `ServerError`.

## Running the binary

The standalone binary requires the `bin` feature (it pulls in `clap`, `dotenv`,
and the multi-thread tokio runtime):

```bash
cargo run -p cognee-http-server --features bin
```

### Configuration

Arguments can be supplied as flags or via environment variables:

| Flag | Env var | Default | Purpose |
|---|---|---|---|
| `--host` | `HTTP_API_HOST` | `0.0.0.0` | Bind host |
| `--port` | `HTTP_API_PORT` | `8000` | Bind port |
| `--cors-allowed-origins` | `CORS_ALLOWED_ORIGINS` | — | Comma-separated CORS origins |
| `--env` | `ENV` | `prod` | Deployment environment (`dev`/`prod`/`test`) |

A `.env` file at startup is loaded automatically. Logging is configured via the
`cognee-logging` env vars (`COGNEE_LOG_*`, `LOG_FILE_NAME`, `LOG_LEVEL`,
`RUST_LOG`); the `/spans` endpoint is backed by an in-memory span buffer.

## Features

- `telemetry` (default) — compiles OpenTelemetry/product-telemetry capability.
  Product analytics remain runtime-disabled until explicit opt-in; use
  `--no-default-features` to remove the capability at compile time.
- `bin` — builds the standalone `cognee-http-server` binary and its
  argument-parsing dependencies.

[`axum`]: https://github.com/tokio-rs/axum
