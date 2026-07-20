use std::sync::Arc;

use cognee::add::AddPipeline;
use cognee::core::RayonThreadPool;
use cognee::database::{PipelineRunRepository, SeaOrmPipelineRunRepository};
use cognee::models::DataInput;
use cognee::{ComponentManager, PipelineContext};
use tracing::info;
use uuid::Uuid;

use crate::cli::AddArgs;
use crate::error::CliError;

pub fn run(args: AddArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let owner_id = Uuid::parse_str(&cm.settings().default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            cm.settings().default_user_id
        ))
    })?;

    let tenant_id = args
        .tenant_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|error| CliError::Validation(format!("Invalid --tenant-id: {error}")))?;

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
        let thread_pool = Arc::new(
            RayonThreadPool::with_default_threads()
                .map_err(|e| CliError::Runtime(format!("Failed to build thread pool: {e}")))?,
        );

        // Gap 08-07: persist the four-state `pipeline_runs` trail so CLI
        // add shows up in `/api/v1/activity/pipeline-runs`.
        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

        let pipeline = AddPipeline::new(
            storage,
            Arc::clone(&database) as Arc<dyn cognee::database::IngestDb>,
        )
        .with_thread_pool(thread_pool)
        .with_graph_db(graph_db)
        .with_vector_db(vector_db)
        .with_database(database)
        .with_pipeline_run_repo(pipeline_run_repo);

        let inputs = args
            .data
            .into_iter()
            .map(DataInput::from_string)
            .collect::<Vec<_>>();

        let results = pipeline
            .add(inputs, &args.dataset_name, owner_id, tenant_id)
            .await
            .map_err(|error| CliError::Runtime(format!("Add operation failed: {error}")))?;

        info!(
            "Success: Added {} item(s) to dataset '{}'.",
            results.len(),
            args.dataset_name
        );

        Ok(())
    })
}
