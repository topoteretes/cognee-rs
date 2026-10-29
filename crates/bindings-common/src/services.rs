//! `CogneeServices` — the single place where the 6 raw engines from
//! `ComponentManager` and all derived services are built and cached.
//!
//! This is the keystone facade for the SDK bindings: every `sdk_*` function
//! obtains a `CogneeServices` via `HandleState::services()` and calls a
//! `cognee` API with the bundled `Arc<dyn …>` handles, so the wiring lives
//! in exactly one place (mirroring the CLI command builders, which are the
//! authoritative reference).

use std::sync::Arc;

use uuid::Uuid;

use cognee::ComponentManager;
use cognee::PipelineContext;
use cognee::add::AddPipeline;
use cognee::api::get_or_create_default_user;
use cognee::cognify::CognifyConfig;
use cognee::core::{CpuPool, DatasetLocks, RayonThreadPool};
use cognee::database::{
    CheckpointStore, DeleteDb, IngestDb, PipelineRunRepository, RelationalDb, SearchHistoryDb,
};
use cognee::delete::DeleteService;
use cognee::embedding::EmbeddingEngine;
use cognee::graph::GraphDBTrait;
use cognee::llm::Llm;
use cognee::ontology::OntologyResolver;
use cognee::search::{SearchBuilder, SearchOrchestrator, SessionManager, SessionStore};
use cognee::storage::StorageTrait;
use cognee::vector::VectorDB;

use crate::SdkError;

/// A fully-wired bundle of engines + derived services.
///
/// Built once per config version by [`CogneeServices::build`] and cached by the
/// handle. All fields are `Arc`-shared so `sdk_*` functions can cheaply clone a
/// handle into a `cognee` API call.
// Most fields are consumed by the SDK ops added in later phases; they are part
// of the facade contract now so the wiring lives in one place.
#[allow(dead_code)]
pub struct CogneeServices {
    // 6 raw engines from `ComponentManager` (the `PipelineContext` surface).
    pub storage: Arc<dyn StorageTrait>,
    /// The relational backend, as built by the `ComponentManager` registry.
    /// [`RelationalDb`] has every per-domain DB trait as a supertrait, so derived
    /// services upcast it with `Arc::clone(&database) as Arc<dyn IngestDb>` (etc.),
    /// or use its accessors such as `pipeline_runs()` and `checkpoints()`.
    pub database: Arc<dyn RelationalDb>,
    pub graph_db: Arc<dyn GraphDBTrait>,
    pub vector_db: Arc<dyn VectorDB>,
    pub embedding_engine: Arc<dyn EmbeddingEngine>,
    pub llm: Arc<dyn Llm>,

    // Derived services (built here; see the §4 facade table in the plan).
    pub thread_pool: Arc<RayonThreadPool>,
    pub pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    pub add_pipeline: Arc<AddPipeline>,
    pub delete_service: Arc<DeleteService>,
    pub search_orchestrator: Arc<SearchOrchestrator>,
    pub session_store: Arc<dyn SessionStore>,
    pub session_manager: Arc<SessionManager>,
    pub ontology_resolver: Arc<dyn OntologyResolver>,
    pub cognify_config: CognifyConfig,
    pub checkpoint_store: Arc<dyn CheckpointStore>,
    /// The manager these services were built from; per-run components (the
    /// graph-extraction backend) are built through its registry.
    pub component_manager: Arc<ComponentManager>,
}

/// Startup-recovery gates, one per relational database this process has
/// touched, keyed by a hash of the resolved URL. The `bool` a gate resolves to
/// is "this process actually ran recovery on that database".
///
/// Per database, not a single process-wide flag: one process can build
/// services against more than one relational URL (a test harness, an embedder
/// switching tenants), and a bare flag would recover the first and silently
/// skip every other. Per *process* and not per handle or per config version,
/// because `CogneeServices::build` runs again on every config-version change
/// and recovering once is enough: a run is only ever recovered when its
/// ownership marker can be claimed (see `cognee_delete::recover_stale_pipeline_runs`),
/// which a live run of this process never allows.
///
/// Hashed rather than stored verbatim because a Postgres URL carries its
/// password, and this map lives for the whole process. Nothing ever reads a
/// URL back out of it — the only question asked is "seen before?".
///
/// `BTreeMap` rather than `HashMap` so the whole thing is a `const` initialiser
/// and needs no lazy wrapper.
static RECOVERY_GATES: std::sync::Mutex<
    std::collections::BTreeMap<u64, Arc<tokio::sync::OnceCell<bool>>>,
> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The gate for `relational_db_url`, creating it if this is the first sight of
/// that database.
///
/// A *gate*, not a "was it swept?" boolean, because the answer has to be given
/// to a concurrent second builder only once recovery has actually finished.
/// The previous version returned `false` immediately to every builder after
/// the first, which is a race: recovery is asynchronous, so builder B could
/// return, start a cognify, and have builder A's still-running sweep retire
/// that live run. When all the sweep did was drop a claim that was a
/// millisecond-wide annoyance; now that it deletes the run's graph and vector
/// artifacts it is data loss. A [`tokio::sync::OnceCell`] makes every later
/// builder *wait* for the first one's recovery instead of overtaking it.
///
/// Failure mode, deliberately chosen: if the recovery future panics the cell
/// stays uninitialised and the next builder retries it, rather than every
/// waiter blocking forever. Recovery itself is written to be infallible —
/// every step logs its own failure and returns `()` — so the retry path is a
/// backstop, not a design point.
fn recovery_gate(relational_db_url: &str) -> Arc<tokio::sync::OnceCell<bool>> {
    use std::hash::{Hash, Hasher};

    // A process-local dedup key, not a security boundary and not persisted, so
    // `DefaultHasher` is enough. A collision between two URLs in one process
    // would skip a recovery rather than perform an extra one.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    relational_db_url.hash(&mut hasher);
    let key = hasher.finish();

    let mut gates = match RECOVERY_GATES.lock() {
        Ok(guard) => guard,
        // A panic in another holder says nothing about this map's contents:
        // the only mutation is one `entry().or_default()`, so the worst a
        // poisoned lock can mean is that the entry did or did not appear.
        // Recovering keeps the "at most one recovery" guarantee; propagating
        // would turn it into a panic on every later build.
        Err(poisoned) => poisoned.into_inner(),
    };
    // The guard is a `std::sync::MutexGuard` and is dropped at the end of this
    // *synchronous* function; the `.await` happens on the returned `Arc`, so
    // nothing holds a `!Send` guard across a suspension point.
    Arc::clone(gates.entry(key).or_default())
}

/// Recover the pipeline runs a killed process left in flight on this database
/// — once per database per process, as Python does at API startup
/// (`recover_stale_pipeline_runs_on_startup`).
///
/// A run killed mid-flight (SIGKILL, OOM, an Android process kill) leaves its
/// `STARTED` row and whatever it had already written into the graph and vector
/// stores, recorded in the ownership ledger against its `pipeline_run_id`.
/// Recovery rolls those back and closes the run with an `ERRORED` row — but
/// only for a run whose ownership marker it can claim, which proves the writer
/// gone (see `cognee_delete::recover_stale_pipeline_runs` for the conditions).
/// It is therefore safe with any number of processes on the database, file-backed
/// SQLite and shared Postgres included: a live peer holds its marker's OS lock.
///
/// Nothing is gated on the outcome. A dataset is never refused because a row
/// says `STARTED`, so a run that cannot be recovered costs a stale status, not
/// a wedged dataset.
///
/// Best-effort: every failure is logged and ignored, because refusing to build
/// the SDK over a recovery convenience would be worse than the stale row.
async fn recover_abandoned_runs(
    cm: &ComponentManager,
    pipeline_run_repo: &Arc<dyn PipelineRunRepository>,
    database: &Arc<dyn RelationalDb>,
    graph_db: &Arc<dyn GraphDBTrait>,
    vector_db: &Arc<dyn VectorDB>,
) {
    let (relational_db_url, ownership_dir, min_age) = {
        // Snapshot under the read guard and drop it before the `.await`:
        // `RwLockReadGuard` is `!Send`, and this future is awaited from PyO3
        // bindings that require `Send`.
        let settings = cm.settings();
        (
            settings.resolved_relational_db_url(),
            settings.run_ownership_dir(),
            settings.stale_run_recovery_min_age(),
        )
    };

    // Every builder awaits this gate, not just the first, so a second builder
    // never hands its caller a working SDK while the first is still recovering.
    let gate = recovery_gate(&relational_db_url);
    gate.get_or_init(|| async {
        let report = cognee::delete::recover_stale_pipeline_runs(
            pipeline_run_repo.as_ref(),
            Arc::clone(database),
            Arc::clone(graph_db),
            Arc::clone(vector_db),
            &DatasetLocks::global(),
            &ownership_dir,
            min_age,
        )
        .await;
        if report.recovered > 0 || report.failed > 0 {
            tracing::warn!(
                recovered = report.recovered,
                failed = report.failed,
                graph_nodes_deleted = report.graph_nodes_deleted,
                vector_points_deleted = report.vector_points_deleted,
                "recovered pipeline runs a previous process left in flight"
            );
        }
        true
    })
    .await;
}

