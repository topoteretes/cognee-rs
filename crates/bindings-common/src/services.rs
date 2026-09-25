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
use cognee::cognify::{ChunkStrategy, CognifyConfig};
use cognee::core::{CpuPool, RayonThreadPool};
use cognee::database::{
    CheckpointStore, DatabaseConnection, DeleteDb, IngestDb, PipelineRunRepository,
    SeaOrmCheckpointStore, SeaOrmPipelineRunRepository, SearchHistoryDb,
};
use cognee::delete::DeleteService;
use cognee::embedding::EmbeddingEngine;
use cognee::graph::GraphDBTrait;
use cognee::llm::Llm;
use cognee::ontology::{NoOpOntologyResolver, OntologyResolver, RdfLibOntologyResolver};
use cognee::search::{
    SeaOrmSessionStore, SearchBuilder, SearchOrchestrator, SessionManager, SessionStore,
};
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
    /// Concrete SeaORM connection. `DatabaseConnection` implements every DB
    /// trait, so derived services coerce it via `Arc::clone(&database) as Arc<dyn …>`.
    pub database: Arc<DatabaseConnection>,
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
}

/// Startup-recovery gates, one per relational database this process has
/// touched, keyed by a hash of the resolved URL. The `bool` a gate resolves to
/// is "this process actually ran recovery on that database".
///
/// Per database, not a single process-wide flag: one process can build
/// services against more than one relational URL (a test harness, an embedder
/// switching tenants), and a bare flag would recover the first and silently
/// skip every other. Per *process* and not per handle or per config version,
/// because `CogneeServices::build` runs again on every config-version change,
/// and by then a cognify started through an earlier handle may hold a claim
/// that is very much alive. Sweeping then would drop it, admit a second run
/// into the same dataset, and — now that recovery also rolls artifacts back —
/// delete that live run's nodes out from under it. The only moment at which
/// every leftover in a database is provably dead is the first time this
/// process touches it.
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
    // would skip a recovery rather than perform an extra one — the
    // conservative direction, and the same outcome as not asserting
    // single-process.
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

