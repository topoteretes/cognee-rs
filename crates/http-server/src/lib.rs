//! Cognee HTTP server library.
//!
//! Provides `build_router` (assembles the `axum::Router` with all middleware and
//! sub-routers) and `run` (binds a TCP listener and drives `axum::serve`).
//!
//! The standalone `cognee-http-server` binary is a thin shell over these two
//! functions.  Library embedders can call `build_router` directly and host the
//! returned `Router` in their own runtime.

pub mod config;
pub mod error;
pub mod lifecycle;
pub mod middleware;
pub mod openapi;
pub mod routers;
pub mod state;

pub use config::HttpServerConfig;
pub use error::{ApiError, ServerError};
pub use state::AppState;

use std::net::SocketAddr;

use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;

// ─── Root handler ─────────────────────────────────────────────────────────────

/// `GET /` — lightweight root endpoint used as a k8s liveness probe.
///
/// Python equivalent: the `root` handler in `client.py`.
async fn root() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({"message": "Hello, World, I am alive!"})),
    )
}

// ─── build_router ─────────────────────────────────────────────────────────────

/// Assemble the full `axum::Router`, apply middleware, and call `on_startup`.
///
/// Called by `run()` and by tests that need the router without a bound socket.
pub async fn build_router(state: AppState) -> Result<Router, ServerError> {
    lifecycle::on_startup(&state).await?;

    let body_limit = state.config.body_limit;

    let app = Router::new()
        // Root endpoint
        .route("/", get(root))
        // Health router (mounted at /health, not /api/v1/health)
        .nest("/health", routers::health::router())
        // OpenAPI document
        .route("/openapi.json", get(openapi::openapi_json))
        // Middleware stack (outer → inner): trace → CORS → body limit
        .layer(RequestBodyLimitLayer::new(body_limit))
        .layer(middleware::cors::cors_layer(&state.config))
        .layer(middleware::tracing::trace_layer())
        .with_state(state);

    Ok(app)
}

// ─── Graceful shutdown signal ─────────────────────────────────────────────────

/// Waits for SIGTERM or SIGINT (Ctrl-C).
///
/// Only compiled when the `bin` feature is enabled so the library does not
/// require `tokio/signal`.
#[cfg(feature = "bin")]
async fn shutdown_signal(state: AppState) {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    lifecycle::on_shutdown(&state).await;
}

// ─── run ──────────────────────────────────────────────────────────────────────

/// Bind `addr`, build the router, and serve until a shutdown signal.
///
/// This is the main entry point for both the standalone binary and embedders
/// that want a ready-made server loop.
pub async fn run(addr: SocketAddr, state: AppState) -> Result<(), ServerError> {
    let app = build_router(state.clone()).await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("listening on {addr}");

    #[cfg(feature = "bin")]
    {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(state))
            .await
            .map_err(|e| ServerError::Other(anyhow::anyhow!(e)))?;
    }

    #[cfg(not(feature = "bin"))]
    {
        // Library consumers that call `run()` without the `bin` feature get a
        // server without graceful shutdown — they manage termination themselves.
        axum::serve(listener, app)
            .await
            .map_err(|e| ServerError::Other(anyhow::anyhow!(e)))?;
    }

    Ok(())
}
