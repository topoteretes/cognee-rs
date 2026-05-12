//! Application state — a single `Clone`-able struct injected into every handler
//! via `axum::extract::State`.
//!
//! All fields are `Arc<…>` so `AppState::clone()` is cheap.  Axum clones the
//! state once per request.

use std::sync::Arc;

#[cfg(feature = "telemetry")]
use cognee_observability::TelemetryGuard;

use cognee_core::PipelineRunRegistry;
use cognee_core::pipeline_run_registry::DefaultPipelineRunRegistry;
use cognee_database::{DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository};

use crate::{
    auth::{AuthContext, Mailer},
    components::ComponentHandles,
    config::{HttpServerConfig, RegistryConfig},
    error::ServerError,
    observability::{BufferConfig, SpanBuffer},
    sync::SyncRegistry,
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
    ///
    /// Wired to `DefaultPipelineRunRegistry` in P3.  After P3, callers may
    /// rely on this being set — it is no longer optional.
    ///
    /// The inner `Arc<dyn PipelineRunRegistry>` is `Clone`-able cheaply.
    pub pipelines: Arc<dyn PipelineRunRegistry>,

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

    /// In-memory span buffer feeding `GET /api/v1/activity/spans`.
    /// Always populated — `BufferConfig::from_env()` reads the cap. To
    /// effectively disable the buffer pass `BufferConfig { max_traces: 0, .. }`.
    pub spans: Arc<SpanBuffer>,

    /// In-memory registry tracking one running cloud sync per user. Always
    /// populated; the registry itself starts empty.
    pub sync: Arc<SyncRegistry>,

    /// Flush-on-drop guard for the OpenTelemetry exporter (decision 9).
    /// Held only for its `Drop` side effect: the last `Arc` released calls
    /// `provider.force_flush()` + `provider.shutdown()`. `None` when built
    /// without explicit telemetry init (test paths, library embedders).
    #[cfg(feature = "telemetry")]
    pub telemetry_guard: Option<Arc<TelemetryGuard>>,
}

impl AppState {
    /// Build a no-op `Arc<dyn PipelineRunRegistry>` backed by a
    /// `NoOpPipelineRunRepository`.  Useful in tests that construct `AppState`
    /// directly without a real database.
    pub fn noop_pipelines() -> Arc<dyn PipelineRunRegistry> {
        let repo = Arc::new(NoOpPipelineRunRepository) as Arc<dyn PipelineRunRepository>;
        let cfg = RegistryConfig::default();
        DefaultPipelineRunRegistry::new(repo, cfg)
    }

