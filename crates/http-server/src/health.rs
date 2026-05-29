//! Real `HealthChecker` implementation — probes the live backends behind
//! `ComponentHandles` and aggregates them into a `HealthCheckReport`.
//!
//! Python parity (cognee/api/v1/health/health.py):
//! - Critical components (UNHEALTHY if any fails): `relational_db`,
//!   `vector_db`, `graph_db`, `file_storage`.
//! - Non-critical components (DEGRADED if any fails): `llm_provider`,
//!   `embedding_service` — only checked when opt-in via the
//!   `health_probe_llm` config flag (matches Python's `is_available`
//!   short-circuit).
//! - Overall: any critical UNHEALTHY → UNHEALTHY; else any DEGRADED →
//!   DEGRADED; else HEALTHY.
//!
//! All probes run concurrently via `futures::future::join_all`, each wrapped
//! in `tokio::time::timeout(probe_timeout)`. The aggregated call also enforces
//! a wall-clock deadline (`probe_timeout` + small overhead) so the
//! `/health` request never blocks indefinitely.
//!
//! Results are optionally cached in-process for `health_cache_ttl_ms` to
//! avoid hammering all backends from a fast k8s liveness probe.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, Statement};
use tokio::time::timeout as tokio_timeout;

use crate::{
    components::ComponentHandles,
    config::HttpServerConfig,
    routers::health::{
        ComponentHealth, HealthCheckError, HealthCheckReport, HealthChecker, HealthStatus,
    },
};

// ─── Component name constants ────────────────────────────────────────────────

const COMPONENT_RELATIONAL_DB: &str = "relational_db";
const COMPONENT_VECTOR_DB: &str = "vector_db";
const COMPONENT_GRAPH_DB: &str = "graph_db";
const COMPONENT_FILE_STORAGE: &str = "file_storage";
const COMPONENT_LLM: &str = "llm_provider";
const COMPONENT_EMBEDDING: &str = "embedding_service";

// ─── Cache ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct CachedReport {
    report: HealthCheckReport,
    cached_at: Instant,
}

// ─── RealHealthChecker ───────────────────────────────────────────────────────

/// Live health checker that exercises each backend in `ComponentHandles`.
///
/// Build with [`RealHealthChecker::new`] from an `Arc<ComponentHandles>` and
/// the active [`HttpServerConfig`]. The probe timeout, cache TTL and LLM
/// opt-in flag are pulled from the config; mutating the config after
/// construction has no effect on the checker.
pub struct RealHealthChecker {
    components: Arc<ComponentHandles>,
    start_time: Instant,
    probe_timeout: Duration,
    cache_ttl: Duration,
    probe_llm: bool,
    cache: Mutex<Option<CachedReport>>,
}

impl RealHealthChecker {
    /// Create a new real checker from the wired component handles and config.
    pub fn new(components: Arc<ComponentHandles>, config: &HttpServerConfig) -> Self {
        Self {
            components,
            start_time: Instant::now(),
            probe_timeout: Duration::from_millis(config.health_probe_timeout_ms),
            cache_ttl: Duration::from_millis(config.health_cache_ttl_ms),
            probe_llm: config.health_probe_llm,
            cache: Mutex::new(None),
        }
    }

