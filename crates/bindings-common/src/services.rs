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
                .with_chunk_size(s.chunk_size as usize)
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
