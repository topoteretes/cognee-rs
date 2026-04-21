use std::io;
use std::sync::Arc;

use cognee_lib::PipelineContext;
use cognee_lib::database::AclDb;
use cognee_lib::delete::{
    AuthorizedDeleteService, DeleteMode, DeleteRequest, DeleteScope, DeleteService,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::{DeleteArgs, DeleteModeArg};
use crate::error::CliError;

pub fn run(args: DeleteArgs, cm: Arc<cognee_lib::ComponentManager>) -> Result<(), CliError> {
    let dry_run = args.dry_run;
    let force = args.force;
    let enforce_acl = args.enforce_acl;

    validate_scope_selection(&args)?;

    let owner_id = if let Some(user_id) = &args.user_id {
        Uuid::parse_str(user_id).map_err(|error| {
            CliError::Validation(format!("Invalid --user-id '{}': {error}", user_id))
        })?
    } else {
        Uuid::parse_str(&cm.settings().default_user_id).map_err(|error| {
            CliError::Validation(format!(
                "Invalid default_user_id '{}': {error}",
                cm.settings().default_user_id
            ))
        })?
    };

    let request = build_request(args, owner_id)?;

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

        let service = DeleteService::new(
            storage,
            database.clone() as Arc<dyn cognee_lib::database::DeleteDb>,
        )
        .with_graph_db(graph_db)
        .with_vector_db(vector_db);

        if enforce_acl {
            let acl_db: Arc<dyn AclDb> = database.clone();
            let delete_db: Arc<dyn cognee_lib::database::DeleteDb> = database;
            let auth_service = AuthorizedDeleteService::new(service, acl_db, delete_db);

            let preview = auth_service
                .preview(&request, owner_id)
                .await
                .map_err(|error| CliError::Runtime(format!("Delete preview failed: {error}")))?;
            print_preview(&preview);

            if dry_run {
                return Ok(());
            }
            if !force {
                confirm_deletion()?;
            }

            let result = auth_service
                .execute(&request, owner_id)
                .await
                .map_err(|error| CliError::Runtime(format!("Delete execution failed: {error}")))?;
            print_result(&result);
        } else {
            let preview = service
                .preview(&request)
                .await
                .map_err(|error| CliError::Runtime(format!("Delete preview failed: {error}")))?;
            print_preview(&preview);

            if dry_run {
                return Ok(());
            }
            if !force {
                confirm_deletion()?;
            }

            let result = service
                .execute(&request)
                .await
                .map_err(|error| CliError::Runtime(format!("Delete execution failed: {error}")))?;
            print_result(&result);
        }

        Ok(())
    })
}

fn print_preview(preview: &cognee_lib::delete::DeletePreview) {
    info!("Preview:");
    info!("  datasets_to_delete: {}", preview.datasets_to_delete);
    info!(
        "  dataset_links_to_delete: {}",
        preview.dataset_links_to_delete
    );
    info!("  data_to_delete: {}", preview.data_to_delete);
    info!(
        "  storage_files_to_delete: {}",
        preview.storage_files_to_delete
    );
    info!("  graph_nodes_to_delete: {}", preview.graph_nodes_to_delete);
    info!(
        "  vector_points_to_delete: {}",
        preview.vector_points_to_delete
    );
}

fn print_result(result: &cognee_lib::delete::DeleteResult) {
    info!(
        "Success: Deleted datasets={}, links={}, data={}, storage_files={}, graph_nodes={}, vector_points={}, orphan_entities={}, orphan_entity_types={}, orphan_edge_types={}",
        result.deleted_datasets,
        result.deleted_dataset_links,
        result.deleted_data,
        result.deleted_storage_files,
        result.deleted_graph_nodes,
        result.deleted_vector_points,
        result.deleted_orphan_entities,
        result.deleted_orphan_entity_types,
        result.deleted_orphan_edge_types,
    );

    for warning in &result.warnings {
        warn!("Warning: {warning}");
    }
}

fn confirm_deletion() -> Result<(), CliError> {
    info!("This operation is irreversible. Continue? [y/N]: ");

    let mut confirmation = String::new();
    io::stdin()
        .read_line(&mut confirmation)
        .map_err(|error| CliError::Runtime(format!("Failed to read confirmation: {error}")))?;

    let answer = confirmation.trim().to_lowercase();
    if answer != "y" && answer != "yes" {
        info!("Deletion cancelled.");
        return Err(CliError::Validation(
            "Deletion cancelled by user".to_string(),
        ));
    }

    Ok(())
}

fn validate_scope_selection(args: &DeleteArgs) -> Result<(), CliError> {
    let mut selected = 0usize;
    if args.data_id.is_some() {
        selected += 1;
    }
    if args.dataset_name.is_some() {
        selected += 1;
    }
    if args.user_id.is_some() {
        selected += 1;
    }
    if args.all {
        selected += 1;
    }

    if selected != 1 {
        return Err(CliError::Validation(
            "Specify exactly one delete scope: --data-id, --dataset-name, --user-id, or --all"
                .to_string(),
        ));
    }

    if args.delete_dataset_if_empty && args.data_id.is_none() {
        return Err(CliError::Validation(
            "--delete-dataset-if-empty only applies with --data-id".to_string(),
        ));
    }

    Ok(())
}

fn build_request(args: DeleteArgs, owner_id: Uuid) -> Result<DeleteRequest, CliError> {
    let mode = match args.mode {
        DeleteModeArg::Soft => DeleteMode::Soft,
        DeleteModeArg::Hard => DeleteMode::Hard,
    };

    let scope = if let Some(data_id) = args.data_id {
        let parsed_data_id = Uuid::parse_str(&data_id).map_err(|error| {
            CliError::Validation(format!("Invalid --data-id '{}': {error}", data_id))
        })?;

        DeleteScope::Data {
            owner_id,
            data_id: parsed_data_id,
            dataset_name: args.dataset_name,
            delete_dataset_if_empty: args.delete_dataset_if_empty,
        }
    } else if let Some(dataset_name) = args.dataset_name {
        DeleteScope::Dataset {
            owner_id,
            dataset_name,
        }
    } else if args.user_id.is_some() {
        DeleteScope::User { owner_id }
    } else {
        DeleteScope::All
    };

    Ok(DeleteRequest { scope, mode })
}
