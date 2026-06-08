use std::sync::Arc;

use cognee_lib::add::AddPipeline;
use cognee_lib::cognify::{ChunkStrategy, CognifyConfig, cognify};
use cognee_lib::database::{
    IngestDb, PipelineRunRepository, SeaOrmPipelineRunRepository, UserDb, ops,
};
use cognee_lib::models::DataInput;
use cognee_lib::ontology::{NoOpOntologyResolver, OntologyResolver, RdfLibOntologyResolver};
use cognee_lib::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::{AddAndCognifyArgs, ChunkerArg};
use crate::error::CliError;

pub fn run(args: AddAndCognifyArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
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
    let summarization_model = cm.settings().summarization_model.clone();
    let settings_ontology_path = cm.settings().ontology_file_path.clone();

    match args.chunker {
        ChunkerArg::Text => {}
        ChunkerArg::Langchain | ChunkerArg::Csv => {
            warn!(
                "Warning: selected chunker is not natively available in Rust yet; using TextChunker-compatible flow."
            );
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async {
        let database = cm
            .database()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let storage = cm
            .storage()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        // ── Add ──────────────────────────────────────────────────────────────
        let graph_db_for_add = cm
            .graph_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let vector_db_for_add = cm
            .vector_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let thread_pool_for_add = Arc::new(
            cognee_lib::core::RayonThreadPool::with_default_threads()
                .map_err(|e| CliError::Runtime(format!("Failed to build thread pool: {e}")))?,
        );
        // Gap 08-07: persist the four-state `pipeline_runs` trail through
        // both phases.
        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

        let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
            .with_thread_pool(thread_pool_for_add)
            .with_graph_db(graph_db_for_add)
            .with_vector_db(vector_db_for_add)
            .with_database(Arc::clone(&database))
            .with_pipeline_run_repo(Arc::clone(&pipeline_run_repo));

        let inputs = args
            .data
            .into_iter()
            .map(DataInput::from_string)
            .collect::<Vec<_>>();

        let added_data = ingest
            .add(inputs, &args.dataset_name, owner_id, None)
            .await
            .map_err(|error| CliError::Runtime(format!("Add operation failed: {error}")))?;

        info!(
            "Success: Added {} item(s) to dataset '{}'.",
            added_data.len(),
            args.dataset_name
        );

        if added_data.is_empty() {
            info!("No new data to cognify.");
            return Ok(());
        }

        // ── Cognify only the newly-added items ──────────────────────────────
        let dataset =
            ops::datasets::get_dataset_by_name(&database, &args.dataset_name, owner_id, None)
                .await
                .map_err(|error| {
                    CliError::Runtime(format!(
                        "Failed to resolve dataset '{}': {error}",
                        args.dataset_name
                    ))
                })?
                .ok_or_else(|| {
                    CliError::Validation(format!(
                        "Dataset '{}' was not found for owner {}",
                        args.dataset_name, owner_id
                    ))
                })?;

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

        let mut cognify_config = CognifyConfig::default()
            .with_chunk_size(effective_chunk_size as usize)
            .with_chunk_overlap(cm.settings().chunk_overlap as usize)
            .with_chunk_strategy(chunk_strategy)
            .with_max_parallel_extractions(effective_max_parallel)
            .with_summarization(!summarization_model.is_empty());
        if let Some(transcriber) = cm
            .transcriber()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?
        {
            cognify_config = cognify_config.with_transcriber(transcriber);
        }

        info!(
            "Cognifying {} new data item(s) in dataset '{}'",
            added_data.len(),
            args.dataset_name
        );

        // Best-effort lookup of `User.email` for provenance stamping; falls
        // back to `user_id.to_string()` inside `cognify()`.
        let user_email = database
            .get_user(owner_id)
            .await
            .ok()
            .flatten()
            .map(|u| u.email);

        let thread_pool: Arc<dyn cognee_lib::core::CpuPool> = Arc::new(
            cognee_lib::core::RayonThreadPool::with_default_threads()
                .map_err(|e| CliError::Runtime(format!("failed to construct thread pool: {e}")))?,
        );

        let result = cognify(
            added_data,
            dataset.id,
            Some(owner_id),
            user_email,
            None,
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            Arc::clone(&database),
            pipeline_run_repo,
            thread_pool,
            ontology_resolver,
            &cognify_config,
        )
        .await
        .map_err(|error| {
            CliError::Runtime(format!(
                "Cognify execution failed for dataset '{}': {error}",
                args.dataset_name
            ))
        })?;

        info!(
            "Cognify completed. chunks={}, entities={}, edges={}, summaries={}, embeddings={}",
            result.chunks.len(),
            result.entities.len(),
            result.edges.len(),
            result.summaries.len(),
            result.embeddings.len()
        );

        Ok(())
    })
}
