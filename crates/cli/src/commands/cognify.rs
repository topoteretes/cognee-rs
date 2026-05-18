use std::sync::Arc;

use cognee_lib::cognify::{ChunkStrategy, CognifyConfig, cognify};
use cognee_lib::database::{
    DatabaseConnection, PipelineRunRepository, SeaOrmPipelineRunRepository, UserDb, ops,
};
use cognee_lib::ontology::{NoOpOntologyResolver, OntologyResolver, RdfLibOntologyResolver};
use cognee_lib::{ComponentManager, PipelineContext};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::cli::{ChunkerArg, CognifyArgs};
use crate::error::CliError;

pub fn run(args: CognifyArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();
    let effective_chunk_size = args.chunk_size.unwrap_or(settings.chunk_size);
    let effective_max_parallel = args
        .llm_max_parallel_requests
        .unwrap_or(settings.llm_max_parallel_requests)
        .max(1) as usize;
    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;
    let settings_ontology_path = settings.ontology_file_path.clone();

    if args.background {
        warn!(
            "Warning: --background is accepted for compatibility, but execution remains synchronous and in-process."
        );
    }

    match args.chunker {
        ChunkerArg::Text => {}
        ChunkerArg::Langchain | ChunkerArg::Csv => {
            warn!(
                "Warning: selected chunker is not natively available in Rust yet; using TextChunker-compatible flow."
            );
        }
    }

    let requested_datasets = args.datasets.clone();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async {
        // Resolve datasets first (cheap) — fail early before initializing heavy components
        let database = cm
            .database()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let dataset_names = resolve_dataset_names(&database, owner_id, requested_datasets).await?;

        let storage = cm
            .storage()
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

        let ontology_path = args.ontology_file.as_deref().or({
            if settings_ontology_path.is_empty() {
                None
            } else {
                Some(settings_ontology_path.as_str())
            }
        });
        let ontology_resolver: Arc<dyn OntologyResolver> = match ontology_path {
            Some(path) => Arc::new(RdfLibOntologyResolver::new(path).map_err(|error| {
                CliError::Runtime(format!("Ontology initialization failed: {error}"))
            })?),
            None => Arc::new(NoOpOntologyResolver::new()),
        };

        let chunk_strategy = match cm.settings().chunk_strategy.to_uppercase().as_str() {
            "RECURSIVE" => ChunkStrategy::Recursive,
            _ => ChunkStrategy::Paragraph,
        };

        let cognify_config = CognifyConfig::default()
            .with_chunk_size(effective_chunk_size as usize)
            .with_chunk_overlap(cm.settings().chunk_overlap as usize)
            .with_chunk_strategy(chunk_strategy)
            .with_max_parallel_extractions(effective_max_parallel)
            .with_temporal_cognify(args.temporal_cognify);

        let mut total_chunks = 0usize;
        let mut total_entities = 0usize;
        let mut total_edges = 0usize;
        let mut total_summaries = 0usize;
        let mut total_embeddings = 0usize;

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
                            "Dataset '{dataset_name}' was not found for owner {}",
                            owner_id
                        ))
                    })?;

            let data_items = ops::datasets::get_dataset_data(&database, dataset.id)
                .await
                .map_err(|error| {
                    CliError::Runtime(format!(
                        "Failed to load data for dataset '{dataset_name}': {error}"
                    ))
                })?;

            if data_items.is_empty() {
                info!("Dataset '{dataset_name}': no data to cognify, skipping.");
                continue;
            }

            info!(
                "Dataset '{dataset_name}': cognifying {} data item(s)",
                data_items.len()
            );

            // Best-effort lookup of `User.email` for provenance stamping;
            // falls back to `user_id.to_string()` inside `cognify()`.
            let user_email = database
                .get_user(owner_id)
                .await
                .ok()
                .flatten()
                .map(|u| u.email);

            let thread_pool: Arc<dyn cognee_lib::core::CpuPool> = Arc::new(
                cognee_lib::core::RayonThreadPool::with_default_threads().map_err(|e| {
                    CliError::Runtime(format!("failed to construct thread pool: {e}"))
                })?,
            );

            // Gap 08-07: persist the four-state `pipeline_runs` trail so
            // CLI cognify shows up in `/api/v1/activity/pipeline-runs`.
            let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

            let result = cognify(
                data_items,
                dataset.id,
                Some(owner_id),
                user_email,
                dataset.tenant_id,
                llm.clone(),
                Arc::clone(&storage),
                Arc::clone(&graph_db),
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&database),
                pipeline_run_repo,
                thread_pool,
                Arc::clone(&ontology_resolver),
                &cognify_config,
            )
            .await
            .map_err(|error| {
                CliError::Runtime(format!(
                    "Cognify execution failed for dataset '{dataset_name}': {error}"
                ))
            })?;

            // Gap 08-08: surface the short-circuit verdict (Python parity).
            if result.already_completed {
                if let Some(prior) = result.prior_pipeline_run_id {
                    info!(
                        "Dataset '{dataset_name}': already complete (prior pipeline_run_id={prior}); skipping cognify."
                    );
                } else {
                    info!("Dataset '{dataset_name}': already complete; skipping cognify.");
                }
                continue;
            }

            total_chunks += result.chunks.len();
            total_entities += result.entities.len();
            total_edges += result.edges.len();
            total_summaries += result.summaries.len();
            total_embeddings += result.embeddings.len();

            debug!(
                "Dataset '{}' -> chunks={}, entities={}, edges={}, summaries={}, embeddings={}",
                dataset_name,
                result.chunks.len(),
                result.entities.len(),
                result.edges.len(),
                result.summaries.len(),
                result.embeddings.len()
            );
        }

        info!(
            "Cognify completed. chunks={}, entities={}, edges={}, summaries={}, embeddings={}",
            total_chunks, total_entities, total_edges, total_summaries, total_embeddings
        );

        Ok(())
    })
}

pub(crate) async fn resolve_dataset_names(
    database: &DatabaseConnection,
    owner_id: Uuid,
    requested_datasets: Vec<String>,
) -> Result<Vec<String>, CliError> {
    if !requested_datasets.is_empty() {
        return Ok(requested_datasets);
    }

    let datasets = ops::datasets::list_datasets_by_owner(database, owner_id)
        .await
        .map_err(|error| {
            CliError::Runtime(format!(
                "Failed to list datasets for owner {}: {error}",
                owner_id
            ))
        })?;

    if datasets.is_empty() {
        return Err(CliError::Validation(format!(
            "No datasets found for owner {}. Add data first or pass --datasets.",
            owner_id
        )));
    }

    Ok(datasets.into_iter().map(|dataset| dataset.name).collect())
}