impl CogneeServices {
    /// Build the full bundle from a `ComponentManager`, returning the bundle and
    /// the resolved owner id.
    ///
    /// Owner id is the OSS default user materialised by
    /// `get_or_create_default_user(&settings)`: it is the parsed
    /// `settings.default_user_id` UUID. A downstream build may replace this
    /// helper with a DB-backed equivalent that upserts a row in the `users`
    /// table; the call shape is identical, so this assembly path is unchanged.
    ///
    /// A missing `llm_api_key` does not fail the build: the services get an
    /// LLM whose every call fails with "LLM API key is not set.".
    pub async fn build(cm: &Arc<ComponentManager>) -> Result<(Self, Uuid), SdkError> {
        // --- 1. Raw engines (errors map to ComponentError → SdkError). ---
        let storage = cm.storage().await?;
        let database = cm.database().await?;
        let graph_db = cm.graph_db().await?;
        let vector_db = cm.vector_db().await?;
        let embedding_engine = cm.embedding_engine().await?;
        let llm = cm.llm().await?;

        // --- 2. Resolve owner id (Python default-user semantics). ---
        // Snapshot the email under the read guard, then drop the guard
        // before the `.await` — `RwLockReadGuard` from `std::sync` is
        // `!Send`, and `CogneeServices::build` is awaited from PyO3
        // bindings that require `Send` futures.
        //
        // owner_id = uuid5(NAMESPACE_OID, email) — must match Python.
        let default_user_email = {
            let settings = cm.settings();
            settings.default_user_email.clone()
        };
        let user = get_or_create_default_user(&default_user_email)
            .await
            .map_err(|e| SdkError::UserBootstrap(e.to_string()))?;
        let owner_id = user.id;

        // --- 3. Derived services (mirrors the CLI command builders). ---
        let thread_pool = Arc::new(
            RayonThreadPool::with_default_threads()
                .map_err(|e| SdkError::ServiceBuild(format!("thread pool: {e}")))?,
        );

        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::clone(&database).pipeline_runs();

        recover_abandoned_runs(cm, &pipeline_run_repo, &database, &graph_db, &vector_db).await;

        // Taken out of the guard here: the builder chain below awaits.
        let run_ownership_dir = cm.settings().run_ownership_dir();
        let add_pipeline = Arc::new(
            AddPipeline::new(
                Arc::clone(&storage),
                Arc::clone(&database) as Arc<dyn IngestDb>,
            )
            .with_thread_pool(Arc::clone(&thread_pool) as Arc<dyn CpuPool>)
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_database(Arc::clone(&database))
            .with_pipeline_run_repo(Arc::clone(&pipeline_run_repo))
            // One lock registry for pipeline runs, `add` and `delete`, as in
            // Python (`infrastructure/locks/dataset_lock.py`).
            .with_dataset_locks(DatasetLocks::global())
            // Every Python run holds an ownership marker (`run_tasks.py:96`),
            // `add` included, so recovery can close an abandoned one.
            .with_run_ownership_dir(run_ownership_dir)
            // Python runs its image and audio loaders at `add`.
            .with_loader_registry(cm.add_loader_registry().await),
        );

        // Unauthorized DeleteService; the ACL-enforcing wrapper is a later-phase
        // concern.
        let mut delete_service = DeleteService::new(
            Arc::clone(&storage),
            Arc::clone(&database) as Arc<dyn DeleteDb>,
        )
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_dataset_locks(DatasetLocks::global());
        // Per-dataset storage (`ENABLE_BACKEND_ACCESS_CONTROL`): deleting a
        // dataset drops its own databases.
        if let Some(dataset_storage) = cm
            .dataset_storage()
            .await
            .map_err(|e| SdkError::ServiceBuild(format!("dataset storage: {e}")))?
        {
            delete_service = delete_service.with_dataset_storage(dataset_storage);
        }
        let delete_service = Arc::new(delete_service);

        // Session: the `session.store` factory (default `relational`).
        let session_store = cm
            .session_store()
            .await
            .map_err(|e| SdkError::ServiceBuild(format!("session store: {e}")))?;
        let feedback_llm_timeout = cm.settings().feedback_llm_timeout();
        let session_manager = Arc::new(
            SessionManager::new(Arc::clone(&session_store))
                .with_feedback_llm_timeout(feedback_llm_timeout),
        );

        let search_tuning = cm.settings().search_tuning();
        // Per-dataset storage: every dataset is searched in its own databases.
        let per_dataset_storage = cm
            .dataset_storage()
            .await
            .map_err(|e| SdkError::ServiceBuild(format!("dataset storage: {e}")))?
            .is_some();
        let search_orchestrator = Arc::new(
            SearchBuilder::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                Arc::clone(&database) as Arc<dyn SearchHistoryDb>,
            )
            .with_tuning(search_tuning)
            .with_session_manager(Arc::clone(&session_manager))
            .with_dataset_resolver(Arc::clone(&database) as Arc<dyn IngestDb>)
            .with_search_history_logging(cm.settings().log_search_history)
            .with_per_dataset_storage(per_dataset_storage)
            .build(),
        );

