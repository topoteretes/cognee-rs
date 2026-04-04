use std::sync::Arc;

use cognee_lib::add::AddPipeline;
use cognee_lib::models::DataInput;
use cognee_lib::{ComponentManager, PipelineContext};
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

        let pipeline = AddPipeline::new(storage, database);

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
