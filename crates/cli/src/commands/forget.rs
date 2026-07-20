use std::sync::Arc;

use cognee::api::{DatasetRef, ForgetTarget, forget};
use cognee::database::{DeleteDb, IngestDb, PipelineRunRepository, SeaOrmPipelineRunRepository};
use cognee::delete::DeleteService;
use cognee::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::ForgetArgs;
use crate::error::CliError;

pub fn run(args: ForgetArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    validate_target_selection(&args)?;

    let settings = cm.settings();
    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;
    drop(settings);

    let _tenant_id = args
        .tenant_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|error| CliError::Validation(format!("Invalid --tenant-id: {error}")))?;

    let data_id = args
        .data_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|error| CliError::Validation(format!("Invalid --data-id: {error}")))?;

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

        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::new(SeaOrmPipelineRunRepository::new(database.clone()));

        let delete_service = DeleteService::new(storage, database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(graph_db)
            .with_vector_db(vector_db)
            .with_pipeline_run_repo(pipeline_run_repo);

        // Build the ForgetTarget from the (already-validated) flags.
        let target = if args.all {
            ForgetTarget::All
        } else if let Some(data_id) = data_id {
            // --data-id requires a dataset to scope the deletion.
            let dataset_name = args.dataset_name.clone().ok_or_else(|| {
                CliError::Validation(
                    "--data-id requires --dataset-name to scope the deletion".to_string(),
                )
            })?;
            ForgetTarget::Item {
                data_id,
                dataset: DatasetRef::Name(dataset_name),
            }
        } else if let Some(dataset_name) = args.dataset_name.clone() {
            ForgetTarget::Dataset {
                dataset: DatasetRef::Name(dataset_name),
            }
        } else {
            // Unreachable: validate_target_selection guarantees one target.
            return Err(CliError::Validation(
                "Specify exactly one forget target: --dataset-name, --data-id, or --all"
                    .to_string(),
            ));
        };

        let result = forget(
            target,
            owner_id,
            &delete_service,
            Some(database.as_ref() as &dyn IngestDb),
        )
        .await
        .map_err(|error| CliError::Runtime(format!("Forget failed: {error}")))?;

        let dr = &result.delete_result;
        info!(
            target = %result.target,
            deleted_datasets = dr.deleted_datasets,
            deleted_data = dr.deleted_data,
            deleted_storage = dr.deleted_storage_files,
            deleted_graph_nodes = dr.deleted_graph_nodes,
            deleted_vector_points = dr.deleted_vector_points,
            "forget completed"
        );
        for w in &dr.warnings {
            warn!(warning = %w, "forget warning");
        }

        Ok(())
    })
}

/// Enforce that exactly one forget target was selected.
///
/// Valid combinations:
///   * `--all` (alone)
///   * `--data-id --dataset-name` (delete one data item from a dataset)
///   * `--dataset-name` (alone — delete the whole dataset)
fn validate_target_selection(args: &ForgetArgs) -> Result<(), CliError> {
    if args.all {
        if args.dataset_name.is_some() || args.data_id.is_some() {
            return Err(CliError::Validation(
                "--all cannot be combined with --dataset-name or --data-id".to_string(),
            ));
        }
        return Ok(());
    }

    if args.data_id.is_some() {
        if args.dataset_name.is_none() {
            return Err(CliError::Validation(
                "--data-id requires --dataset-name to scope the deletion".to_string(),
            ));
        }
        return Ok(());
    }

    if args.dataset_name.is_some() {
        return Ok(());
    }

    Err(CliError::Validation(
        "Specify a forget target: --dataset-name, --data-id (+ --dataset-name), or --all"
            .to_string(),
    ))
}
