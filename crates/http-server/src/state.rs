//! Application state — a single `Clone`-able struct injected into every handler
//! via `axum::extract::State`.
//!
//! All fields are `Arc<…>` so `AppState::clone()` is cheap.  Axum clones the
//! state once per request.

use std::sync::Arc;

use cognee_core::PipelineRunRegistry;

use crate::{
    auth::{AuthContext, Mailer},
    components::ComponentHandles,
    config::HttpServerConfig,
    error::ServerError,
};

// ─── AppState ────────────────────────────────────────────────────────────────

/// Per-server dependency container shared across all handlers.
///
/// Fields that depend on phases beyond P0 are either `Option<Arc<…>>` (landed
/// later) or `()` placeholders annotated with the landing phase.
#[derive(Clone)]
pub struct AppState {
    /// HTTP server config (host, port, CORS, JWT, …).
    pub config: Arc<HttpServerConfig>,

    /// Background pipeline-run lifecycle registry.
    /// `None` in P0 — wired to `DefaultPipelineRunRegistry` in P3.
    // TODO(P3): wire concrete DefaultPipelineRunRegistry here
    pub pipelines: Option<Arc<dyn PipelineRunRegistry>>,

    /// Pre-built component handles (database, storage, delete_service,
    /// ontology_manager). `None` until `AppState::build` fully initialises
    /// the backends — most tests leave this `None` and stub out the
    /// relevant functionality directly.
    // P2: wired by AppState::build when storage/DB env vars are available.
    pub lib: Option<Arc<ComponentHandles>>,

    /// JWT + cookie config and user repository.
    /// Wired in P1 step 2.
    pub auth: Option<Arc<AuthContext>>,

    /// Email delivery abstraction. Defaults to `LoggingMailer` (P1).
    /// SMTP impl deferred to P7.
    pub mailer: Arc<dyn Mailer>,

    /// Health checker for /health endpoints.
    /// `None` in P0 — wired when cognee_lib::health lands.
    // TODO(P1): wire Arc<dyn cognee_lib::health::HealthChecker> here
    pub health: Option<Arc<dyn crate::routers::health::HealthChecker>>,

    /// In-memory OTEL-style span buffer for /api/v1/activity/spans.
    /// `None` in P0 — wired in the observability phase.
    // TODO(P6): wire Arc<SpanBuffer> here
    pub spans: Option<Arc<()>>,

    /// Sync registry for /api/v1/sync endpoints.
    /// `None` in P0 — wired when the sync router lands.
    // TODO(P7): wire Arc<SyncRegistry> here
    pub sync: Option<Arc<()>>,
}

impl AppState {
    /// Construct an `AppState` with the given config; all optional components
    /// default to `None`.  Later phases call this and then set individual fields.
    pub async fn build(config: HttpServerConfig) -> Result<Self, ServerError> {
        Ok(Self {
            config: Arc::new(config),
            pipelines: None,
            lib: None,
            auth: None,
            mailer: Arc::new(crate::auth::LoggingMailer),
            health: None,
            spans: None,
            sync: None,
        })
    }

    /// Convenience accessor for the component handles.
    ///
    /// Returns `None` when the server is running in test mode without backends
    /// wired. Most integration tests build their own `ComponentHandles` directly.
    pub fn components(&self) -> Option<&ComponentHandles> {
        self.lib.as_deref()
    }
}
