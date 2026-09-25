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
use cognee_database::{
    DatabaseConnection, NoopPipelineRunRepository, PipelineRunRepository,
    SeaOrmPipelineRunRepository,
};
use cognee_ingestion::DatasetLocks;

use crate::{
    auth_resolver::AuthResolver,
    components::ComponentHandles,
    config::{HttpServerConfig, RegistryConfig},
    error::ServerError,
    observability::{BufferConfig, SpanBuffer},
    sync::SyncRegistry,
};

// ─── AppState ────────────────────────────────────────────────────────────────

/// Per-server dependency container shared across all handlers.
///
/// Fields that depend on closed-side features (auth chain, mailer, etc.)
/// are kept as injection seams (`Option<Arc<dyn ...>>`) so the closed
/// `cognee-http-cloud` crate can populate them via the `RouterBuilder`.
#[derive(Clone)]
pub struct AppState {
    /// HTTP server config (host, port, CORS, JWT, …).
    pub config: Arc<HttpServerConfig>,

    /// Background pipeline-run lifecycle registry.
    ///
    /// The inner `Arc<dyn PipelineRunRegistry>` is `Clone`-able cheaply.
    pub pipelines: Arc<dyn PipelineRunRegistry>,

    /// Pre-built component handles (database, storage, delete_service,
    /// ontology_manager). `None` until `AppState::build` fully initialises
    /// the backends — most tests leave this `None` and stub out the
    /// relevant functionality directly.
    pub lib: Option<Arc<ComponentHandles>>,

    /// Closed-side authentication chain. `None` in pure-OSS builds; closed
    /// embedders install one via `RouterBuilder::with_auth_resolver(...)`
    /// or `RouterBuilder::with_extra_validator(...)`. When `None`, the
    /// `AuthenticatedUser` extractor falls through to either a synthetic
    /// default user (`require_authentication=false`) or a 401
    /// (`require_authentication=true`).
    pub auth_resolver: Option<Arc<dyn AuthResolver>>,

    /// Health checker for /health endpoints. `None` falls back to a
    /// synthetic `MockHealthChecker`. Embedders populate by calling
    /// [`AppState::install_real_health_checker`] after wiring `lib`.
    pub health: Option<Arc<dyn crate::routers::health::HealthChecker>>,

    /// In-memory span buffer feeding `GET /api/v1/activity/spans`.
    /// Always populated — `BufferConfig::from_env()` reads the cap. To
    /// effectively disable the buffer pass `BufferConfig { max_traces: 0, .. }`.
    pub spans: Arc<SpanBuffer>,

    /// In-memory registry tracking one running cloud sync per user. Always
    /// populated; the registry itself starts empty.
    pub sync: Arc<SyncRegistry>,

    /// Per-dataset-identity locks serializing "look the dataset up, create it
    /// if missing, grant the owner's ACL rows" against itself (SDK-636).
    ///
    /// Always populated and shared by every handler, which is the point: the
    /// dataset row and its ACL rows cannot be written in one transaction, so
    /// `POST /v1/datasets` compensates a failed grant by deleting the row it
    /// just wrote. Without a common lock a concurrent `POST /v1/datasets` can
    /// answer 200 for a row that rollback then removes, and a concurrent
    /// `POST /v1/add` can ingest into it and have it deleted underneath.
    ///
    /// In-process only — see [`cognee_ingestion::DatasetLocks`] for what it
    /// does and does not cover.
    pub dataset_locks: Arc<DatasetLocks>,

    /// Flush-on-drop guard for the OpenTelemetry exporter (decision 9).
    /// Held only for its `Drop` side effect: the last `Arc` released calls
    /// `provider.force_flush()` + `provider.shutdown()`. `None` when built
    /// without explicit telemetry init (test paths, library embedders).
    #[cfg(feature = "telemetry")]
    pub telemetry_guard: Option<Arc<TelemetryGuard>>,
}