    /// Construct an `AppState` with the given config; all optional components
    /// default to `None`.  Later phases call this and then set individual fields.
    ///
    /// Builds `DefaultPipelineRunRegistry` from the config's registry knobs and
    /// runs the startup orphan-reset per pipelines.md §12 — any `INITIATED` /
    /// `STARTED` rows left over from a previous unclean shutdown are rewritten to
    /// `ERRORED` with `reason = "server_restart_orphan"`.
    pub async fn build(config: HttpServerConfig) -> Result<Self, ServerError> {
        // Build an in-memory-only pipeline run repository backed by a temporary
        // SQLite database.  The real repository (backed by the server's own DB)
        // is wired when `lib` is populated.  For now we use a NoOp repo so the
        // registry is always non-None.
        let repo = Arc::new(NoOpPipelineRunRepository) as Arc<dyn PipelineRunRepository>;
        let registry_cfg = config.to_registry_config();
        let pipelines: Arc<dyn PipelineRunRegistry> =
            DefaultPipelineRunRegistry::new(repo, registry_cfg);

        Ok(Self {
            config: Arc::new(config),
            pipelines,
            lib: None,
            auth: None,
            mailer: Arc::new(crate::auth::LoggingMailer),
            health: None,
            spans: Arc::new(SpanBuffer::new(BufferConfig::from_env())),
            sync: Arc::new(SyncRegistry::new()),
            #[cfg(feature = "telemetry")]
            telemetry_guard: None,
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

// ─── NoOpPipelineRunRepository ────────────────────────────────────────────────

/// No-op repository used when no real DB is wired (P0/P1 test helpers).
///
/// All writes are silently discarded; reads return empty results.
struct NoOpPipelineRunRepository;

#[async_trait::async_trait]
impl PipelineRunRepository for NoOpPipelineRunRepository {
    async fn log_pipeline_run(
        &self,
        pipeline_run_id: uuid::Uuid,
        _pipeline_id: uuid::Uuid,
        _pipeline_name: &str,
        _dataset_id: Option<uuid::Uuid>,
        _status: cognee_database::PipelineRunStatus,
        _run_info: Option<serde_json::Value>,
    ) -> Result<uuid::Uuid, cognee_database::DatabaseError> {
        Ok(pipeline_run_id)
    }

    async fn latest_status(
        &self,
        _dataset_ids: &[uuid::Uuid],
        _pipeline_name: &str,
    ) -> Result<
        std::collections::HashMap<uuid::Uuid, cognee_database::PipelineRunStatus>,
        cognee_database::DatabaseError,
    > {
        Ok(std::collections::HashMap::new())
    }

    async fn list_recent(
        &self,
        _dataset_id: Option<uuid::Uuid>,
        _limit: u32,
    ) -> Result<Vec<cognee_database::PipelineRun>, cognee_database::DatabaseError> {
        Ok(Vec::new())
    }

    async fn reset_orphans(&self, _reason: &str) -> Result<u64, cognee_database::DatabaseError> {
        Ok(0)
    }

    async fn set_payload_field(
        &self,
        _run_id: uuid::Uuid,
        _key: &str,
        _value: serde_json::Value,
    ) -> Result<(), cognee_database::DatabaseError> {
        Ok(())
    }

    async fn get_payload(
        &self,
        _run_id: uuid::Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, cognee_database::DatabaseError> {
        Ok(serde_json::Map::new())
    }

    async fn list_pipeline_names_for_dataset(
        &self,
        _dataset_id: uuid::Uuid,
    ) -> Result<Vec<(String, cognee_database::PipelineRunStatus)>, cognee_database::DatabaseError>
    {
        Ok(Vec::new())
    }
}

// ─── Build state with a real database ─────────────────────────────────────────

impl AppState {
    /// Build `AppState` with a real `DatabaseConnection` wired into the pipeline
    /// registry.  Used by the server startup path when backend env vars are
    /// present.
    ///
    /// Runs the orphan-reset once on startup per pipelines.md §12.
    pub async fn build_with_db(
        config: HttpServerConfig,
        db: Arc<DatabaseConnection>,
    ) -> Result<Self, ServerError> {
        let repo = Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)))
            as Arc<dyn PipelineRunRepository>;
        let registry_cfg = config.to_registry_config();

        // Run orphan reset on startup (best-effort — non-fatal).
        let pipelines: Arc<dyn PipelineRunRegistry> =
            match DefaultPipelineRunRegistry::new_with_orphan_reset(repo, registry_cfg).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "pipeline registry startup orphan-reset failed (non-fatal): {e}"
                    );
                    // Fall back to plain new() without reset.
                    let repo2 = Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)))
                        as Arc<dyn PipelineRunRepository>;
                    DefaultPipelineRunRegistry::new(repo2, config.to_registry_config())
                }
            };

        Ok(Self {
            config: Arc::new(config),
            pipelines,
            lib: None,
            auth: None,
            mailer: Arc::new(crate::auth::LoggingMailer),
            health: None,
            spans: Arc::new(SpanBuffer::new(BufferConfig::from_env())),
            sync: Arc::new(SyncRegistry::new()),
            #[cfg(feature = "telemetry")]
            telemetry_guard: None,
        })
    }
}
