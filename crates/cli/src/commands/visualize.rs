use std::path::PathBuf;
use std::sync::Arc;

use cognee::models::dataset_scope::maybe_with_dataset;
use cognee::{ComponentManager, PipelineContext, visualize};
use tracing::info;
use uuid::Uuid;

use crate::cli::VisualizeArgs;
use crate::error::CliError;

pub fn run(args: VisualizeArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    // Scoped so the settings read guard is dropped before the async block.
    let (owner_id, dataset) = {
        let settings = cm.settings();
        let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
            CliError::Validation(format!(
                "Invalid default_user_id '{}': {error}",
                settings.default_user_id
            ))
        })?;
        let dataset = args
            .dataset
            .clone()
            .unwrap_or_else(|| settings.effective_default_dataset_name().to_string());
        (owner_id, dataset)
    };
    let tenant_id = super::parse_tenant_id(args.tenant_id.as_deref())?;

    crate::teardown::run_command(Arc::clone(&cm), async move {
        let graph_db = cm
            .graph_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let per_dataset = cm
            .dataset_storage()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?
            .is_some();

        // The dataset's scope: under per-dataset storage the graph it reaches
        // is the dataset's own.
        // An id is the caller's like a name is: its dataset must be theirs and
        // in their tenant, or it is "not found".
        let database = cm
            .database()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let lookup_error = |error: cognee::database::DatabaseError| {
            CliError::Runtime(format!("Failed to resolve dataset '{dataset}': {error}"))
        };
        let dataset_id = match Uuid::parse_str(&dataset) {
            Ok(id) => cognee::database::IngestDb::get_dataset(&*database, id)
                .await
                .map_err(lookup_error)?
                .filter(|ds| ds.owner_id == owner_id && ds.tenant_id == tenant_id)
                .map(|ds| ds.id),
            Err(_) => cognee::database::IngestDb::get_dataset_by_name_in_tenant(
                &*database, &dataset, owner_id, tenant_id,
            )
            .await
            .map_err(lookup_error)?
            .map(|ds| ds.id),
        };
        if per_dataset && dataset_id.is_none() {
            return Err(CliError::Validation(format!(
                "Dataset '{dataset}' not found"
            )));
        }

        let output: Option<PathBuf> = args.output.as_deref().map(PathBuf::from);
        let path = maybe_with_dataset(dataset_id, visualize(&*graph_db, output.as_deref()))
            .await
            .map_err(|error| CliError::Runtime(format!("Visualization failed: {error}")))?;

        info!("Graph visualization saved to {}", path.display());
        println!("{}", path.display());
        Ok(())
    })
}