    /// Run all probes concurrently and assemble the report.
    async fn run_probes(&self) -> HealthCheckReport {
        // Build futures for every probe up-front; tokio::join! schedules
        // them concurrently on the current task.
        let db_fut = self.probe_database();
        let graph_fut = self.probe_graph_db();
        let vector_fut = self.probe_vector_db();
        let storage_fut = self.probe_file_storage();

        let (db, graph, vector, storage) = tokio::join!(db_fut, graph_fut, vector_fut, storage_fut);

        let mut components = HashMap::new();
        components.insert(COMPONENT_RELATIONAL_DB.to_string(), db);
        components.insert(COMPONENT_GRAPH_DB.to_string(), graph);
        components.insert(COMPONENT_VECTOR_DB.to_string(), vector);
        components.insert(COMPONENT_FILE_STORAGE.to_string(), storage);

        // Non-critical probes run only when the operator opts in. When the
        // flag is off, the keys are omitted from the report entirely — this
        // mirrors Python's "is_available" short-circuit, which prevents the
        // /health endpoint from echoing back the absence of a remote
        // provider for every liveness probe.
        if self.probe_llm {
            let llm = self.probe_llm_provider().await;
            let embedding = self.probe_embedding_engine().await;
            components.insert(COMPONENT_LLM.to_string(), llm);
            components.insert(COMPONENT_EMBEDDING.to_string(), embedding);
        }

        // Aggregate overall status from per-component statuses (Python
        // parity): critical UNHEALTHY → UNHEALTHY; any DEGRADED → DEGRADED;
        // otherwise HEALTHY. Non-critical components can only escalate to
        // DEGRADED.
        let mut overall = HealthStatus::Healthy;
        for (name, comp) in &components {
            let is_critical = matches!(
                name.as_str(),
                COMPONENT_RELATIONAL_DB
                    | COMPONENT_VECTOR_DB
                    | COMPONENT_GRAPH_DB
                    | COMPONENT_FILE_STORAGE
            );
            match (comp.status, is_critical) {
                (HealthStatus::Unhealthy, true) => {
                    overall = HealthStatus::Unhealthy;
                }
                (HealthStatus::Unhealthy, false) | (HealthStatus::Degraded, _) => {
                    if overall == HealthStatus::Healthy {
                        overall = HealthStatus::Degraded;
                    }
                }
                _ => {}
            }
        }

        HealthCheckReport {
            status: overall,
            timestamp: chrono::Utc::now(),
            version: env!("CARGO_PKG_VERSION").into(),
            uptime: self.start_time.elapsed(),
            components,
        }
    }

    /// Wrap a probe future in `tokio::time::timeout` and convert a timeout
    /// into the supplied "timed-out" `ComponentHealth`.
    async fn with_timeout<F>(&self, provider: &str, critical: bool, fut: F) -> ComponentHealth
    where
        F: std::future::Future<Output = ComponentHealth>,
    {
        let started = Instant::now();
        match tokio_timeout(self.probe_timeout, fut).await {
            Ok(result) => result,
            Err(_elapsed) => ComponentHealth {
                status: if critical {
                    HealthStatus::Unhealthy
                } else {
                    HealthStatus::Degraded
                },
                provider: provider.to_string(),
                response_time: started.elapsed(),
                details: format!(
                    "probe timed out after {} ms",
                    self.probe_timeout.as_millis()
                ),
            },
        }
    }

    // ── Per-component probes ──────────────────────────────────────────────

    async fn probe_database(&self) -> ComponentHealth {
        let db = Arc::clone(&self.components.database);
        let provider = cognee_database::database_system_label(&db).to_string();
        self.with_timeout(&provider, true, async move {
            let started = Instant::now();
            let backend = db.get_database_backend();
            let stmt = Statement::from_string(backend, "SELECT 1".to_string());
            match db.execute(stmt).await {
                Ok(_) => ComponentHealth {
                    status: HealthStatus::Healthy,
                    provider: cognee_database::database_system_label(&db).to_string(),
                    response_time: started.elapsed(),
                    details: "SELECT 1 ok".into(),
                },
                Err(e) => ComponentHealth {
                    status: HealthStatus::Unhealthy,
                    provider: cognee_database::database_system_label(&db).to_string(),
                    response_time: started.elapsed(),
                    // Avoid echoing connection strings; SeaORM's Display
                    // typically contains the driver class + short message.
                    details: format!("query failed: {e}"),
                },
            }
        })
        .await
    }

    async fn probe_graph_db(&self) -> ComponentHealth {
        let graph = self.components.graph_db.as_ref().map(Arc::clone);
        self.with_timeout("graph_db", true, async move {
            let started = Instant::now();
            match graph {
                None => ComponentHealth {
                    status: HealthStatus::Unhealthy,
                    provider: "none".into(),
                    response_time: started.elapsed(),
                    details: "graph backend not wired".into(),
                },
                Some(g) => match g.is_empty().await {
                    Ok(_) => ComponentHealth {
                        status: HealthStatus::Healthy,
                        provider: "graph".into(),
                        response_time: started.elapsed(),
                        details: "is_empty ok".into(),
                    },
                    Err(e) => ComponentHealth {
                        status: HealthStatus::Unhealthy,
                        provider: "graph".into(),
                        response_time: started.elapsed(),
                        details: format!("is_empty failed: {e}"),
                    },
                },
            }
        })
        .await
    }

