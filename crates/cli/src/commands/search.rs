use std::path::{Path, PathBuf};
use std::sync::Arc;

use cognee_lib::database::{DatabaseTrait, SqliteDatabase};
use cognee_lib::embedding::{EmbeddingConfig, OnnxEmbeddingEngine};
use cognee_lib::graph::{GraphDBTrait, LadybugAdapter};
use cognee_lib::llm::OpenAIAdapter;
use cognee_lib::search::{SearchBuilder, SearchOutput, SearchRequest, SearchResponse, SearchType};
use cognee_lib::vector::QdrantAdapter;
use tracing::{info, warn};

use crate::cli::{OutputFormatArg, QueryTypeArg, SearchArgs};
use crate::config_store::{DEFAULT_SYSTEM_PROMPT_PATH, Settings, load_config};
use crate::error::CliError;

pub fn run(args: SearchArgs) -> Result<(), CliError> {
    let config = load_config()?;
    let effective_llm_max_retries = args
        .llm_max_retries
        .unwrap_or(config.settings.llm_max_retries)
        .max(1);

    if !(1..=100).contains(&args.top_k) {
        return Err(CliError::Validation(
            "--top-k must be between 1 and 100".to_string(),
        ));
    }

    let mapped_query_type = map_query_type(args.query_type);

    if requires_llm(mapped_query_type) && config.settings.llm_api_key.is_empty() {
        warn!("Warning: llm_api_key is empty. LLM-based search types may fail at runtime.");
    }

    let system_prompt = args.system_prompt.unwrap_or_else(|| {
        if config.settings.default_system_prompt_path.is_empty() {
            DEFAULT_SYSTEM_PROMPT_PATH.to_string()
        } else {
            config.settings.default_system_prompt_path.clone()
        }
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async {
        let dependencies =
            build_search_dependencies(&config.settings, effective_llm_max_retries).await?;
        let orchestrator = SearchBuilder::new(
            Arc::clone(&dependencies.vector_db),
            Arc::clone(&dependencies.embedding_engine),
            Arc::clone(&dependencies.graph_db),
            Arc::clone(&dependencies.llm),
            Arc::clone(&dependencies.database),
        )
        .build();

        let datasets = if args.datasets.is_empty() {
            None
        } else {
            Some(args.datasets)
        };

        let request = SearchRequest {
            query_text: args.query_text,
            search_type: mapped_query_type,
            top_k: Some(args.top_k),
            datasets,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: Some(system_prompt),
            only_context: Some(false),
            use_combined_context: Some(false),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: Some(false),
            verbose: Some(false),
        };

        let response = orchestrator
            .search(&request)
            .await
            .map_err(|error| CliError::Runtime(format!("Search execution failed: {error}")))?;

        render_output(&response, args.output_format)?;
        Ok(())
    })
}

struct SearchDependencies {
    database: Arc<dyn DatabaseTrait>,
    graph_db: Arc<LadybugAdapter>,
    vector_db: Arc<QdrantAdapter>,
    embedding_engine: Arc<OnnxEmbeddingEngine>,
    llm: Arc<OpenAIAdapter>,
}

async fn build_search_dependencies(
    settings: &Settings,
    llm_max_retries: u32,
) -> Result<SearchDependencies, CliError> {
    let database = Arc::new(
        SqliteDatabase::new(&settings.relational_db_url)
            .await
            .map_err(|error| {
                CliError::Runtime(format!("Database initialization failed: {error}"))
            })?,
    );
    database.initialize().await.map_err(|error| {
        CliError::Runtime(format!("Database schema initialization failed: {error}"))
    })?;

    let graph_provider = settings.graph_database_provider.to_lowercase();
    if graph_provider != "ladybug" && graph_provider != "kuzu" {
        return Err(CliError::Validation(format!(
            "Unsupported graph_database_provider '{}'. Supported for CLI runtime: ladybug, kuzu (compat alias).",
            settings.graph_database_provider
        )));
    }

    let graph_path = if !settings.graph_file_path.is_empty() {
        settings.graph_file_path.clone()
    } else {
        format!("{}/graph", settings.system_root_directory)
    };

    let graph_db = Arc::new(LadybugAdapter::new(&graph_path).await.map_err(|error| {
        CliError::Runtime(format!("Graph database initialization failed: {error}"))
    })?);
    graph_db.initialize().await.map_err(|error| {
        CliError::Runtime(format!(
            "Graph database schema initialization failed: {error}"
        ))
    })?;

    let vector_provider = settings.vector_db_provider.to_lowercase();
    if vector_provider != "qdrant" && vector_provider != "lancedb" {
        return Err(CliError::Validation(format!(
            "Unsupported vector_db_provider '{}'. Supported for CLI runtime: qdrant, lancedb (compat alias).",
            settings.vector_db_provider
        )));
    }

    if vector_provider == "lancedb" {
        warn!(
            "Warning: vector_db_provider=lancedb is mapped to embedded qdrant adapter in Rust CLI runtime."
        );
    }

    let vector_data_dir = if !settings.vector_db_url.is_empty() {
        PathBuf::from(&settings.vector_db_url)
    } else {
        Path::new(&settings.system_root_directory).join("vectors")
    };

    let vector_db = Arc::new(QdrantAdapter::new(
        vector_data_dir,
        settings.embedding_dimensions as usize,
    ));

    let embedding_engine = Arc::new(
        OnnxEmbeddingEngine::new(EmbeddingConfig {
            model_path: PathBuf::from(&settings.embedding_model_path),
            tokenizer_path: PathBuf::from(&settings.embedding_tokenizer_path),
            model_name: settings.embedding_model_name.clone(),
            dimensions: settings.embedding_dimensions as usize,
            max_sequence_length: settings.embedding_max_sequence_length as usize,
            batch_size: settings.embedding_batch_size as usize,
        })
        .map_err(|error| {
            CliError::Runtime(format!("Embedding engine initialization failed: {error}"))
        })?,
    );

    let llm_provider = settings.llm_provider.to_lowercase();
    if llm_provider != "openai" {
        return Err(CliError::Validation(format!(
            "Unsupported llm_provider '{}'. Supported for CLI runtime: openai.",
            settings.llm_provider
        )));
    }

    let llm = Arc::new(
        OpenAIAdapter::new(
            settings.llm_model.clone(),
            settings.llm_api_key.clone(),
            if settings.llm_endpoint.is_empty() {
                None
            } else {
                Some(settings.llm_endpoint.clone())
            },
        )
        .map(|adapter| adapter.with_structured_output_retries(llm_max_retries.max(1)))
        .map_err(|error| CliError::Runtime(format!("LLM initialization failed: {error}")))?,
    );

    Ok(SearchDependencies {
        database,
        graph_db,
        vector_db,
        embedding_engine,
        llm,
    })
}

fn map_query_type(query_type: QueryTypeArg) -> SearchType {
    match query_type {
        QueryTypeArg::GraphCompletion => SearchType::GraphCompletion,
        QueryTypeArg::RagCompletion => SearchType::RagCompletion,
        QueryTypeArg::Chunks => SearchType::Chunks,
        QueryTypeArg::Summaries => SearchType::Summaries,
        QueryTypeArg::Code => {
            warn!("Warning: CODE is mapped to CODING_RULES compatibility mode.");
            SearchType::CodingRules
        }
        QueryTypeArg::Cypher => SearchType::Cypher,
    }
}

fn requires_llm(search_type: SearchType) -> bool {
    matches!(
        search_type,
        SearchType::GraphCompletion
            | SearchType::RagCompletion
            | SearchType::TripletCompletion
            | SearchType::GraphSummaryCompletion
            | SearchType::GraphCompletionContextExtension
            | SearchType::GraphCompletionCot
            | SearchType::NaturalLanguage
            | SearchType::Temporal
            | SearchType::CodingRules
            | SearchType::FeelingLucky
            | SearchType::Feedback
    )
}

fn render_output(
    response: &SearchResponse,
    output_format: OutputFormatArg,
) -> Result<(), CliError> {
    match output_format {
        OutputFormatArg::Json => {
            info!(
                "{}",
                serde_json::to_string_pretty(response).map_err(|error| {
                    CliError::Runtime(format!("Failed to render JSON output: {error}"))
                })?
            );
        }
        OutputFormatArg::Simple => match &response.result {
            SearchOutput::Text(text) => info!("{text}"),
            SearchOutput::Texts(items) => {
                for item in items {
                    info!("{item}");
                }
            }
            SearchOutput::Items(items) => {
                for item in items {
                    info!("{}", item.payload);
                }
            }
            other => info!("{:?}", other),
        },
        OutputFormatArg::Pretty => match &response.result {
            SearchOutput::Text(text) => {
                info!("Response: {text}");
            }
            SearchOutput::Texts(items) => {
                if items.is_empty() {
                    info!("No results found for your query.");
                } else {
                    info!("Found {} result(s):", items.len());
                    for (index, item) in items.iter().enumerate() {
                        info!("{}. {}", index + 1, item);
                    }
                }
            }
            SearchOutput::Items(items) => {
                if items.is_empty() {
                    info!("No results found for your query.");
                } else {
                    info!("Found {} result(s):", items.len());
                    for (index, item) in items.iter().enumerate() {
                        info!("Result {}:", index + 1);
                        info!("  Score: {:?}", item.score);
                        info!("  Payload: {}", item.payload);
                    }
                }
            }
            SearchOutput::GraphQueryRows(rows) => {
                if rows.is_empty() {
                    info!("No rows returned.");
                } else {
                    info!("Returned {} row(s):", rows.len());
                    for (index, row) in rows.iter().enumerate() {
                        info!(
                            "Row {}: {}",
                            index + 1,
                            serde_json::Value::Array(row.clone())
                        );
                    }
                }
            }
            SearchOutput::Rules(rules) => {
                if rules.is_empty() {
                    info!("No rules returned.");
                } else {
                    info!("Found {} rule(s):", rules.len());
                    for (index, rule) in rules.iter().enumerate() {
                        info!("{}. [{}] {}", index + 1, rule.node_set, rule.text);
                    }
                }
            }
            SearchOutput::Ack { message } => info!("{message}"),
        },
    }

    Ok(())
}