        // Ontology: the `ontology.resolver` factory when a path is configured,
        // else the no-op resolver.
        let ontology_resolver = cm
            .ontology_resolver(None)
            .await
            .map_err(|e| SdkError::ServiceBuild(format!("ontology resolver: {e}")))?;

        // CognifyConfig from Settings.
        let cognify_config = cm.settings().cognify_config();

        let checkpoint_store: Arc<dyn CheckpointStore> = Arc::clone(&database).checkpoints();

        let services = CogneeServices {
            storage,
            database,
            graph_db,
            vector_db,
            embedding_engine,
            llm,
            thread_pool,
            pipeline_run_repo,
            add_pipeline,
            delete_service,
            search_orchestrator,
            session_store,
            session_manager,
            ontology_resolver,
            cognify_config,
            checkpoint_store,
            component_manager: Arc::clone(cm),
        };

        Ok((services, owner_id))
    }

    /// The thread pool as the `dyn CpuPool` some APIs (e.g. cognify) require.
    #[allow(dead_code)] // used by cognify in later phases
    pub fn cpu_pool(&self) -> Arc<dyn CpuPool> {
        Arc::clone(&self.thread_pool) as Arc<dyn CpuPool>
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// F4: a second builder must **wait** for the first one's startup recovery,
    /// not overtake it.
    ///
    /// The gate used to be a synchronous "have I seen this database?" set: the
    /// first caller got `true` and started recovering, every later caller got
    /// `false` and returned immediately. Recovery is asynchronous, so a second
    /// builder could hand its caller a working SDK, that caller could start a
    /// cognify, and the first builder's still-running recovery would then
    /// retire that live run. When recovery only dropped a claim that was a
    /// millisecond-wide annoyance; now that it also deletes the run's graph
    /// and vector artifacts it is data loss.
    ///
    /// Two things are asserted, and the second is the one that used to fail:
    /// the late caller does not run recovery itself (the initialiser panics if
    /// it is ever called), and it does not return until the first caller's
    /// recovery has actually finished.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_builder_waits_for_the_first_recovery_instead_of_overtaking_it() {
        // Unique to this test: the gate map is process-wide, so a URL shared
        // with another test would couple them.
        let url = "sqlite:./f4-late-builder-waits.db";

        let recovery_finished = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();

        let first = {
            let recovery_finished = Arc::clone(&recovery_finished);
            let gate = recovery_gate(url);
            tokio::spawn(async move {
                gate.get_or_init(|| async move {
                    entered_tx.send(()).expect("nobody drops the receiver");
                    // Stands in for the rollback + close sequence, which is
                    // many round-trips to the database.
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    recovery_finished.store(true, Ordering::SeqCst);
                    true
                })
                .await;
            })
        };

        entered_rx
            .await
            .expect("the first caller must reach the gate");
        assert!(
            !recovery_finished.load(Ordering::SeqCst),
            "premise: the first caller's recovery is still in flight"
        );

        // The late builder. Same URL, so the same gate.
        let recovered = *recovery_gate(url)
            .get_or_init(|| async {
                panic!("a late builder must not run a second recovery of the same database")
            })
            .await;

        assert!(
            recovery_finished.load(Ordering::SeqCst),
            "the late builder returned while the first caller's recovery was still running — \
             its caller could now start a run while the recovery was still running"
        );
        assert!(
            recovered,
            "it must also see what the first caller resolved, not re-derive it"
        );

        first.await.expect("the first task must not panic");
    }

    /// The gate is per database, not per process: two URLs must not share one.
    ///
    /// A single process can build services against more than one relational
    /// database (a test harness, an embedder switching tenants), and one flag
    /// for all of them would recover the first and silently skip the rest.
    #[tokio::test]
    async fn each_database_gets_its_own_gate() {
        let a = "sqlite:./f4-gate-identity-a.db";
        let b = "sqlite:./f4-gate-identity-b.db";

        assert!(
            Arc::ptr_eq(&recovery_gate(a), &recovery_gate(a)),
            "the same URL must resolve to the same gate, or nothing is deduplicated"
        );
        assert!(
            !Arc::ptr_eq(&recovery_gate(a), &recovery_gate(b)),
            "two databases must not share a gate"
        );

        recovery_gate(a).get_or_init(|| async { true }).await;
        assert!(
            !recovery_gate(b).initialized(),
            "recovering one database must not mark another as recovered"
        );
    }
}
