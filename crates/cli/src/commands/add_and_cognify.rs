use std::sync::Arc;

use cognee::add::AddPipeline;
use cognee::cognify::cognify;
use cognee::database::{IngestDb, PipelineRunRepository};
use cognee::models::DataInput;
use cognee::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::{AddAndCognifyArgs, ChunkerArg};
use crate::error::CliError;

pub fn run(args: AddAndCognifyArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();
    // `None` all the way down selects the auto-calculation (Python `chunk_size=None`).
    let effective_chunk_size = args.chunk_size.or(settings.chunk_size);
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

    match args.chunker {
        ChunkerArg::Text => {}
        ChunkerArg::Langchain | ChunkerArg::Csv => {
            warn!(
                "Warning: selected chunker is not natively available in Rust yet; using TextChunker-compatible flow."
            );
        }
    }

    crate::teardown::run_command(Arc::clone(&cm), async {
        // `graph_extraction.backend`, resolved before the add: an unregistered
        // kind must not get the documents ingested by a call that was always
        // going to fail (nor silently fall back to the LLM extractor).
        let graph_backend = cm.configured_graph_extractor().await.map_err(|error| {
            CliError::Runtime(format!("Graph extractor initialization failed: {error}"))
        })?;

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
            cognee::core::RayonThreadPool::with_default_threads()
                .map_err(|e| CliError::Runtime(format!("Failed to build thread pool: {e}")))?,
        );
        // Gap 08-07: persist the four-state `pipeline_runs` trail through
        // both phases.
        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::clone(&database).pipeline_runs();

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
        let dataset = cognee::database::IngestDb::get_dataset_by_name(
            &*database,
            &args.dataset_name,
            owner_id,
            None,
        )
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

        // `--ontology-file` overrides `ontology.file_path`; neither means none.
        let ontology_resolver = cm
            .ontology_resolver(args.ontology_file.as_deref())
            .await
            .map_err(|error| {
                CliError::Runtime(format!("Ontology initialization failed: {error}"))
            })?;

        let base_config = cm.settings().cognify_config();
        let mut cognify_config = base_config
            .with_chunk_size_opt(effective_chunk_size.map(|n| n as usize))
            .with_max_parallel_extractions(effective_max_parallel)
            .with_summarization(!summarization_model.is_empty());
        if let Some(backend) = graph_backend {
            cognify_config = cognify_config.with_graph_backend(backend);
        }
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

        // OSS build has no DB-backed user lookup (there is no `users`
        // table), so `user_email` always falls back to `None`. `cognify()` then
        // uses `user_id.to_string()` as the provenance stamp.
        let user_email: Option<String> = None;

        let thread_pool: Arc<dyn cognee::core::CpuPool> = Arc::new(
            cognee::core::RayonThreadPool::with_default_threads()
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
            result.embedding_count
        );

        Ok(())
    })
}
