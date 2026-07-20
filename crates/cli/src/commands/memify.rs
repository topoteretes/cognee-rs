use std::sync::Arc;

use cognee::cognee_core::{CpuPool, RayonThreadPool};
use cognee::cognify::{MemifyConfig, run_memify};
use cognee::database::{PipelineRunRepository, SeaOrmPipelineRunRepository, ops};
use cognee::{ComponentManager, PipelineContext};
use tracing::{debug, info};
use uuid::Uuid;

use crate::cli::MemifyArgs;
use crate::error::CliError;

use super::cognify::resolve_dataset_names;

pub fn run(args: MemifyArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();
    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;

    let requested_datasets = args.datasets.clone();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async {
        // Resolve datasets first (cheap) -- fail early before initializing heavy components
        let database = cm
            .database()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let dataset_names = resolve_dataset_names(&database, owner_id, requested_datasets).await?;

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

        // Build config from CLI args
        let mut config = MemifyConfig::default().with_triplet_batch_size(args.batch_size);

        if let Some(node_type) = &args.node_type {
            config = config.with_node_type_filter(node_type.clone());
        }
        if !args.node_names.is_empty() {
            config = config.with_node_name_filter(args.node_names.clone());
        }

        let mut total_triplets = 0usize;
        let mut total_indexed = 0usize;
        let mut total_batches = 0usize;

        for dataset_name in &dataset_names {
            let dataset =
                ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
                    .await
                    .map_err(|error| {
                        CliError::Runtime(format!(
                            "Failed to resolve dataset '{dataset_name}': {error}"
                        ))
                    })?
                    .ok_or_else(|| {
                        CliError::Validation(format!(
                            "Dataset '{dataset_name}' was not found for owner {owner_id}"
                        ))
                    })?;

            info!("Dataset '{dataset_name}': running memify");

            let thread_pool: Arc<dyn CpuPool> = Arc::new(
                RayonThreadPool::with_default_threads()
                    .map_err(|e| CliError::Runtime(format!("Failed to build thread pool: {e}")))?,
            );

            // Gap 08-07: persist the four-state `pipeline_runs` trail.
            let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

            let result = run_memify(
                Arc::clone(&graph_db),
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                thread_pool,
                Arc::clone(&database),
                pipeline_run_repo,
                Some(dataset.id),
                Some(owner_id),
                dataset.tenant_id,
                &config,
            )
            .await
            .map_err(|error| {
                CliError::Runtime(format!(
                    "Memify failed for dataset '{dataset_name}': {error}"
                ))
            })?;

            // Gap 08-08: surface the short-circuit verdict (Python parity).
            if result.already_completed {
                if let Some(prior) = result.prior_pipeline_run_id {
                    info!(
                        "Dataset '{dataset_name}': already complete (prior pipeline_run_id={prior}); skipping memify."
                    );
                } else {
                    info!("Dataset '{dataset_name}': already complete; skipping memify.");
                }
                continue;
            }

            total_triplets += result.triplet_count;
            total_indexed += result.index_result.indexed_count;
            total_batches += result.index_result.batch_count;

            debug!(
                "Dataset '{}' -> triplets={}, indexed={}, batches={}",
                dataset_name,
                result.triplet_count,
                result.index_result.indexed_count,
                result.index_result.batch_count
            );
        }

        info!(
            "Memify completed. triplets={}, indexed={}, batches={}",
            total_triplets, total_indexed, total_batches
        );

        Ok(())
    })
}
