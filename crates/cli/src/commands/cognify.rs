use std::sync::Arc;

use chrono::Utc;
use cognee_lib::cognify::{ChunkStrategy, CognifyConfig, cognify};
use cognee_lib::database::{ArtifactReference, DatabaseConnection, ops};
use cognee_lib::ontology::{OntologyResolver, RdfLibOntologyResolver};
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

        let dataset_names =
            resolve_dataset_names(&database, owner_id, requested_datasets).await?;

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

        if let Some(path) = &args.ontology_file {
            // Validate the ontology file eagerly so we fail fast.
            let _resolver: Arc<dyn OntologyResolver> = Arc::new(
                RdfLibOntologyResolver::new(path.as_str()).map_err(|error| {
                    CliError::Runtime(format!("Ontology initialization failed: {error}"))
                })?,
            );
            // NOTE: ontology enrichment is not yet wired into the pipeline tasks.
        }

        let chunk_strategy = match cm.settings().chunk_strategy.to_uppercase().as_str() {
            "RECURSIVE" => ChunkStrategy::Recursive,
            _ => ChunkStrategy::Paragraph,
        };

        let cognify_config = CognifyConfig::default()
            .with_chunk_size(effective_chunk_size as usize)
            .with_chunk_overlap(cm.settings().chunk_overlap as usize)
            .with_chunk_strategy(chunk_strategy)
            .with_max_parallel_extractions(effective_max_parallel);

        let mut total_chunks = 0usize;
        let mut total_entities = 0usize;
        let mut total_edges = 0usize;
        let mut total_summaries = 0usize;
        let mut total_embeddings = 0usize;

        for dataset_name in &dataset_names {
            let dataset = ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
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

            let result = cognify(
                data_items,
                dataset.id,
                Some(owner_id),
                None,
                llm.clone(),
                Arc::clone(&storage),
                Arc::clone(&graph_db),
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                None,
                &cognify_config,
            )
                .await
                .map_err(|error| {
                    CliError::Runtime(format!(
                        "Cognify execution failed for dataset '{dataset_name}': {error}"
                    ))
                })?;

            let artifact_references = build_artifact_references(owner_id, dataset.id, &result);
            ops::artifact_refs::upsert_artifact_references(&database, &artifact_references)
                .await
                .map_err(|error| {
                    CliError::Runtime(format!(
                        "Failed to persist artifact references for dataset '{dataset_name}': {error}"
                    ))
                })?;

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

async fn resolve_dataset_names(
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

pub(crate) fn build_artifact_references(
    owner_id: Uuid,
    dataset_id: Uuid,
    result: &cognee_lib::cognify::CognifyResult,
) -> Vec<ArtifactReference> {
    let created_at = Utc::now();
    let mut references = Vec::new();

    let mut chunk_to_data_id = std::collections::HashMap::new();

    for chunk in &result.chunks {
        chunk_to_data_id.insert(chunk.base.id, chunk.document_id);

        references.push(ArtifactReference {
            id: Uuid::new_v4(),
            owner_id,
            dataset_id,
            data_id: Some(chunk.document_id),
            artifact_kind: "vector_point".to_string(),
            artifact_id: chunk.base.id.to_string(),
            collection_name: Some("DocumentChunk_text".to_string()),
            created_at,
        });
    }

    for summary in &result.summaries {
        let data_id = summary
            .made_from
            .and_then(|chunk_id| chunk_to_data_id.get(&chunk_id).copied());
        references.push(ArtifactReference {
            id: Uuid::new_v4(),
            owner_id,
            dataset_id,
            data_id,
            artifact_kind: "vector_point".to_string(),
            artifact_id: summary.base.id.to_string(),
            collection_name: Some("TextSummary_text".to_string()),
            created_at,
        });
    }

    for entity in &result.entities {
        let entity_id = entity.entity.base.id.to_string();

        references.push(ArtifactReference {
            id: Uuid::new_v4(),
            owner_id,
            dataset_id,
            data_id: None,
            artifact_kind: "graph_node".to_string(),
            artifact_id: entity_id.clone(),
            collection_name: None,
            created_at,
        });

        references.push(ArtifactReference {
            id: Uuid::new_v4(),
            owner_id,
            dataset_id,
            data_id: None,
            artifact_kind: "vector_point".to_string(),
            artifact_id: entity_id,
            collection_name: Some("Entity_name".to_string()),
            created_at,
        });
    }

    references
}
