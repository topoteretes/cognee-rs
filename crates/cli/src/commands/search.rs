use std::sync::Arc;

use cognee_lib::database::IngestDb;
use cognee_lib::search::{
    SeaOrmSessionStore, SearchBuilder, SearchOutput, SearchRequest, SearchResponse, SearchType,
    SessionManager,
};
use cognee_lib::{ComponentManager, PipelineContext};
use tracing::{info, warn};
use uuid::Uuid;

use crate::cli::{OutputFormatArg, QueryTypeArg, SearchArgs};
use crate::config_store::DEFAULT_SYSTEM_PROMPT_PATH;
use crate::error::CliError;

pub fn run(args: SearchArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();

    if !(1..=100).contains(&args.top_k) {
        return Err(CliError::Validation(
            "--top-k must be between 1 and 100".to_string(),
        ));
    }

    let mapped_query_type = map_query_type(args.query_type);

    if requires_llm(mapped_query_type) && settings.llm_api_key.is_empty() {
        warn!("Warning: llm_api_key is empty. LLM-based search types may fail at runtime.");
    }

    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;

    let system_prompt = args.system_prompt.unwrap_or_else(|| {
        if settings.default_system_prompt_path.is_empty() {
            DEFAULT_SYSTEM_PROMPT_PATH.to_string()
        } else {
            settings.default_system_prompt_path.clone()
        }
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| CliError::Runtime(format!("Failed to create async runtime: {error}")))?;

    runtime.block_on(async {
        let vector_db = cm
            .vector_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let embedding_engine = cm
            .embedding_engine()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let graph_db = cm
            .graph_db()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let llm = cm
            .llm()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;
        let database = cm
            .database()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?;

        let session_store = SeaOrmSessionStore::new(Arc::clone(&database))
            .await
            .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?;
        let session_manager = Arc::new(SessionManager::new(Arc::new(session_store)));
        let orchestrator =
            SearchBuilder::new(vector_db, embedding_engine, graph_db, llm, database.clone())
                .with_session_manager(session_manager)
                .with_dataset_resolver(database as Arc<dyn IngestDb>)
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
            session_id: args.session_id,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: Some(false),
            user_id: Some(owner_id),
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
        };

        let response = orchestrator
            .search(&request)
            .await
            .map_err(|error| CliError::Runtime(format!("Search execution failed: {error}")))?;

        render_output(&response, args.output_format)?;
        Ok(())
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
            SearchOutput::Structured(value) => info!("{}", value),
        },
    }

    Ok(())
}
