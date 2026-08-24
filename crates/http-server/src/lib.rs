//! Cognee HTTP server library — OSS surface.
//!
//! Provides `RouterBuilder` (the embedder-facing injection seam) and a
//! free-function `build_router` shorthand. Closed embedders use
//! `RouterBuilder` to splice the moved auth / api-keys / users / sync /
//! checks / permissions routers back onto the OSS surface and to install
//! an `AuthResolver` / `ExtraAuthValidator` against the
//! `AuthenticatedUser` extractor.
//!
//! Pure-OSS callers (and tests) use `build_router(state)` which is
//! equivalent to `RouterBuilder::new(state).build()`.
//!
//! The standalone `cognee-http-server` binary is a thin shell over
//! `build_router` + `axum::serve`.

pub mod auth;
pub mod auth_resolver;
pub mod cloud_client;
pub mod components;
pub mod config;
pub mod dto;
pub mod error;
pub mod health;
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
mod router_builder;
pub mod routers;
pub mod state;
pub mod sync;
pub mod telemetry;
pub mod wiring;

pub use config::HttpServerConfig;
pub use error::{ApiError, ServerError};
pub use router_builder::{RouterBuilder, build_router};
pub use state::AppState;

use std::net::SocketAddr;

// ─── Graceful shutdown signal ─────────────────────────────────────────────────

/// Waits for SIGTERM or SIGINT (Ctrl-C).
///
/// **Signal only.** The teardown deliberately does *not* live here: axum starts
/// draining in-flight requests only once this future **completes**, so anything
/// done inside it runs while the listener is still live and handlers are still
/// executing. Closing the stores here meant an in-flight request could hit
/// `graph database is closed` and return 500 for a shutdown that was supposed to
/// be graceful. The teardown belongs after `serve(...)` returns — see
/// [`serve_with_shutdown`].
///
/// Only compiled when the `bin` feature is enabled so the library does not
/// require `tokio/signal`.
#[cfg(feature = "bin")]
async fn shutdown_signal() {
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
}

/// Serve until `shutdown` resolves, let axum drain the in-flight requests, and
/// **then** release the server's resources.
///
/// The ordering is the point. `with_graceful_shutdown` treats the future it is
/// given as "stop accepting" — draining begins when that future completes — so a
/// teardown placed inside it runs *before* the drain, against handlers that are
/// still running. Running it after `serve(...)` returns is what makes the drain
/// and the teardown sequential: every handler has finished by then, so nothing can
/// observe a closed store.
///
/// The teardown runs whether or not `serve` succeeded: an accept-loop error still
/// leaves the pools and the embedded graph open, and dropping them is not closing
/// them (topoteretes/cognee-rs#132).
///
/// Taking the shutdown trigger as a parameter is also what makes this testable
/// without raising a process signal — the binary passes [`shutdown_signal`], a
/// test passes a channel.
pub async fn serve_with_shutdown<F>(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    shutdown: F,
    state: AppState,
) -> Result<(), ServerError>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await;

    // Draining is complete here — or the accept loop failed. Either way the
    // resources are still open, so tear down before propagating.
    lifecycle::on_shutdown(&state).await;

    served.map_err(|e| ServerError::Other(anyhow::anyhow!(e)))
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
        serve_with_shutdown(listener, app, shutdown_signal(), state).await?;
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