    async fn probe_vector_db(&self) -> ComponentHealth {
        let vector = self.components.vector_db.as_ref().map(Arc::clone);
        self.with_timeout("vector_db", true, async move {
            let started = Instant::now();
            match vector {
                None => ComponentHealth {
                    status: HealthStatus::Unhealthy,
                    provider: "none".into(),
                    response_time: started.elapsed(),
                    details: "vector backend not wired".into(),
                },
                Some(v) => match v.list_collections().await {
                    Ok(cols) => ComponentHealth {
                        status: HealthStatus::Healthy,
                        provider: "vector".into(),
                        response_time: started.elapsed(),
                        details: format!("{} collection(s)", cols.len()),
                    },
                    Err(e) => ComponentHealth {
                        status: HealthStatus::Unhealthy,
                        provider: "vector".into(),
                        response_time: started.elapsed(),
                        details: format!("list_collections failed: {e}"),
                    },
                },
            }
        })
        .await
    }

    async fn probe_file_storage(&self) -> ComponentHealth {
        let storage = Arc::clone(&self.components.storage);
        self.with_timeout("file_storage", true, async move {
            let started = Instant::now();
            // Round-trip a small probe artifact through write/read/delete.
            // The name uses a UUID v4 so concurrent probes never collide,
            // and the scope is the storage backend's own root rather than
            // a global temp dir (matches the security note in the plan).
            let probe_id = uuid::Uuid::new_v4();
            let file_name = format!(".health-check-{probe_id}");
            let payload = b"cognee-health-probe";

            let store_result = storage.store(payload, &file_name).await;
            let location = match store_result {
                Ok(loc) => loc,
                Err(e) => {
                    return ComponentHealth {
                        status: HealthStatus::Unhealthy,
                        provider: "local".into(),
                        response_time: started.elapsed(),
                        details: format!("store failed: {e}"),
                    };
                }
            };

            // Helper: best-effort cleanup that always runs, regardless of
            // the retrieve outcome. We swallow cleanup errors so they
            // don't override the actual probe result.
            let retrieve_result = storage.retrieve(&location).await;
            let _ = storage.delete(&location).await;

            match retrieve_result {
                Ok(buf) if buf == payload => ComponentHealth {
                    status: HealthStatus::Healthy,
                    provider: "local".into(),
                    response_time: started.elapsed(),
                    details: "write/read/delete ok".into(),
                },
                Ok(_) => ComponentHealth {
                    status: HealthStatus::Unhealthy,
                    provider: "local".into(),
                    response_time: started.elapsed(),
                    details: "round-trip payload mismatch".into(),
                },
                Err(e) => ComponentHealth {
                    status: HealthStatus::Unhealthy,
                    provider: "local".into(),
                    response_time: started.elapsed(),
                    details: format!("retrieve failed: {e}"),
                },
            }
        })
        .await
    }

    async fn probe_llm_provider(&self) -> ComponentHealth {
        let llm = self.components.llm.as_ref().map(Arc::clone);
        self.with_timeout("llm", false, async move {
            let started = Instant::now();
            match llm {
                None => ComponentHealth {
                    status: HealthStatus::Degraded,
                    provider: "none".into(),
                    response_time: started.elapsed(),
                    details: "LLM backend not wired".into(),
                },
                Some(l) => {
                    // 1-token max-output ping. We do not retain the body —
                    // recording it on the health response could echo
                    // upstream content back to unauthenticated callers.
                    let opts = cognee_llm::types::GenerationOptions {
                        temperature: Some(0.0),
                        max_tokens: Some(1),
                        ..Default::default()
                    };
                    let msgs = vec![cognee_llm::types::Message::user("ping")];
                    match l.generate(msgs, Some(opts)).await {
                        Ok(_resp) => ComponentHealth {
                            status: HealthStatus::Healthy,
                            provider: "llm".into(),
                            response_time: started.elapsed(),
                            details: "generate ok".into(),
                        },
                        Err(e) => ComponentHealth {
                            status: HealthStatus::Degraded,
                            provider: "llm".into(),
                            response_time: started.elapsed(),
                            details: format!("generate failed: {e}"),
                        },
                    }
                }
            }
        })
        .await
    }

