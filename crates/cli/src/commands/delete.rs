use std::io;
use std::sync::Arc;

use cognee_lib::database::ArtifactReference;
use cognee_lib::delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_lib::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::{DeleteArgs, DeleteModeArg};
use crate::error::CliError;

pub fn run(args: DeleteArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let dry_run = args.dry_run;
    let force = args.force;

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

        let service = DeleteService::new(storage, database);

        let preview = service
            .preview(&request)
            .await
            .map_err(|error| CliError::Runtime(format!("Delete preview failed: {error}")))?;

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

        if dry_run {
            return Ok(());
        }

        if !force {
            info!("This operation is irreversible. Continue? [y/N]: ");

            let mut confirmation = String::new();
            io::stdin().read_line(&mut confirmation).map_err(|error| {
                CliError::Runtime(format!("Failed to read confirmation: {error}"))
            })?;

            let answer = confirmation.trim().to_lowercase();
            if answer != "y" && answer != "yes" {
                info!("Deletion cancelled.");
                return Ok(());
            }
        }

        let artifact_references = service
            .artifact_references_for_request(&request)
            .await
            .map_err(|error| {
                CliError::Runtime(format!(
                    "Failed to resolve artifact references for cleanup: {error}"
                ))
            })?;

        let mut result = service
            .execute(&request)
            .await
            .map_err(|error| CliError::Runtime(format!("Delete execution failed: {error}")))?;

        let cleanup_warnings =
            cleanup_graph_and_vector(&cm, &request, &artifact_references).await?;
        result.warnings.extend(cleanup_warnings);

        info!(
            "Success: Deleted datasets={}, links={}, data={}, storage_files={}",
            result.deleted_datasets,
            result.deleted_dataset_links,
            result.deleted_data,
            result.deleted_storage_files
        );

        for warning in result.warnings {
            warn!("Warning: {warning}");
        }

        Ok(())
    })
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

async fn cleanup_graph_and_vector(
    cm: &ComponentManager,
    request: &DeleteRequest,
    artifact_references: &[ArtifactReference],
) -> Result<Vec<String>, CliError> {
    let mut warnings = Vec::new();

    let is_all_scope = matches!(request.scope, DeleteScope::All);

    if !is_all_scope && artifact_references.is_empty() {
        warnings.push(
            "No artifact references found for targeted graph/vector cleanup; run cognify to populate provenance for precise deletion."
                .to_string(),
        );
        return Ok(warnings);
    }

    let graph_db = cm
        .graph_db()
        .await
        .map_err(|e| CliError::Runtime(format!("{e}")))?;

    if is_all_scope {
        graph_db
            .delete_graph()
            .await
            .map_err(|error| CliError::Runtime(format!("Graph cleanup failed: {error}")))?;
    } else {
        let node_ids: Vec<String> = artifact_references
            .iter()
            .filter(|reference| reference.artifact_kind == "graph_node")
            .map(|reference| reference.artifact_id.clone())
            .collect();
        if !node_ids.is_empty() {
            graph_db.delete_nodes(&node_ids).await.map_err(|error| {
                CliError::Runtime(format!("Targeted graph cleanup failed: {error}"))
            })?;
        }
    }

    let vector_db = cm
        .vector_db()
        .await
        .map_err(|e| CliError::Runtime(format!("{e}")))?;

    if is_all_scope {
        let known_collections = [
            ("DocumentChunk", "text"),
            ("Entity", "name"),
            ("Entity", "description"),
            ("TextSummary", "text"),
            ("Triplet", "embeddable_text"),
        ];

        for (data_type, field_name) in known_collections {
            let exists = vector_db
                .has_collection(data_type, field_name)
                .await
                .map_err(|error| {
                    CliError::Runtime(format!(
                        "Failed to inspect vector collection {data_type}_{field_name}: {error}"
                    ))
                })?;

            if exists {
                vector_db
                    .delete_collection(data_type, field_name)
                    .await
                    .map_err(|error| {
                        CliError::Runtime(format!(
                            "Failed to delete vector collection {data_type}_{field_name}: {error}"
                        ))
                    })?;
            }
        }
    } else {
        let mut by_collection: std::collections::HashMap<String, Vec<Uuid>> =
            std::collections::HashMap::new();
        for reference in artifact_references
            .iter()
            .filter(|reference| reference.artifact_kind == "vector_point")
        {
            if let Some(collection_name) = &reference.collection_name
                && let Ok(id) = Uuid::parse_str(&reference.artifact_id)
            {
                by_collection
                    .entry(collection_name.clone())
                    .or_default()
                    .push(id);
            }
        }

        for (collection_name, ids) in by_collection {
            if ids.is_empty() {
                continue;
            }
            if let Some((data_type, field_name)) = collection_name.split_once('_') {
                let exists = vector_db
                    .has_collection(data_type, field_name)
                    .await
                    .map_err(|error| {
                        CliError::Runtime(format!(
                            "Failed to inspect vector collection {}: {}",
                            collection_name, error
                        ))
                    })?;
                if exists {
                    vector_db
                        .delete_points(data_type, field_name, &ids)
                        .await
                        .map_err(|error| {
                            CliError::Runtime(format!(
                                "Failed to delete vector points from {}: {}",
                                collection_name, error
                            ))
                        })?;
                }
            } else {
                warnings.push(format!(
                    "Skipping unsupported collection naming '{}'; expected '<Type>_<field>'",
                    collection_name
                ));
            }
        }
    }

    Ok(warnings)
}
