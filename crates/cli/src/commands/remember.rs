use std::sync::Arc;

use cognee::add::AddPipeline;
use cognee::api::remember;
use cognee::core::RayonThreadPool;
use cognee::models::DataInput;
use cognee::ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee::search::SessionManager;
use cognee::session::SessionStore;
use cognee::{ComponentManager, PipelineContext};
use tracing::info;
use uuid::Uuid;

use crate::cli::RememberArgs;
use crate::error::CliError;

pub fn run(args: RememberArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    // `--dataset-name`, else the `default_dataset_name` setting (`main_dataset`
    // when it is blank).
    let dataset_name = args
        .dataset_name
        .clone()
        .unwrap_or_else(|| cm.settings().effective_default_dataset_name().to_owned());
    let settings = cm.settings();
    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;
    drop(settings);

    let tenant_id = super::parse_tenant_id(args.tenant_id.as_deref())?;

    // `--no-improve` flips the default-on self-improvement (memify) pass.
    let self_improvement = !args.no_improve;

    let inputs = args
        .data
        .into_iter()
        .map(DataInput::from_string)
        .collect::<Vec<_>>();

    crate::teardown::run_command(Arc::clone(&cm), async {
        // `graph_extraction.backend`, resolved before any side effect: an
        // unregistered kind is an error, never a silent LLM-extractor run.
        let mut cognify_config = cm.settings().cognify_config();
        if let Some(backend) = cm.configured_graph_extractor().await.map_err(|error| {
            CliError::Runtime(format!("Graph extractor initialization failed: {error}"))
        })? {
            cognify_config = cognify_config.with_graph_backend(backend);
        }

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
        let add_pipeline = Arc::new(cm.attach_transcriber(add_pipeline).await);

        // Session backing store/manager are only required by session-mode
        // remember (when `--session-id` is set); building them eagerly is cheap
        // and harmless in permanent mode.
        let session_store: Arc<dyn SessionStore> = cm
            .session_store()
            .await
            .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?;
        let feedback_llm_timeout = cm.settings().feedback_llm_timeout();
        let session_manager = Arc::new(
            SessionManager::new(Arc::clone(&session_store))
                .with_feedback_llm_timeout(feedback_llm_timeout),
        );

        let ontology_resolver: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());
        let cognify_config = Arc::new(cognify_config);

        let result = remember(
            inputs,
            &dataset_name,
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