    async fn probe_embedding_engine(&self) -> ComponentHealth {
        let engine = self.components.embedding_engine.as_ref().map(Arc::clone);
        self.with_timeout("embedding", false, async move {
            let started = Instant::now();
            match engine {
                None => ComponentHealth {
                    status: HealthStatus::Degraded,
                    provider: "none".into(),
                    response_time: started.elapsed(),
                    details: "embedding backend not wired".into(),
                },
                Some(e) => match e.embed(&["ping"]).await {
                    Ok(vectors) => ComponentHealth {
                        status: HealthStatus::Healthy,
                        provider: "embedding".into(),
                        response_time: started.elapsed(),
                        details: format!(
                            "embed ok ({} vector(s), dim={})",
                            vectors.len(),
                            vectors.first().map(|v| v.len()).unwrap_or(0)
                        ),
                    },
                    Err(err) => ComponentHealth {
                        status: HealthStatus::Degraded,
                        provider: "embedding".into(),
                        response_time: started.elapsed(),
                        details: format!("embed failed: {err}"),
                    },
                },
            }
        })
        .await
    }
}

#[async_trait]
impl HealthChecker for RealHealthChecker {
    async fn get_health_status(
        &self,
        _detailed: bool,
    ) -> Result<HealthCheckReport, HealthCheckError> {
        // Fast path: serve a fresh cache entry without touching backends.
        // We deliberately ignore the `detailed` flag for cache lookup — the
        // probe set is identical (the only difference is the response
        // shape rendered by the handler).
        if !self.cache_ttl.is_zero() {
            // Lock poison is unrecoverable — propagate the panic the same
            // way Mutex always does.
            let guard = self.cache.lock().unwrap();
            if let Some(cached) = guard.as_ref()
                && cached.cached_at.elapsed() < self.cache_ttl
            {
                return Ok(cached.report.clone());
            }
            drop(guard);
        }

        // Wall-clock deadline: each probe is already wrapped in
        // `with_timeout(probe_timeout)`; running them concurrently means the
        // join finishes when the slowest probe does. We add a small slack
        // (50 ms) on top of `probe_timeout` to allow the timeout branches to
        // produce their structured `ComponentHealth` entries before the
        // outer deadline fires.
        let deadline = self.probe_timeout + Duration::from_millis(50);
        let report = match tokio_timeout(deadline, self.run_probes()).await {
            Ok(r) => r,
            Err(_elapsed) => {
                // All probes blew past the deadline. Surface this as a
                // checker-level failure so the handler returns the
                // failure-shape body (`status: not ready` / `error`
                // depending on the route).
                return Err(HealthCheckError(format!(
                    "health probes exceeded deadline ({} ms)",
                    deadline.as_millis()
                )));
            }
        };

        if !self.cache_ttl.is_zero() {
            let mut guard = self.cache.lock().unwrap();
            *guard = Some(CachedReport {
                report: report.clone(),
                cached_at: Instant::now(),
            });
        }

        Ok(report)
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_database::{DatabaseConnection, connect, initialize};
    use cognee_delete::DeleteService;
    use cognee_embedding::MockEmbeddingEngine;
    use cognee_graph::MockGraphDB;
    use cognee_llm::Llm;
    use cognee_ontology::OntologyManager;
    use cognee_storage::{LocalStorage, StorageError, StorageTrait};
    use cognee_vector::MockVectorDB;

    /// Build an in-memory SQLite + LocalStorage `ComponentHandles` with the
    /// supplied optional overrides.
    async fn build_handles(
        storage: Option<Arc<dyn StorageTrait>>,
        graph_ok: bool,
        vector_ok: bool,
        llm: Option<Arc<dyn Llm>>,
        embedding: Option<Arc<dyn cognee_embedding::EmbeddingEngine>>,
    ) -> Arc<ComponentHandles> {
        let db: Arc<DatabaseConnection> = Arc::new(
            connect("sqlite::memory:")
                .await
                .expect("open in-memory sqlite"),
        );
        initialize(&db).await.expect("run migrations");

        let storage_dir = tempfile::tempdir().expect("tmp storage");
        let local =
            Arc::new(LocalStorage::new(storage_dir.path().to_path_buf())) as Arc<dyn StorageTrait>;
        Box::leak(Box::new(storage_dir));
        let storage_handle = storage.unwrap_or(local);

        let delete_service = Arc::new(DeleteService::new(
            Arc::clone(&storage_handle),
            db.clone() as Arc<dyn cognee_database::DeleteDb>,
        ));
        let ontology_dir = tempfile::tempdir().expect("tmp ontology");
        let ontology_manager = Arc::new(OntologyManager::new(ontology_dir.path().to_path_buf()));
        Box::leak(Box::new(ontology_dir));

        let graph_db: Option<Arc<dyn cognee_graph::GraphDBTrait>> = if graph_ok {
            Some(Arc::new(MockGraphDB::new()))
        } else {
            Some(Arc::new(FailingGraphDB::new()))
        };
        let vector_db: Option<Arc<dyn cognee_vector::VectorDB>> = if vector_ok {
            Some(Arc::new(MockVectorDB::new()))
        } else {
            Some(Arc::new(FailingVectorDB::new()))
        };

        Arc::new(ComponentHandles {
            database: db,
            storage: storage_handle,
            delete_service,
            ontology_manager,
            search_orchestrator: None,
            llm,
            embedding_engine: embedding,
            graph_db,
            vector_db,
            thread_pool: None,
            permissions: None,
            sync_ops: None,
            session_store: None,
            session_manager: None,
            checkpoint_store: None,
            ontology_resolver: None,
            responses_client: None,
            notebook_runner: None,
        })
    }

    fn fast_config() -> HttpServerConfig {
        HttpServerConfig {
            health_probe_timeout_ms: 500,
            health_cache_ttl_ms: 0,
            ..HttpServerConfig::default()
        }
    }

    #[tokio::test]
    async fn all_healthy_yields_overall_healthy() {
        let handles = build_handles(None, true, true, None, None).await;
        let checker = RealHealthChecker::new(handles, &fast_config());

        let report = checker
            .get_health_status(true)
            .await
            .expect("checker should succeed");

        assert_eq!(report.status, HealthStatus::Healthy);
        for key in &[
            COMPONENT_RELATIONAL_DB,
            COMPONENT_VECTOR_DB,
            COMPONENT_GRAPH_DB,
            COMPONENT_FILE_STORAGE,
        ] {
            let comp = report
                .components
                .get(*key)
                .unwrap_or_else(|| panic!("missing {key}"));
            assert_eq!(comp.status, HealthStatus::Healthy, "{key} status");
        }
        // LLM/embedding are opt-in; default config = false → omitted.
        assert!(!report.components.contains_key(COMPONENT_LLM));
        assert!(!report.components.contains_key(COMPONENT_EMBEDDING));
    }

    #[tokio::test]
    async fn critical_graph_failure_yields_unhealthy() {
        let handles = build_handles(None, false, true, None, None).await;
        let checker = RealHealthChecker::new(handles, &fast_config());

        let report = checker
            .get_health_status(true)
            .await
            .expect("checker should succeed");

        assert_eq!(report.status, HealthStatus::Unhealthy);
        let graph = report
            .components
            .get(COMPONENT_GRAPH_DB)
            .expect("graph entry");
        assert_eq!(graph.status, HealthStatus::Unhealthy);
    }

    #[tokio::test]
    async fn non_critical_llm_failure_yields_degraded() {
        let handles = build_handles(
            None,
            true,
            true,
            Some(Arc::new(FailingLlm)),
            // Pair with a healthy embedding so the only failure is the LLM.
            Some(Arc::new(MockEmbeddingEngine::new(8))),
        )
        .await;
        let cfg = HttpServerConfig {
            health_probe_llm: true,
            ..fast_config()
        };
        let checker = RealHealthChecker::new(handles, &cfg);

        let report = checker.get_health_status(true).await.expect("report");
        assert_eq!(report.status, HealthStatus::Degraded);
        let llm_entry = report.components.get(COMPONENT_LLM).expect("llm entry");
        assert_eq!(llm_entry.status, HealthStatus::Degraded);
        let emb_entry = report
            .components
            .get(COMPONENT_EMBEDDING)
            .expect("embedding entry");
        assert_eq!(emb_entry.status, HealthStatus::Healthy);
    }

    #[tokio::test]
    async fn opt_in_llm_off_omits_entries() {
        let handles = build_handles(
            None,
            true,
            true,
            Some(Arc::new(FailingLlm)),
            Some(Arc::new(FailingEmbedding)),
        )
        .await;
        // Default config: health_probe_llm = false.
        let checker = RealHealthChecker::new(handles, &fast_config());

        let report = checker.get_health_status(true).await.expect("report");
        // Even though both broken backends are wired, the report should
        // omit them entirely → overall stays HEALTHY.
        assert_eq!(report.status, HealthStatus::Healthy);
        assert!(!report.components.contains_key(COMPONENT_LLM));
        assert!(!report.components.contains_key(COMPONENT_EMBEDDING));
    }

    #[tokio::test]
    async fn slow_probe_times_out_within_budget() {
        // Use a slow storage that blocks on store(); the per-probe timeout
        // is 100 ms, so the request must return well under 2 s.
        let slow_storage: Arc<dyn StorageTrait> = Arc::new(SlowStorage::new());
        let handles = build_handles(Some(slow_storage), true, true, None, None).await;
        let cfg = HttpServerConfig {
            health_probe_timeout_ms: 100,
            health_cache_ttl_ms: 0,
            ..HttpServerConfig::default()
        };
        let checker = RealHealthChecker::new(handles, &cfg);

        let started = Instant::now();
        let report = checker.get_health_status(true).await.expect("report");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(1000),
            "checker hung past per-probe timeout: {elapsed:?}"
        );
        let storage_entry = report
            .components
            .get(COMPONENT_FILE_STORAGE)
            .expect("storage entry");
        assert_eq!(storage_entry.status, HealthStatus::Unhealthy);
        assert!(
            storage_entry.details.contains("timed out"),
            "expected timeout details, got: {}",
            storage_entry.details
        );
        // Critical timeout → overall must be Unhealthy.
        assert_eq!(report.status, HealthStatus::Unhealthy);
    }

