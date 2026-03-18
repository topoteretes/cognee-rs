use std::sync::Arc;

use cognee_lib::add::IngestPipeline;
use cognee_lib::cognify::{ChunkStrategy, CognifyConfig, CognifyPipeline};
use cognee_lib::models::DataInput;
use cognee_lib::ontology::{OntologyResolver, RdfLibOntologyResolver};
use cognee_lib::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::{AddAndCognifyArgs, ChunkerArg};
use crate::error::CliError;

use super::cognify::build_artifact_references;

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
        let ingest = IngestPipeline::new(Arc::clone(&storage), Arc::clone(&database));

        let inputs = args
            .data
            .into_iter()
            .map(DataInput::from_string)
            .collect::<Vec<_>>();

        let added_data = ingest
            .add(inputs, &args.dataset_name, owner_id)
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
        let dataset = database
            .get_dataset_by_name(&args.dataset_name, owner_id)
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

        let ontology_resolver: Option<Arc<dyn OntologyResolver>> =
            if let Some(path) = &args.ontology_file {
                Some(Arc::new(
                    RdfLibOntologyResolver::new(path.as_str()).map_err(|error| {
                        CliError::Runtime(format!("Ontology initialization failed: {error}"))
                    })?,
                ))
            } else {
                None
            };

        let chunk_strategy = match cm.settings().chunk_strategy.to_uppercase().as_str() {
            "RECURSIVE" => ChunkStrategy::Recursive,
            _ => ChunkStrategy::Paragraph,
        };

        let cognify_config = CognifyConfig::default()
            .with_chunk_size(effective_chunk_size as usize)
            .with_chunk_overlap(cm.settings().chunk_overlap as usize)
            .with_chunk_strategy(chunk_strategy)
            .with_max_parallel_extractions(effective_max_parallel);

        let pipeline = CognifyPipeline::new(
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            cognify_config,
            ontology_resolver,
        );

        info!(
            "Cognifying {} new data item(s) in dataset '{}'",
            added_data.len(),
            args.dataset_name
        );

        let result = pipeline
            .cognify(added_data, dataset.id, llm)
            .await
            .map_err(|error| {
                CliError::Runtime(format!(
                    "Cognify execution failed for dataset '{}': {error}",
                    args.dataset_name
                ))
            })?;

        let artifact_references = build_artifact_references(owner_id, dataset.id, &result);
        database
            .upsert_artifact_references(&artifact_references)
            .await
            .map_err(|error| {
                CliError::Runtime(format!(
                    "Failed to persist artifact references for dataset '{}': {error}",
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
