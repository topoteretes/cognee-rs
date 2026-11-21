use std::sync::Arc;

use cognee::add::AddPipeline;
use cognee::api::{
    RememberCodeParams, RememberContentType, RememberOptionCheck, remember, remember_code,
    validate_remember_options,
};
use cognee::core::RayonThreadPool;
use cognee::models::DataInput;
use cognee::ontology::{NoOpOntologyResolver, OntologyResolver};
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

    // As in Python: a session-scoped remember never cognifies, so an explicit
    // extractor there is a mistake, not a no-op.
    if args.session_id.is_some() && args.extractor.is_some() {
        return Err(CliError::Validation(
            "--extractor is not supported with --session-id".to_string(),
        ));
    }

    let content_type = validate_remember_options(RememberOptionCheck {
        content_type: args.content_type.as_deref(),
        session_id: args.session_id.as_deref(),
        ..RememberOptionCheck::default()
    })
    .map_err(|error| CliError::Validation(error.to_string()))?;
    if content_type == Some(RememberContentType::Code) {
        if args.extractor.is_some() {
            // The code route runs enola only: no graph extractor to choose.
            return Err(CliError::Validation(
                "--extractor is not supported with --content-type code".to_string(),
            ));
        }
        return run_code(args.data, dataset_name, owner_id, tenant_id, cm);
    }

    // `--no-improve` flips the default-on self-improvement (`improve`) pass.
    let self_improvement = !args.no_improve;

    let inputs = args
        .data
        .into_iter()
        .map(DataInput::from_string)
        .collect::<Vec<_>>();

    crate::teardown::run_command(Arc::clone(&cm), async {
        // `graph_extraction.backend`, resolved before any side effect: an
        // unregistered kind is an error, never a silent LLM-extractor run. A
        // session-scoped remember does not cognify, so it resolves none.
        let mut cognify_config = cm.settings().cognify_config();
        if args.session_id.is_none()
            && let Some(backend) = cm
                .graph_extractor(
                    args.extractor.as_deref(),
                    &serde_json::Value::String(String::new()),
                )
                .await
                .map_err(|error| {
                    CliError::Runtime(format!("Graph extractor initialization failed: {error}"))
                })?
        {
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

        let add_pipeline = Arc::new(
            AddPipeline::new(
                Arc::clone(&storage),
                Arc::clone(&database) as Arc<dyn cognee::database::IngestDb>,
            )
            .with_thread_pool(thread_pool)
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_database(Arc::clone(&database))
            .with_repo_sources(cm.settings().repo_sources())
            // Python runs its image and audio loaders at `add`.
            .with_loader_registry(cm.add_loader_registry().await),
        );

        // Session backing store/manager are only required by session-mode
        // remember (when `--session-id` is set); building them eagerly is cheap
        // and harmless in permanent mode.
        let session_store: Arc<dyn SessionStore> = cm
            .session_store()
            .await
            .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?;
        let session_manager = Arc::new(cm.settings().session_manager(Arc::clone(&session_store)));

        let ontology_resolver: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());
        let cognify_config = Arc::new(cognify_config);

        let (improve_config, personalization, auto_feedback) = {
            let settings = cm.settings();
            (
                settings.auto_improve_config(),
                settings.preference_config(),
                settings.auto_feedback_enabled(),
            )
        };
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
            ontology_resolver,
            cognify_config,
            improve_config,
            personalization,
            auto_feedback,
        )
        .await
        .map_err(|error| CliError::Runtime(format!("Remember failed: {error}")))?;

        info!("{result}");

        Ok(())
    })
}

/// `remember --content-type code`: index each path, a local code repository,
/// as a code graph. Keyless: no LLM, embedding engine or graph extractor is
/// built.
fn run_code(
    data: Vec<String>,
    dataset_name: String,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    cm: Arc<ComponentManager>,
) -> Result<(), CliError> {
    let inputs = data.into_iter().map(DataInput::FilePath).collect();
    crate::teardown::run_command(Arc::clone(&cm), async {
        let runtime = |e: &dyn std::fmt::Display| CliError::Runtime(format!("{e}"));
        let storage = cm.storage().await.map_err(|e| runtime(&e))?;
        let database = cm.database().await.map_err(|e| runtime(&e))?;
        let graph_db = cm.graph_db().await.map_err(|e| runtime(&e))?;
        let vector_db = cm.vector_db().await.map_err(|e| runtime(&e))?;
        let thread_pool = Arc::new(
            RayonThreadPool::with_default_threads()
                .map_err(|e| CliError::Runtime(format!("Failed to build thread pool: {e}")))?,
        );
        let add_pipeline = Arc::new(
            AddPipeline::new(
                Arc::clone(&storage),
                Arc::clone(&database) as Arc<dyn cognee::database::IngestDb>,
            )
            .with_thread_pool(thread_pool)
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_database(Arc::clone(&database))
            // Remote repositories are cloned under `code_graph.clones_dir`.
            .with_repo_sources(cm.settings().repo_sources()),
        );
        let cognify_config = Arc::new(cm.settings().cognify_config());
        let result = remember_code(RememberCodeParams {
            data: inputs,
            dataset_name,
            owner_id,
            tenant_id,
            dataset_id: None,
            node_set: None,
            repo_credentials: None,
            raise_on_error: true,
            add_pipeline,
            storage,
            graph_db,
            vector_db,
            db: database,
            cognify_config,
        })
        .await
        .map_err(|error| CliError::Runtime(format!("Remember failed: {error}")))?;

        info!("{result}");
        for item in &result.items {
            info!(
                "code repository {} -> data id {}, pipeline run {}",
                item.path.as_deref().unwrap_or_default(),
                item.id.map(|id| id.to_string()).unwrap_or_default(),
                item.pipeline_run_id
                    .map(|id| id.to_string())
                    .unwrap_or_default()
            );
        }

        Ok(())
    })
}