    #[tokio::test]
    async fn cache_serves_repeat_calls() {
        let handles = build_handles(None, true, true, None, None).await;
        let cfg = HttpServerConfig {
            health_probe_timeout_ms: 500,
            health_cache_ttl_ms: 10_000,
            ..HttpServerConfig::default()
        };
        let checker = RealHealthChecker::new(handles, &cfg);

        let first = checker.get_health_status(true).await.expect("first");
        let second = checker.get_health_status(true).await.expect("second");
        // Timestamps must match because the second call short-circuits on
        // the cache.
        assert_eq!(first.timestamp, second.timestamp);
    }

    // ── helper mocks ──────────────────────────────────────────────────────

    /// Wraps a [`MockGraphDB`] and re-emits an error from `is_empty()`. All
    /// other methods delegate to the inner mock, which lets the rest of the
    /// pipeline keep working in tests that exercise multi-backend behaviour.
    struct FailingGraphDB {
        inner: MockGraphDB,
    }

    impl FailingGraphDB {
        fn new() -> Self {
            Self {
                inner: MockGraphDB::new(),
            }
        }
    }

    #[async_trait]
    impl cognee_graph::GraphDBTrait for FailingGraphDB {
        async fn initialize(&self) -> cognee_graph::GraphDBResult<()> {
            self.inner.initialize().await
        }
        async fn is_empty(&self) -> cognee_graph::GraphDBResult<bool> {
            Err(cognee_graph::GraphDBError::QueryError(
                "synthetic graph failure".into(),
            ))
        }
        async fn query(
            &self,
            q: &str,
            params: Option<
                std::collections::HashMap<std::borrow::Cow<'static, str>, serde_json::Value>,
            >,
        ) -> cognee_graph::GraphDBResult<Vec<Vec<serde_json::Value>>> {
            self.inner.query(q, params).await
        }
        async fn delete_graph(&self) -> cognee_graph::GraphDBResult<()> {
            self.inner.delete_graph().await
        }
        async fn has_node(&self, id: &str) -> cognee_graph::GraphDBResult<bool> {
            self.inner.has_node(id).await
        }
        async fn add_node_raw(&self, n: serde_json::Value) -> cognee_graph::GraphDBResult<()> {
            self.inner.add_node_raw(n).await
        }
        async fn add_nodes_raw(
            &self,
            ns: Vec<serde_json::Value>,
        ) -> cognee_graph::GraphDBResult<()> {
            self.inner.add_nodes_raw(ns).await
        }
        async fn delete_node(&self, id: &str) -> cognee_graph::GraphDBResult<()> {
            self.inner.delete_node(id).await
        }
        async fn delete_nodes(&self, ids: &[String]) -> cognee_graph::GraphDBResult<()> {
            self.inner.delete_nodes(ids).await
        }
        async fn get_node(
            &self,
            id: &str,
        ) -> cognee_graph::GraphDBResult<Option<cognee_graph::NodeData>> {
            self.inner.get_node(id).await
        }
        async fn get_nodes(
            &self,
            ids: &[String],
        ) -> cognee_graph::GraphDBResult<Vec<cognee_graph::NodeData>> {
            self.inner.get_nodes(ids).await
        }
        async fn has_edge(&self, s: &str, t: &str, r: &str) -> cognee_graph::GraphDBResult<bool> {
            self.inner.has_edge(s, t, r).await
        }
        async fn has_edges(
            &self,
            edges: &[cognee_graph::EdgeData],
        ) -> cognee_graph::GraphDBResult<Vec<cognee_graph::EdgeData>> {
            self.inner.has_edges(edges).await
        }
        async fn add_edge(
            &self,
            s: &str,
            t: &str,
            r: &str,
            p: Option<std::collections::HashMap<std::borrow::Cow<'static, str>, serde_json::Value>>,
        ) -> cognee_graph::GraphDBResult<()> {
            self.inner.add_edge(s, t, r, p).await
        }
        async fn add_edges(
            &self,
            edges: &[cognee_graph::EdgeData],
        ) -> cognee_graph::GraphDBResult<()> {
            self.inner.add_edges(edges).await
        }
        async fn get_edges(
            &self,
            id: &str,
        ) -> cognee_graph::GraphDBResult<Vec<cognee_graph::EdgeData>> {
            self.inner.get_edges(id).await
        }
        async fn get_neighbors(
            &self,
            id: &str,
        ) -> cognee_graph::GraphDBResult<Vec<cognee_graph::NodeData>> {
            self.inner.get_neighbors(id).await
        }
        async fn get_connections(
            &self,
            id: &str,
        ) -> cognee_graph::GraphDBResult<
            Vec<(
                cognee_graph::NodeData,
                std::collections::HashMap<std::borrow::Cow<'static, str>, serde_json::Value>,
                cognee_graph::NodeData,
            )>,
        > {
            self.inner.get_connections(id).await
        }
        async fn get_graph_data(
            &self,
        ) -> cognee_graph::GraphDBResult<(Vec<cognee_graph::GraphNode>, Vec<cognee_graph::EdgeData>)>
        {
            self.inner.get_graph_data().await
        }
        async fn get_graph_metrics(
            &self,
            include_optional: bool,
        ) -> cognee_graph::GraphDBResult<
            std::collections::HashMap<std::borrow::Cow<'static, str>, serde_json::Value>,
        > {
            self.inner.get_graph_metrics(include_optional).await
        }
        async fn get_filtered_graph_data(
            &self,
            f: &std::collections::HashMap<std::borrow::Cow<'static, str>, Vec<serde_json::Value>>,
        ) -> cognee_graph::GraphDBResult<(Vec<cognee_graph::GraphNode>, Vec<cognee_graph::EdgeData>)>
        {
            self.inner.get_filtered_graph_data(f).await
        }
        async fn get_nodeset_subgraph(
            &self,
            node_type: &str,
            node_names: &[String],
            op: &str,
        ) -> cognee_graph::GraphDBResult<(Vec<cognee_graph::GraphNode>, Vec<cognee_graph::EdgeData>)>
        {
            self.inner
                .get_nodeset_subgraph(node_type, node_names, op)
                .await
        }
    }

