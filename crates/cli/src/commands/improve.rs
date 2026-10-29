use std::sync::Arc;

use cognee::add::AddPipeline;
use cognee::api::{ImproveParams, improve};
use cognee::core::RayonThreadPool;
use cognee::ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee::search::SessionManager;
use cognee::session::SessionStore;
use cognee::{ComponentManager, PipelineContext};
use tracing::info;
use uuid::Uuid;

use crate::cli::ImproveArgs;
use crate::error::CliError;

pub fn run(args: ImproveArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
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
    // Fail before any work on a bad `improve.*` value (an
    // IMPROVE_STAGES_DISABLED typo, an out-of-range alpha).
    let improve_config = settings
        .improve_config()
        .map_err(|error| CliError::Validation(error.to_string()))?;
    let personalization = settings.preference_config();
    let auto_feedback = settings.auto_feedback_enabled();
    drop(settings);

    let tenant_id = super::parse_tenant_id(args.tenant_id.as_deref())?;

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

        let session_store: Arc<dyn SessionStore> = cm
            .session_store()
            .await
            .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?;
        let feedback_llm_timeout = cm.settings().feedback_llm_timeout();
        let session_manager = Arc::new(
            SessionManager::new(Arc::clone(&session_store))
                .with_feedback_llm_timeout(feedback_llm_timeout),
        );
        let checkpoint_store = Arc::clone(&database).checkpoints();

        let ontology_resolver: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());

        let result = improve(ImproveParams {
            dataset_name: dataset_name.clone(),
            dataset_id: None,
            session_ids,
            node_name,
            owner_id,
            tenant_id,
            feedback_alpha: args.feedback_alpha,
            config: improve_config,
            personalization,
            auto_feedback,
            extraction_tasks: None,
            enrichment_tasks: None,
            data: None,
            build_global_context_index: false,
            build_truth_subspace: false,
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
            // The add pipeline's registry (none here), else the process-wide one.
            dataset_locks: None,
            cognify_config: &cognify_config,
        })
        .await;
        let result = match result {
            Ok(result) => result,
            // A fatal-stage abort still reports what ran before it, as
            // Python's raised error carries `improve_result`.
            Err(cognee::api::ApiError::ImproveAborted { result, .. }) => {
                log_stages(&result);
                let error = result.error.as_deref().unwrap_or("a fatal stage errored");
                return Err(CliError::Runtime(format!(
                    "Improve failed: {error} (status {})",
                    result.status().as_str()
                )));
            }
            Err(error) => return Err(CliError::Runtime(format!("Improve failed: {error}"))),
        };

        log_stages(&result);
        info!(status = result.status().as_str(), "improve completed");

        Ok(())
    })
}

/// One log line per stage of an improve run, in registry order.
fn log_stages(result: &cognee::api::ImproveResult) {
    for stage in &result.stages {
        info!(
            stage = %stage.stage,
            status = stage.status.as_str(),
            reason = stage.reason.as_deref().unwrap_or(""),
            error = stage.error.as_deref().unwrap_or(""),
            counts = ?stage.counts,
            duration_ms = stage.duration_ms,
            "improve stage"
        );
    }
}