/// Clear what a killed run left behind on this database — once per database
/// per process, and only where the deployment asserts one process per
/// relational database.
///
/// A run killed mid-flight (SIGKILL, OOM, an Android process kill) leaves
/// **three** things behind. Two of them wedge the dataset, and clearing one of
/// those alone is worth nothing:
///
/// 1. Whatever the run had already written into the graph and vector stores,
///    recorded in the ownership ledger against its `pipeline_run_id`. A run
///    that fails while its process is alive rolls this back through
///    `cognify::rollback::on_run_failed`; a killed one never reaches that
///    code, so the artifacts survive attributed to a run that will never
///    finish, and no later sweep will ever select them again.
/// 2. The `pipeline_runs` row left at `Initiated`/`Started`.
///    `check_pipeline_run_qualification` reads it *first*, before any claim is
///    consulted, and returns `AlreadyRunning`. It never expires. Until this
///    landed, the only sweep that retired it ran at HTTP-server startup, so an
///    embedded consumer never reached it at all.
/// 3. The exclusive-run claim. Released only by its holder
///    (`release_pipeline_run_claim` filters on the `claim_id` that died with
///    it), so liveness is inferred from age against a day-long window.
///
/// The order is the point: roll back first, retire second, unclaim last. Step
/// 2 is what makes the dataset runnable again, so doing it before step 1 opens
/// a window in which a new run starts while the rollback is still deleting the
/// dead run's nodes. Python orders it the same way — `cognify_rollback_handler`
/// runs before the status reset in `modules/cognify/recovery.py`.
///
/// `cognee-cli pipeline-unblock` clears gates 2 and 3 for the same reason, and
/// says so in its own module docs. An embedded consumer has neither that CLI
/// nor an HTTP server.
///
/// Safety: the two clears are unscoped and the rollback deletes graph data, so
/// all three are sound **only** where no peer process can hold what they drop.
/// With one process per database, every leftover present the first time this
/// process touches it was written by a dead incarnation of this same process.
/// Anything else — a shared Postgres, or the *file-backed* SQLite that is the
/// shipped default — derives `false` and is left entirely alone.
/// `COGNEE_SINGLE_PROCESS` is how a deployment that does own its file (one
/// process per device, say) opts in; the SDK cannot infer that from the URL.
///
/// The steps are attempted independently: a transient failure on one must not
/// suppress the others, since the dataset stays wedged unless both gates go.
/// All are best-effort — a failure is logged and ignored, because refusing to
/// build the SDK over a recovery convenience would turn a recoverable wedge
/// into a hard startup failure.
async fn sweep_killed_run_leftovers(
    cm: &ComponentManager,
    pipeline_run_repo: &Arc<dyn PipelineRunRepository>,
    database: &Arc<DatabaseConnection>,
    graph_db: &Arc<dyn GraphDBTrait>,
    vector_db: &Arc<dyn VectorDB>,
) {
    // Snapshot under the read guard and drop it before the `.await`:
    // `RwLockReadGuard` is `!Send`, and this future is awaited from PyO3
    // bindings that require `Send`.
    let (relational_db_url, single_process) = {
        let settings = cm.settings();
        (
            settings.resolved_relational_db_url(),
            settings.resolved_single_process(),
        )
    };

    // Every builder awaits this, not just the first — see `recovery_gate` for
    // the race that returning early used to open. The assertion is consulted
    // *inside* the gate so a database is marked considered either way:
    // resolving it outside would let a first build that answered `false` leave
    // the gate open for a later build to sweep with this process's own runs
    // already in flight.
    let gate = recovery_gate(&relational_db_url);
    let recovered = *gate
        .get_or_init(|| async {
            if !single_process {
                return false;
            }

            let rollback = cognee::delete::sweep_orphaned_run_artifacts(
                pipeline_run_repo.as_ref(),
                Arc::clone(database),
                Arc::clone(graph_db),
                Arc::clone(vector_db),
            )
            .await;
            if rollback.runs_found > 0 {
                tracing::warn!(
                    runs_found = rollback.runs_found,
                    runs_swept = rollback.runs_swept,
                    runs_failed = rollback.runs_failed,
                    graph_nodes_deleted = rollback.graph_nodes_deleted,
                    vector_points_deleted = rollback.vector_points_deleted,
                    "rolled back the artifacts of pipeline runs a previous process left in \
                     flight"
                );
            }

            match pipeline_run_repo
                .reset_orphans("sdk_startup_orphan_single_process")
                .await
            {
                Ok(0) => {}
                Ok(reset) => tracing::warn!(
                    reset,
                    "retired pipeline-run rows left in flight by a previous process; the \
                     datasets they blocked are runnable again"
                ),
                Err(e) => tracing::warn!(
                    "startup reset of orphaned pipeline runs failed (non-fatal); a dataset \
                     wedged by a killed run stays wedged, and this gate never expires: {e}"
                ),
            }

            match pipeline_run_repo
                .release_all_pipeline_run_claims("sdk_startup_sweep_single_process")
                .await
            {
                Ok(0) => {}
                Ok(released) => tracing::warn!(
                    released,
                    "released pipeline-run claims left behind by a previous process"
                ),
                Err(e) => tracing::warn!(
                    "startup sweep of pipeline-run claims failed (non-fatal); a dataset wedged \
                     by a killed run stays wedged until its claim ages out: {e}"
                ),
            }

            true
        })
        .await;

    // The setting is read once per database, on the first build that reaches
    // it — and `CogneeServices::build` is lazy, so "the first build" is the
    // first *operation*, not construction of the handle. Turning the assertion
    // on afterwards therefore does nothing for the rest of the process, and
    // silence about that reads as a recovery that ran and found nothing.
    if single_process && !recovered {
        tracing::warn!(
            "single_process is asserted, but this process had already opened its relational \
             database without it, so startup recovery was skipped and will not run again for \
             the lifetime of this process. Set single_process (or COGNEE_SINGLE_PROCESS) \
             before the first operation; restart to recover a dataset wedged by a killed run."
        );
    }
}