    /// Wraps a [`MockVectorDB`] and re-emits an error from `list_collections()`.
    struct FailingVectorDB {
        inner: MockVectorDB,
    }

    impl FailingVectorDB {
        fn new() -> Self {
            Self {
                inner: MockVectorDB::new(),
            }
        }
    }

    #[async_trait]
    impl cognee_vector::VectorDB for FailingVectorDB {
        async fn create_collection(
            &self,
            data_type: &str,
            field_name: &str,
            dimension: usize,
        ) -> cognee_vector::VectorDBResult<()> {
            self.inner
                .create_collection(data_type, field_name, dimension)
                .await
        }
        async fn has_collection(
            &self,
            data_type: &str,
            field_name: &str,
        ) -> cognee_vector::VectorDBResult<bool> {
            self.inner.has_collection(data_type, field_name).await
        }
        async fn index_points(
            &self,
            data_type: &str,
            field_name: &str,
            points: &[cognee_vector::VectorPoint],
        ) -> cognee_vector::VectorDBResult<()> {
            self.inner.index_points(data_type, field_name, points).await
        }
        async fn search_similar(
            &self,
            data_type: &str,
            field_name: &str,
            query_vector: &[f32],
            top_k: usize,
        ) -> cognee_vector::VectorDBResult<Vec<cognee_vector::SearchResult>> {
            self.inner
                .search_similar(data_type, field_name, query_vector, top_k)
                .await
        }
        async fn delete_collection(
            &self,
            data_type: &str,
            field_name: &str,
        ) -> cognee_vector::VectorDBResult<()> {
            self.inner.delete_collection(data_type, field_name).await
        }
        async fn collection_size(
            &self,
            data_type: &str,
            field_name: &str,
        ) -> cognee_vector::VectorDBResult<usize> {
            self.inner.collection_size(data_type, field_name).await
        }
        async fn list_collections(&self) -> cognee_vector::VectorDBResult<Vec<(String, String)>> {
            Err(cognee_vector::VectorDBError::StorageError(
                "synthetic vector failure".into(),
            ))
        }
    }

