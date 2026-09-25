//! Embedder-facing router assembly.
//!
//! `RouterBuilder` is the injection seam closed embedders use to splice
//! the moved auth/cloud/permissions routers back onto the OSS surface and
//! to install an `AuthResolver` / `ExtraAuthValidator` against the
//! `AuthenticatedUser` extractor.
//!
//! Pure-OSS callers use the legacy free function [`build_router`] which is
//! equivalent to `RouterBuilder::new(state).build()`.

use std::sync::Arc;

use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::get};
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;

use crate::auth_resolver::{AuthResolver, ExtraAuthValidator, resolver_from_validator};
use crate::error::ServerError;
use crate::lifecycle;
use crate::middleware;
use crate::openapi;
use crate::routers;
use crate::state::AppState;

// ─── Root handler ─────────────────────────────────────────────────────────────

/// `GET /` — lightweight root endpoint used as a k8s liveness probe.
async fn root() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({"message": "Hello, World, I am alive!"})),
    )
}

// ─── RouterBuilder ────────────────────────────────────────────────────────────

/// Builder for the full `axum::Router`. Closed embedders use this to
/// inject extra routers (e.g. the auth/permissions families) and to
/// install an `AuthResolver` against the `AuthenticatedUser` extractor.
pub struct RouterBuilder {
    state: AppState,
    extra_routers: Vec<(&'static str, Router<AppState>)>,
    extra_validator: Option<Arc<dyn ExtraAuthValidator>>,
    auth_resolver: Option<Arc<dyn AuthResolver>>,
}

impl RouterBuilder {
    /// Create a builder from the given pre-built state.
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            extra_routers: Vec::new(),
            extra_validator: None,
            auth_resolver: None,
        }
    }

    /// Splice an additional router mounted under `mount` (e.g.
    /// `/api/v1/auth`). Routers are nested in insertion order.
    pub fn with_router(mut self, mount: &'static str, r: Router<AppState>) -> Self {
        self.extra_routers.push((mount, r));
        self
    }

    /// Install a narrow Auth0 / OIDC validator hook. Wrapped in a
    /// resolver that performs only the validator step.
    ///
    /// If both `with_auth_resolver` and `with_extra_validator` are set,
    /// the explicit resolver wins.
    pub fn with_extra_validator(mut self, v: Arc<dyn ExtraAuthValidator>) -> Self {
        self.extra_validator = Some(v);
        self
    }

    /// Install a full authentication chain (Bearer → cookie → API key
    /// → optional external validator).
    pub fn with_auth_resolver(mut self, r: Arc<dyn AuthResolver>) -> Self {
        self.auth_resolver = Some(r);
        self
    }

    /// Assemble the router, applying middleware and running the OSS
    /// startup hook.
    pub async fn build(mut self) -> Result<Router, ServerError> {
        if let Some(r) = self.auth_resolver.take() {
            self.state.auth_resolver = Some(r);
        } else if let Some(v) = self.extra_validator.take() {
            self.state.auth_resolver = Some(resolver_from_validator(v));
        }

        lifecycle::on_startup(&self.state).await?;

        let body_limit = self.state.config.body_limit;

        let mut app = Router::new()
            // Root endpoint
            .route("/", get(root))
            // Health router (mounted at /health, not /api/v1/health)
            .nest("/health", routers::health::router())
            // OpenAPI document
            .route("/openapi.json", get(openapi::openapi_json))
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
            // P5 admin routers (settings stays OSS; configuration +
            // permissions move closed — both consume types relocated to
            // cognee-access-control).
            .nest("/api/v1/settings", routers::settings::router())
            // P6 observability
            .nest("/api/v1/activity", routers::activity::router())
            // P7 notebooks + responses
            .nest("/api/v1/notebooks", routers::notebooks::router())
            .nest("/api/v1/responses", routers::responses::router());

        // Splice in any embedder-supplied routers.
        for (mount, r) in self.extra_routers.drain(..) {
            app = app.nest(mount, r);
        }

        let app = app
            // Middleware stack (outer → inner): trace → CORS → body limit
            .layer(RequestBodyLimitLayer::new(body_limit))
            .layer(middleware::cors::cors_layer(&self.state.config))
            .layer(middleware::tracing::trace_layer())
            .with_state(self.state);

        Ok(app)
    }
}

/// Assemble the full `axum::Router` with no extra routers — equivalent to
/// `RouterBuilder::new(state).build()`.
///
/// Pure-OSS embedders that don't need the closed auth/permissions surface
/// call this directly.
pub async fn build_router(state: AppState) -> Result<Router, ServerError> {
    RouterBuilder::new(state).build().await
}
