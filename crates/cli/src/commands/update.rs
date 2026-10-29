use std::sync::Arc;

use cognee::add::AddPipeline;
use cognee::api::update;
use cognee::core::RayonThreadPool;
use cognee::database::IngestDb;
use cognee::models::DataInput;
use cognee::ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee::{ComponentManager, PipelineContext};
use tracing::info;
use uuid::Uuid;

use crate::cli::UpdateArgs;
use crate::error::CliError;

/// `cognee-cli update`: replace one document of a dataset, keeping its data id
/// (Python's `cognee.update()`; the library call is `cognee::api::update`).
pub fn run(args: UpdateArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
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
    let data_id = Uuid::parse_str(&args.data_id)
        .map_err(|error| CliError::Validation(format!("Invalid --data-id: {error}")))?;
    let node_set = (!args.node_set.is_empty()).then(|| args.node_set.clone());
    let new_data = vec![DataInput::from_string(args.data.clone())];

    crate::teardown::run_command(Arc::clone(&cm), async {
        // `graph_extraction.backend`, resolved before any side effect.
        let mut cognify_config = cm.settings().cognify_config();
        if let Some(backend) = cm
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
        let add_pipeline = AddPipeline::new(
            Arc::clone(&storage),
            Arc::clone(&database) as Arc<dyn IngestDb>,
        )
        .with_thread_pool(thread_pool)
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database))
        .with_loader_registry(cm.add_loader_registry().await);
        let delete_service = super::build_delete_service(&cm).await?;

        // The dataset is named on the command line; update() works by id.
        let dataset = IngestDb::get_dataset_by_name_in_tenant(
            database.as_ref(),
            &dataset_name,
            owner_id,
            tenant_id,
        )
        .await
        .map_err(|e| CliError::Runtime(format!("Failed to resolve dataset: {e}")))?
        .ok_or_else(|| CliError::Validation(format!("Dataset '{dataset_name}' was not found")))?;

        let ontology_resolver: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());
        let result = update(
            data_id,
            new_data,
            dataset.id,
            &dataset_name,
            owner_id,
            tenant_id,
            node_set,
            None,
            true,
            None,
            &delete_service,
            &add_pipeline,
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            Some(database),
            ontology_resolver,
            &cognify_config,
        )
        .await
        .map_err(|error| CliError::Runtime(format!("Update failed: {error}")))?;

        info!(data_id = %result.data_id, "update completed");
        Ok(())
    })
}
