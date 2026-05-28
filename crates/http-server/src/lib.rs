//! Cognee HTTP server library.
//!
//! Provides `build_router` (assembles the `axum::Router` with all middleware and
//! sub-routers) and `run` (binds a TCP listener and drives `axum::serve`).
//!
//! The standalone `cognee-http-server` binary is a thin shell over these two
//! functions.  Library embedders can call `build_router` directly and host the
//! returned `Router` in their own runtime.

pub mod auth;
pub mod components;
pub mod config;
pub mod dto;
pub mod error;
pub mod lifecycle;
pub mod middleware;
pub mod multipart;
pub mod notebook_runner;
pub mod observability;
pub mod openapi;
pub mod permissions;
pub mod pipelines;
pub mod responses;
pub mod responses_dispatch;
pub mod routers;
pub mod state;
pub mod sync;

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
        // Auth router family (login / logout / auth-me / register / reset / verify)
        .nest(
            "/api/v1/auth",
            Router::new()
                .merge(routers::auth::router())
                .merge(routers::auth_register::router())
                .merge(routers::auth_reset_password::router())
                .merge(routers::auth_verify::router()),
        )
        // API keys router (mounted at /api/v1/auth/api-keys)
        .nest("/api/v1/auth/api-keys", routers::api_keys::router())
        // Users router (me / by-id CRUD + get-user-id)
        .nest(
            "/api/v1/users",
            Router::new()
                .merge(routers::users::router())
                .merge(routers::users_by_email::router()),
        )
        // P2 write-path routers
        .nest("/api/v1/add", routers::add::router())
        .nest("/api/v1/datasets", routers::datasets::router())
        .nest("/api/v1/ontologies", routers::ontologies::router())
        .nest("/api/v1/delete", routers::delete::router())
        .nest("/api/v1/update", routers::update::router())
        .nest("/api/v1/forget", routers::forget::router())
        // P3 pipeline routers
        .nest("/api/v1/cognify", routers::cognify::router())
        .nest("/api/v1/memify", routers::memify::router())
        .nest("/api/v1/remember", routers::remember::router())
        .nest("/api/v1/improve", routers::improve::router())
        // P4 read-path routers
        .nest("/api/v1/search", routers::search::router())
        .nest("/api/v1/recall", routers::recall::router())
        .nest("/api/v1/sessions", routers::sessions::router())
        .nest("/api/v1/llm", routers::llm::router())
        .nest("/api/v1/visualize", routers::visualize::router())
        // P5 admin routers
        .nest("/api/v1/permissions", routers::permissions::router())
        .nest("/api/v1/settings", routers::settings::router())
        .nest("/api/v1/configuration", routers::configuration::router())
        // P6 observability + cloud-sync + cloud-checks
        .nest("/api/v1/activity", routers::activity::router())
        .nest("/api/v1/sync", routers::sync::router())
        .nest("/api/v1/checks", routers::checks::router())
        // P7 notebooks + responses (responses is a 501 stub in Stage A)
        .nest("/api/v1/notebooks", routers::notebooks::router())
        .nest("/api/v1/responses", routers::responses::router())
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
