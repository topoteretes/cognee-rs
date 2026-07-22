use std::sync::Arc;

use cognee::add::AddPipeline;
use cognee::api::{ImproveParams, improve};
use cognee::cognify::CognifyConfig;
use cognee::core::RayonThreadPool;
use cognee::database::SeaOrmCheckpointStore;
use cognee::ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee::search::{SeaOrmSessionStore, SessionManager};
use cognee::session::SessionStore;
use cognee::{ComponentManager, PipelineContext};
use tracing::info;
use uuid::Uuid;

use crate::cli::ImproveArgs;
use crate::error::CliError;

pub fn run(args: ImproveArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();
    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;
    drop(settings);

    let tenant_id = args
        .tenant_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|error| CliError::Validation(format!("Invalid --tenant-id: {error}")))?;

    let session_ids = if args.session_id.is_empty() {
        None
    } else {
        Some(args.session_id.clone())
    };
    let node_name = if args.node_name.is_empty() {
        None
    } else {
        Some(args.node_name.clone())
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async {
        let storage = cm
            .storage()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let database = cm
            .database()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let graph_db = cm
            .graph_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let vector_db = cm
            .vector_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let embedding_engine = cm
            .embedding_engine()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let llm = cm
            .llm()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let thread_pool = Arc::new(
            RayonThreadPool::with_default_threads()
                .map_err(|e| CliError::Runtime(format!("Failed to build thread pool: {e}")))?,
        );

        let add_pipeline = AddPipeline::new(
            Arc::clone(&storage),
            Arc::clone(&database) as Arc<dyn cognee::database::IngestDb>,
        )
        .with_thread_pool(thread_pool)
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

        let session_store: Arc<dyn SessionStore> = Arc::new(
            SeaOrmSessionStore::new(Arc::clone(&database))
                .await
                .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?,
        );
        let session_manager = Arc::new(SessionManager::new(Arc::clone(&session_store)));
        let checkpoint_store = Arc::new(SeaOrmCheckpointStore::new(Arc::clone(&database)));

        let ontology_resolver: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());
        let cognify_config = CognifyConfig::default();

        let result = improve(ImproveParams {
            dataset_name: args.dataset_name.clone(),
            session_ids,
            node_name,
            owner_id,
            tenant_id,
            feedback_alpha: args.feedback_alpha,
            extraction_tasks: None,
            enrichment_tasks: None,
            data: None,
            build_global_context_index: false,
            run_in_background: false,
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            ontology_resolver,
            db: Some(Arc::clone(&database)),
            session_store: Some(session_store),
            session_manager: Some(session_manager),
            add_pipeline: Some(&add_pipeline),
            checkpoint_store: Some(checkpoint_store),
            cognify_config: &cognify_config,
        })
        .await
        .map_err(|error| CliError::Runtime(format!("Improve failed: {error}")))?;

        info!(
            stages_run = ?result.stages_run,
            feedback_processed = result.feedback_entries_processed,
            feedback_applied = result.feedback_entries_applied,
            sessions_persisted = result.sessions_persisted,
            edges_synced = result.edges_synced,
            triplets = result.memify_result.as_ref().map(|m| m.triplet_count).unwrap_or(0),
            "improve completed"
        );

        Ok(())
    })
}
