//! Application state — a single `Clone`-able struct injected into every handler
//! via `axum::extract::State`.
//!
//! All fields are `Arc<…>` so `AppState::clone()` is cheap.  Axum clones the
//! state once per request.

use std::sync::Arc;

use cognee_core::PipelineRunRegistry;

use crate::{config::HttpServerConfig, error::ServerError};

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

    /// The `cognee-lib` facade (add/cognify/search/delete/…).
    /// `None` in P0 — wired when `cognee_lib::ComponentManager` integration lands.
    // TODO(P1): wire Arc<ComponentManager> / CogneeLib facade here
    pub lib: Option<Arc<()>>,

    /// JWT + cookie config and user repository.
    /// `None` in P0 — wired in P2 (auth phase).
    // TODO(P2): wire Arc<AuthContext> here
    pub auth: Option<Arc<()>>,

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
            health: None,
            spans: None,
            sync: None,
        })
    }
}