    struct FailingLlm;
    #[async_trait]
    impl cognee_llm::Llm for FailingLlm {
        fn model(&self) -> &str {
            "failing-test-llm"
        }
        async fn generate(
            &self,
            _messages: Vec<cognee_llm::types::Message>,
            _options: Option<cognee_llm::types::GenerationOptions>,
        ) -> cognee_llm::LlmResult<cognee_llm::types::GenerationResponse> {
            Err(cognee_llm::LlmError::ApiError(
                "synthetic llm failure".into(),
            ))
        }
        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<cognee_llm::types::Message>,
            _json_schema: &serde_json::Value,
            _options: Option<cognee_llm::types::GenerationOptions>,
        ) -> cognee_llm::LlmResult<serde_json::Value> {
            Err(cognee_llm::LlmError::ApiError(
                "synthetic llm failure".into(),
            ))
        }
    }

    struct FailingEmbedding;
    #[async_trait]
    impl cognee_embedding::EmbeddingEngine for FailingEmbedding {
        async fn embed(&self, _texts: &[&str]) -> cognee_embedding::EmbeddingResult<Vec<Vec<f32>>> {
            Err(cognee_embedding::EmbeddingError::InferenceError(
                "synthetic embedding failure".into(),
            ))
        }
        fn dimension(&self) -> usize {
            8
        }
        fn batch_size(&self) -> usize {
            1
        }
        fn max_sequence_length(&self) -> usize {
            16
        }
    }

    /// Storage backend whose `store` future never resolves within the test
    /// timeout, exercising the per-probe `tokio::time::timeout` branch.
    struct SlowStorage {
        inner: Arc<LocalStorage>,
        _guard: Arc<tempfile::TempDir>,
    }

    impl SlowStorage {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tmp dir");
            let inner = Arc::new(LocalStorage::new(dir.path().to_path_buf()));
            Self {
                inner,
                _guard: Arc::new(dir),
            }
        }
    }

    #[async_trait]
    impl StorageTrait for SlowStorage {
        async fn store(&self, _data: &[u8], _file_name: &str) -> Result<String, StorageError> {
            // Sleep longer than any reasonable probe timeout in tests.
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok("never".into())
        }
        async fn store_stream_dyn(
            &self,
            reader: &mut (dyn tokio::io::AsyncRead + Unpin + Send),
            file_name: &str,
        ) -> Result<String, StorageError> {
            self.inner.store_stream_dyn(reader, file_name).await
        }
        async fn create_writer(
            &self,
            file_name: &str,
        ) -> Result<cognee_storage::StorageWriter, StorageError> {
            self.inner.create_writer(file_name).await
        }
        async fn retrieve(&self, location: &str) -> Result<Vec<u8>, StorageError> {
            self.inner.retrieve(location).await
        }
        async fn exists(&self, location: &str) -> Result<bool, StorageError> {
            self.inner.exists(location).await
        }
        async fn delete(&self, location: &str) -> Result<(), StorageError> {
            self.inner.delete(location).await
        }
        fn get_full_path(&self, location: &str) -> std::path::PathBuf {
            self.inner.get_full_path(location)
        }
        fn base_path(&self) -> &str {
            self.inner.base_path()
        }
        async fn initialize(&self) -> Result<(), StorageError> {
            self.inner.initialize().await
        }
        async fn remove_all(&self) -> Result<(), StorageError> {
            self.inner.remove_all().await
        }
    }
}
