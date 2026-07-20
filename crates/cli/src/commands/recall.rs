use std::sync::Arc;

use cognee::api::recall;
use cognee::database::IngestDb;
use cognee::search::{SeaOrmSessionStore, SearchBuilder, SearchType, SessionManager};
use cognee::session::SessionStore;
use cognee::{ComponentManager, PipelineContext};
use uuid::Uuid;

use crate::cli::{OutputFormatArg, QueryTypeArg, RecallArgs};
use crate::error::CliError;

pub fn run(args: RecallArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();

    if !(1..=100).contains(&args.top_k) {
        return Err(CliError::Validation(
            "--top-k must be between 1 and 100".to_string(),
        ));
    }

    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;
    drop(settings);

    // When `--query-type` is omitted, recall auto-routes the search type.
    let (query_type, auto_route): (Option<SearchType>, bool) = match args.query_type {
        Some(qt) => (Some(map_query_type(qt)), false),
        None => (None, true),
    };

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

        let session_store: Arc<dyn SessionStore> = Arc::new(
            SeaOrmSessionStore::new(Arc::clone(&database))
                .await
                .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?,
        );
        let session_manager = Arc::new(SessionManager::new(Arc::clone(&session_store)));

        let orchestrator =
            SearchBuilder::new(vector_db, embedding_engine, graph_db, llm, database.clone())
                .with_session_manager(Arc::clone(&session_manager))
                .with_dataset_resolver(database as Arc<dyn IngestDb>)
                .build();

        let datasets = if args.datasets.is_empty() {
            None
        } else {
            Some(args.datasets)
        };

        let user_id_str = owner_id.to_string();

        let result = recall(
            &args.query,
            query_type,
            datasets,
            args.top_k,
            auto_route,
            args.session_id.as_deref(),
            Some(user_id_str.as_str()),
            &orchestrator,
            Some(session_store.as_ref()),
            Some(session_manager.as_ref()),
            None,
            None,
        )
        .await
        .map_err(|error| CliError::Runtime(format!("Recall failed: {error}")))?;

        render_output(&result, args.output_format)?;
        Ok(())
    })
}

fn map_query_type(query_type: QueryTypeArg) -> SearchType {
    match query_type {
        QueryTypeArg::GraphCompletion => SearchType::GraphCompletion,
        QueryTypeArg::RagCompletion => SearchType::RagCompletion,
        QueryTypeArg::Chunks => SearchType::Chunks,
        QueryTypeArg::Summaries => SearchType::Summaries,
        QueryTypeArg::Code => SearchType::CodingRules,
        QueryTypeArg::Cypher => SearchType::Cypher,
        QueryTypeArg::Temporal => SearchType::Temporal,
    }
}

fn render_output(
    result: &cognee::api::RecallResult,
    output_format: OutputFormatArg,
) -> Result<(), CliError> {
    // Recall results are the command's primary output and must reach stdout
    // regardless of the active log level, so they use `println!` rather than
    // the tracing logger (matches `search`).
    match output_format {
        OutputFormatArg::Json => {
            let items: Vec<&serde_json::Value> = result.items.iter().map(|i| &i.content).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&items).map_err(|error| {
                    CliError::Runtime(format!("Failed to render JSON output: {error}"))
                })?
            );
        }
        OutputFormatArg::Simple => {
            for item in &result.items {
                println!("{}", item.content);
            }
        }
        OutputFormatArg::Pretty => {
            if result.items.is_empty() {
                println!("No results found for your query.");
            } else {
                println!("Found {} result(s):", result.items.len());
                for (index, item) in result.items.iter().enumerate() {
                    println!("{}. [{}] {}", index + 1, item.source.as_str(), item.content);
                }
            }
        }
    }

    Ok(())
}
