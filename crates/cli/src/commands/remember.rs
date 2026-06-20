use std::sync::Arc;

use cognee_lib::add::AddPipeline;
use cognee_lib::api::remember;
use cognee_lib::cognify::CognifyConfig;
use cognee_lib::core::RayonThreadPool;
use cognee_lib::models::DataInput;
use cognee_lib::ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_lib::search::{SeaOrmSessionStore, SessionManager};
use cognee_lib::session::SessionStore;
use cognee_lib::{ComponentManager, PipelineContext};
use tracing::info;
use uuid::Uuid;

use crate::cli::RememberArgs;
use crate::error::CliError;

pub fn run(args: RememberArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
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

    // `--no-improve` flips the default-on self-improvement (memify) pass.
    let self_improvement = !args.no_improve;

    let inputs = args
        .data
        .into_iter()
        .map(DataInput::from_string)
        .collect::<Vec<_>>();

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

        let add_pipeline = Arc::new(
            AddPipeline::new(
                Arc::clone(&storage),
                Arc::clone(&database) as Arc<dyn cognee_lib::database::IngestDb>,
            )
            .with_thread_pool(thread_pool)
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_database(Arc::clone(&database)),
        );

        // Session backing store/manager are only required by session-mode
        // remember (when `--session-id` is set); building them eagerly is cheap
        // and harmless in permanent mode.
        let session_store: Arc<dyn SessionStore> = Arc::new(
            SeaOrmSessionStore::new(Arc::clone(&database))
                .await
                .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?,
        );
        let session_manager = Arc::new(SessionManager::new(Arc::clone(&session_store)));

        let ontology_resolver: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());
        let cognify_config = Arc::new(CognifyConfig::default());

        let result = remember(
            inputs,
            &args.dataset_name,
            args.session_id.as_deref(),
            self_improvement,
            owner_id,
            tenant_id,
            add_pipeline,
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            Some(Arc::clone(&database)),
            Some(session_store),
            Some(session_manager),
            None,
            ontology_resolver,
            cognify_config,
        )
        .await
        .map_err(|error| CliError::Runtime(format!("Remember failed: {error}")))?;

        info!("{result}");

        Ok(())
    })
}