impl AppState {
    /// Build a no-op `Arc<dyn PipelineRunRegistry>` backed by a
    /// `NoopPipelineRunRepository`.  Useful in tests that construct `AppState`
    /// directly without a real database.
    pub fn noop_pipelines() -> Arc<dyn PipelineRunRegistry> {
        let repo = NoopPipelineRunRepository::arc();
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
        // is wired when `lib` is populated.  For now we use the shared
        // `cognee_database::NoopPipelineRunRepository` (gap 08-07) so the
        // registry is always non-None.
        let repo = NoopPipelineRunRepository::arc();
        let registry_cfg = config.to_registry_config();
        let pipelines: Arc<dyn PipelineRunRegistry> =
            DefaultPipelineRunRegistry::new(repo, registry_cfg);

        Ok(Self {
            config: Arc::new(config),
            pipelines,
            lib: None,
            auth_resolver: None,
            health: None,
            spans: Arc::new(SpanBuffer::new(BufferConfig::from_env())),
            sync: Arc::new(SyncRegistry::new()),
            dataset_locks: Arc::new(DatasetLocks::new()),
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

    /// Replace the `health` field with a `RealHealthChecker` built from the
    /// currently-wired `ComponentHandles`. No-op when `lib` is `None`.
    pub fn install_real_health_checker(&mut self) {
        if let Some(handles) = &self.lib {
            let checker = crate::health::RealHealthChecker::new(Arc::clone(handles), &self.config);
            self.health = Some(Arc::new(checker));
        }
    }
}

// ─── Build state with a real database ─────────────────────────────────────────

impl AppState {
    /// Build `AppState` with a real `DatabaseConnection` wired into the pipeline
    /// registry.  Used by the server startup path when backend env vars are
    /// present.
    ///
    /// Runs the orphan-reset once on startup per pipelines.md §12, and — only
    /// where the deployment asserts one process per relational database —
    /// also sweeps the exclusive-run claims that a killed predecessor could
    /// not release.
    ///
    /// Without graph/vector handles this cannot roll back what a killed run
    /// wrote into those stores; see
    /// [`Self::build_with_db_and_backends`], which the real startup path uses.
    pub async fn build_with_db(
        config: HttpServerConfig,
        db: Arc<DatabaseConnection>,
    ) -> Result<Self, ServerError> {
        Self::build_with_db_and_backends(config, db, None, None).await
    }

    /// [`Self::build_with_db`] plus the graph and vector stores, so startup
    /// recovery can roll back the artifacts a killed run left in them.
    ///
    /// Both handles are `Option` because `ComponentHandles` carries them that
    /// way; with either missing the rollback is skipped (and said so in the
    /// log) while the two relational clears run exactly as before. A rollback
    /// that could not reach one of the stores would delete the ownership rows
    /// that record what still needs deleting, which is worse than not trying.
    pub async fn build_with_db_and_backends(
        config: HttpServerConfig,
        db: Arc<DatabaseConnection>,
        graph_db: Option<Arc<dyn cognee_graph::GraphDBTrait>>,
        vector_db: Option<Arc<dyn cognee_vector::VectorDB>>,
    ) -> Result<Self, ServerError> {
        let repo = Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)))
            as Arc<dyn PipelineRunRepository>;
        let registry_cfg = config.to_registry_config();

        // Clearing claims is sound only with no peer process: a claim is
        // released by its holder alone, so where one process owns the database
        // every surviving claim at startup belongs to a dead predecessor.
        //
        // The derivation is deliberately narrow — only in-memory SQLite, which
        // no other process can open, qualifies on its own. This server's own
        // default is a SQLite *file*, which `cognee-cli` or a second server can
        // open at the same time, and a multi-replica deployment shares a
        // Postgres; both derive `false` here and keep cross-process exclusion
        // exactly as before. `COGNEE_SINGLE_PROCESS` is how a single-process
        // deployment asserts what the URL cannot show.
        let sweep_claims = cognee_database::single_process_from_env(&config.relational_db_url);

        // Roll back what those killed runs wrote into the graph and vector
        // stores, *before* `new_with_orphan_reset` retires their status rows.
        // Retiring first is what makes the dataset runnable again, so it would
        // open a window in which a fresh run starts while this is still
        // deleting the corpse's nodes. Python orders it the same way
        // (`cognify_rollback_handler` before the status reset in
        // `modules/cognify/recovery.py`).
        //
        // Gated on the same assertion as the claim sweep, and for a stronger
        // reason: "in flight" and "dead" are the same observation only when no
        // peer process could be running right now. On a shared database this
        // would delete a live replica's graph artifacts mid-run.
        if sweep_claims {
            match (graph_db, vector_db) {
                (Some(graph_db), Some(vector_db)) => {
                    cognee_delete::sweep_orphaned_run_artifacts(
                        repo.as_ref(),
                        Arc::clone(&db),
                        graph_db,
                        vector_db,
                    )
                    .await;
                }
                _ => tracing::warn!(
                    "single_process is asserted but the graph/vector backends are not wired, so \
                     a killed run's artifacts cannot be rolled back; its pipeline_runs row and \
                     claim are still cleared below"
                ),
            }
        }

        // Run orphan reset on startup (best-effort — non-fatal).
        let pipelines: Arc<dyn PipelineRunRegistry> =
            match DefaultPipelineRunRegistry::new_with_orphan_reset(
                repo,
                registry_cfg,
                sweep_claims,
            )
            .await
            {
                Ok(r) => r,
                Err(_) => {
                    // The error is deliberately not repeated here:
                    // `new_with_orphan_reset` already logged which of its two
                    // startup steps failed and why, and echoing it made the
                    // server report one failure twice. What this line adds is
                    // the consequence — the registry is built without them.
                    tracing::warn!(
                        "continuing without startup pipeline-run recovery (non-fatal); see the \
                         warning above for which step failed"
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
            auth_resolver: None,
            health: None,
            spans: Arc::new(SpanBuffer::new(BufferConfig::from_env())),
            sync: Arc::new(SyncRegistry::new()),
            dataset_locks: Arc::new(DatasetLocks::new()),
            #[cfg(feature = "telemetry")]
            telemetry_guard: None,
        })
    }
}
