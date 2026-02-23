use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use cognee_lib::cognify::{ChunkStrategy, CognifyConfig, CognifyPipeline};
use cognee_lib::database::{ArtifactReference, DatabaseTrait, SqliteDatabase};
use cognee_lib::embedding::{EmbeddingConfig, OnnxEmbeddingEngine};
use cognee_lib::graph::{GraphDBTrait, LadybugAdapter};
use cognee_lib::llm::OpenAIAdapter;
use cognee_lib::ontology::{OntologyResolver, RdfLibOntologyResolver};
use cognee_lib::storage::{LocalStorage, StorageTrait};
use cognee_lib::vector::QdrantAdapter;
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::{ChunkerArg, CognifyArgs};
use crate::config_store::load_config;
use crate::error::CliError;

pub fn run(args: CognifyArgs) -> Result<(), CliError> {
    let config = load_config()?;
    let effective_chunk_size = args.chunk_size.unwrap_or(config.settings.chunk_size);
    let effective_llm_max_retries = args
        .llm_max_retries
        .unwrap_or(config.settings.llm_max_retries)
        .max(1);
    let owner_id = Uuid::parse_str(&config.settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            config.settings.default_user_id
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
        let storage = Arc::new(LocalStorage::new(PathBuf::from(
            &config.settings.data_root_directory,
        )));
        storage
            .initialize()
            .await
            .map_err(|error| CliError::Runtime(format!("Storage initialization failed: {error}")))?;

        let database = Arc::new(
            SqliteDatabase::new(&config.settings.relational_db_url)
                .await
                .map_err(|error| CliError::Runtime(format!("Database initialization failed: {error}")))?,
        );
        database.initialize().await.map_err(|error| {
            CliError::Runtime(format!("Database schema initialization failed: {error}"))
        })?;

        let dataset_names = resolve_dataset_names(database.as_ref(), owner_id, requested_datasets)
            .await?;

        let graph_provider = config.settings.graph_database_provider.to_lowercase();
        if graph_provider != "ladybug" && graph_provider != "kuzu" {
            return Err(CliError::Validation(format!(
                "Unsupported graph_database_provider '{}'. Supported for CLI runtime: ladybug, kuzu (compat alias).",
                config.settings.graph_database_provider
            )));
        }

        let graph_path = if !config.settings.graph_file_path.is_empty() {
            config.settings.graph_file_path.clone()
        } else {
            format!("{}/graph", config.settings.system_root_directory)
        };

        let graph_db = Arc::new(
            LadybugAdapter::new(&graph_path)
                .await
                .map_err(|error| CliError::Runtime(format!("Graph database initialization failed: {error}")))?,
        );
        graph_db.initialize().await.map_err(|error| {
            CliError::Runtime(format!("Graph database schema initialization failed: {error}"))
        })?;

        let vector_provider = config.settings.vector_db_provider.to_lowercase();
        if vector_provider != "qdrant" && vector_provider != "lancedb" {
            return Err(CliError::Validation(format!(
                "Unsupported vector_db_provider '{}'. Supported for CLI runtime: qdrant, lancedb (compat alias).",
                config.settings.vector_db_provider
            )));
        }

        if vector_provider == "lancedb" {
            warn!(
                "Warning: vector_db_provider=lancedb is mapped to embedded qdrant adapter in Rust CLI runtime."
            );
        }

        let vector_data_dir = if !config.settings.vector_db_url.is_empty() {
            PathBuf::from(&config.settings.vector_db_url)
        } else {
            Path::new(&config.settings.system_root_directory).join("vectors")
        };
        let vector_db = Arc::new(QdrantAdapter::new(
            vector_data_dir,
            config.settings.embedding_dimensions as usize,
        ));

        let embedding_engine = Arc::new(
            OnnxEmbeddingEngine::new(EmbeddingConfig {
                model_path: PathBuf::from(&config.settings.embedding_model_path),
                tokenizer_path: PathBuf::from(&config.settings.embedding_tokenizer_path),
                model_name: config.settings.embedding_model_name.clone(),
                dimensions: config.settings.embedding_dimensions as usize,
                max_sequence_length: config.settings.embedding_max_sequence_length as usize,
                batch_size: config.settings.embedding_batch_size as usize,
            })
            .map_err(|error| {
                CliError::Runtime(format!("Embedding engine initialization failed: {error}"))
            })?,
        );

        let llm_provider = config.settings.llm_provider.to_lowercase();
        if llm_provider != "openai" {
            return Err(CliError::Validation(format!(
                "Unsupported llm_provider '{}'. Supported for CLI runtime: openai.",
                config.settings.llm_provider
            )));
        }

        if config.settings.llm_api_key.is_empty() {
            return Err(CliError::Validation(
                "llm_api_key must be configured for cognify pipeline".to_string(),
            ));
        }

        let llm = Arc::new(
            OpenAIAdapter::new(
                config.settings.llm_model.clone(),
                config.settings.llm_api_key.clone(),
                if config.settings.llm_endpoint.is_empty() {
                    None
                } else {
                    Some(config.settings.llm_endpoint.clone())
                },
            )
            .map(|adapter| adapter.with_structured_output_retries(effective_llm_max_retries))
            .map_err(|error| CliError::Runtime(format!("LLM initialization failed: {error}")))?,
        );

        let ontology_resolver: Option<Arc<dyn OntologyResolver>> = if let Some(path) = &args.ontology_file {
            Some(Arc::new(RdfLibOntologyResolver::new(path.as_str()).map_err(
                |error| CliError::Runtime(format!("Ontology initialization failed: {error}")),
            )?))
        } else {
            None
        };

        let chunk_strategy = match config.settings.chunk_strategy.to_uppercase().as_str() {
            "RECURSIVE" => ChunkStrategy::Recursive,
            _ => ChunkStrategy::Paragraph,
        };

        let cognify_config = CognifyConfig::default()
            .with_chunk_size(effective_chunk_size as usize)
            .with_chunk_overlap(config.settings.chunk_overlap as usize)
            .with_chunk_strategy(chunk_strategy);

        let pipeline = CognifyPipeline::new(
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            cognify_config,
            ontology_resolver,
        );

        let mut total_chunks = 0usize;
        let mut total_entities = 0usize;
        let mut total_edges = 0usize;
        let mut total_summaries = 0usize;
        let mut total_embeddings = 0usize;

        for dataset_name in &dataset_names {
            let dataset = database
                .get_dataset_by_name(dataset_name, owner_id)
                .await
                .map_err(|error| {
                    CliError::Runtime(format!("Failed to resolve dataset '{dataset_name}': {error}"))
                })?
                .ok_or_else(|| {
                    CliError::Validation(format!(
                        "Dataset '{dataset_name}' was not found for owner {}",
                        owner_id
                    ))
                })?;

            let data_items = database
                .get_dataset_data(dataset.id)
                .await
                .map_err(|error| {
                    CliError::Runtime(format!(
                        "Failed to load data for dataset '{dataset_name}': {error}"
                    ))
                })?;

            if data_items.is_empty() {
                warn!("Warning: dataset '{dataset_name}' has no data to cognify.");
                continue;
            }

            let result = pipeline
                .cognify(data_items, dataset.id, Arc::clone(&llm))
                .await
                .map_err(|error| {
                    CliError::Runtime(format!(
                        "Cognify execution failed for dataset '{dataset_name}': {error}"
                    ))
                })?;

            let artifact_references = build_artifact_references(owner_id, dataset.id, &result);
            database
                .upsert_artifact_references(&artifact_references)
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

            if args.verbose {
                info!(
                    "Dataset '{}' -> chunks={}, entities={}, edges={}, summaries={}, embeddings={}",
                    dataset_name,
                    result.chunks.len(),
                    result.entities.len(),
                    result.edges.len(),
                    result.summaries.len(),
                    result.embeddings.len()
                );
            }
        }

        info!(
            "Success: Cognify completed. chunks={}, entities={}, edges={}, summaries={}, embeddings={}",
            total_chunks, total_entities, total_edges, total_summaries, total_embeddings
        );

        Ok(())
    })
}