impl CogneeServices {
    /// Build the full bundle from a `ComponentManager`, returning the bundle and
    /// the resolved owner id.
    ///
    /// Owner id is the OSS default user materialised by
    /// `get_or_create_default_user(&settings)`: it is the parsed
    /// `settings.default_user_id` UUID. The closed cloud build replaces this
    /// helper with a DB-backed equivalent that upserts a row in the `users`
    /// table; the call shape is identical, so this assembly path is unchanged.
    ///
    /// The LLM is resolved **strictly** here (the simplest correct v1 per the
    /// plan): callers that need keyless warm must set a non-empty dummy
    /// `llm_api_key` — `OpenAIAdapter::new` performs no network I/O at
    /// construction, so this never reaches the network.
    pub async fn build(cm: &ComponentManager) -> Result<(Self, Uuid), SdkError> {
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
            Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

        sweep_killed_run_leftovers(cm, &pipeline_run_repo, &database, &graph_db, &vector_db).await;

        let add_pipeline = Arc::new(
            AddPipeline::new(
                Arc::clone(&storage),
                Arc::clone(&database) as Arc<dyn IngestDb>,
            )
            .with_thread_pool(Arc::clone(&thread_pool) as Arc<dyn CpuPool>)
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_database(Arc::clone(&database))
            .with_pipeline_run_repo(Arc::clone(&pipeline_run_repo)),
        );

        // Unauthorized DeleteService; the ACL-enforcing wrapper is a later-phase
        // concern.
        let delete_service = Arc::new(
            DeleteService::new(
                Arc::clone(&storage),
                Arc::clone(&database) as Arc<dyn DeleteDb>,
            )
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_pipeline_run_repo(Arc::clone(&pipeline_run_repo)),
        );

        // Session: v1 is always SeaOrmSessionStore (fs/redis features are not
        // built into the default binding configurations).
        let session_store_concrete = SeaOrmSessionStore::new(Arc::clone(&database))
            .await
            .map_err(|e| SdkError::ServiceBuild(format!("session store: {e}")))?;
        let session_store: Arc<dyn SessionStore> = Arc::new(session_store_concrete);
        let session_manager = Arc::new(SessionManager::new(Arc::clone(&session_store)));

        let search_orchestrator = Arc::new(
            SearchBuilder::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                Arc::clone(&database) as Arc<dyn SearchHistoryDb>,
            )
            .with_session_manager(Arc::clone(&session_manager))
            .with_dataset_resolver(Arc::clone(&database) as Arc<dyn IngestDb>)
            .build(),
        );

        // Ontology: RdfLib when a path is configured, else NoOp.
        let ontology_resolver: Arc<dyn OntologyResolver> = {
            let path = cm.settings().ontology_file_path.clone();
            if path.trim().is_empty() {
                Arc::new(NoOpOntologyResolver::new())
            } else {
                Arc::new(
                    RdfLibOntologyResolver::new(path.as_str())
                        .map_err(|e| SdkError::ServiceBuild(format!("ontology resolver: {e}")))?,
                )
            }
        };

        // CognifyConfig from Settings. `with_temporal_cognify` is a per-call
        // flag (not a Settings field) and is left at default here.
        let cognify_config = {
            let s = cm.settings();
            let chunk_strategy = match s.chunk_strategy.to_uppercase().as_str() {
                "RECURSIVE" => ChunkStrategy::Recursive,
                _ => ChunkStrategy::Paragraph,
            };
            CognifyConfig::default()
                .with_chunk_size_opt(s.chunk_size.map(|n| n as usize))
                .with_chunk_overlap(s.chunk_overlap as usize)
                .with_chunk_strategy(chunk_strategy)
                .with_max_parallel_extractions(s.llm_max_parallel_requests.max(1) as usize)
        };

        let checkpoint_store: Arc<dyn CheckpointStore> =
            Arc::new(SeaOrmCheckpointStore::new(Arc::clone(&database)));

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
                    // Stands in for the rollback + reset + unclaim sequence,
                    // which is many round-trips to the database.
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
             its caller could now start a run that the recovery would retire"
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