async fn resolve_dataset_names(
    database: &dyn DatabaseTrait,
    owner_id: Uuid,
    requested_datasets: Vec<String>,
) -> Result<Vec<String>, CliError> {
    if !requested_datasets.is_empty() {
        return Ok(requested_datasets);
    }

    let datasets = database
        .list_datasets_by_owner(owner_id)
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

fn build_artifact_references(
    owner_id: Uuid,
    dataset_id: Uuid,
    result: &cognee_lib::cognify::CognifyResult,
) -> Vec<ArtifactReference> {
    let created_at = Utc::now();
    let mut references = Vec::new();

    let mut chunk_to_data_id = std::collections::HashMap::new();

    for chunk in &result.chunks {
        chunk_to_data_id.insert(chunk.id, chunk.document_id);

        references.push(ArtifactReference {
            id: Uuid::new_v4(),
            owner_id,
            dataset_id,
            data_id: Some(chunk.document_id),
            artifact_kind: "vector_point".to_string(),
            artifact_id: chunk.id.to_string(),
            collection_name: Some("DocumentChunk_text".to_string()),
            created_at,
        });
    }

    for summary in &result.summaries {
        let data_id = chunk_to_data_id.get(&summary.chunk_id).copied();
        references.push(ArtifactReference {
            id: Uuid::new_v4(),
            owner_id,
            dataset_id,
            data_id,
            artifact_kind: "vector_point".to_string(),
            artifact_id: summary.id.to_string(),
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
            artifact_id: entity_id.clone(),
            collection_name: Some("Entity_name".to_string()),
            created_at,
        });

        references.push(ArtifactReference {
            id: Uuid::new_v4(),
            owner_id,
            dataset_id,
            data_id: None,
            artifact_kind: "vector_point".to_string(),
            artifact_id: entity_id,
            collection_name: Some("Entity_description".to_string()),
            created_at,
        });
    }

    references
}
