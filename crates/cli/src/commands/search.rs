use std::sync::Arc;

use cognee::database::IngestDb;
use cognee::search::{
    SearchBuilder, SearchError, SearchOutput, SearchRequest, SearchResponse, SearchType,
};
use cognee::{ComponentManager, PipelineContext};
use tracing::warn;
use uuid::Uuid;

use crate::cli::{OutputFormatArg, QueryTypeArg, SearchArgs};
use crate::error::CliError;

pub fn run(args: SearchArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();
    let log_search_history = settings.log_search_history;

    if !(1..=100).contains(&args.top_k) {
        return Err(CliError::Validation(
            "--top-k must be between 1 and 100".to_string(),
        ));
    }

    let mapped_query_type = map_query_type(args.query_type);
    // `--code-query` / `--diagram` / `--diagram-out`, refused before anything
    // runs without `-t CODE` (Python's `build_code_query`).
    let code_query = crate::code_search::build_code_query(
        &args.code.options(),
        mapped_query_type == SearchType::Code,
    )?;
    let diagram_out = args.code.diagram_out.clone();

    if requires_llm(mapped_query_type) && settings.llm().api_key.is_empty() {
        warn!("Warning: llm_api_key is empty. LLM-based search types may fail at runtime.");
    }

    let owner_id = Uuid::parse_str(&settings.default_user_id).map_err(|error| {
        CliError::Validation(format!(
            "Invalid default_user_id '{}': {error}",
            settings.default_user_id
        ))
    })?;

    let tenant_id = crate::commands::parse_tenant_id(args.tenant_id.as_deref())?;

    // `--system-prompt` is inline prompt text and takes precedence over
    // `--system-prompt-path`. With neither, the orchestrator applies the
    // `default_system_prompt_path` setting (see `Settings::search_tuning`).
    let (inline_system_prompt, system_prompt_path) = match args.system_prompt {
        Some(inline) => (Some(inline), None),
        None => (None, args.system_prompt_path),
    };

    crate::teardown::run_command(Arc::clone(&cm), async {
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

        let session_store = cm
            .session_store()
            .await
            .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?;
        let session_manager = Arc::new(cm.settings().session_manager(session_store));
        let search_tuning = cm.settings().search_tuning();
        // Per-dataset storage: every dataset is searched in its own databases.
        let per_dataset_storage = cm
            .dataset_storage()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?
            .is_some();
        let orchestrator =
            SearchBuilder::new(vector_db, embedding_engine, graph_db, llm, database.clone())
                .with_tuning(search_tuning)
                .with_session_manager(session_manager)
                .with_dataset_resolver(database as Arc<dyn IngestDb>)
                .with_search_history_logging(log_search_history)
                .with_per_dataset_storage(per_dataset_storage)
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
            system_prompt: inline_system_prompt,
            system_prompt_path,
            only_context: Some(false),
            use_combined_context: Some(false),
            session_id: args.session_id,
            node_type: None,
            node_name: None,
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            user_id: Some(owner_id),
            tenant_id,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: args.neighborhood_depth.map(usize::from),
            neighborhood_seed_top_k: args.neighborhood_seed_top_k.map(usize::from),
            // The CLI prints the answer text only; it has no flag for references.
            include_references: false,
            code_query,
        };

        let response = orchestrator.search(&request).await.map_err(search_error)?;

        render_output(&response, args.output_format)?;
        // After the results, as Python's `handle_diagram_out` runs.
        for line in crate::code_search::handle_diagram_out(
            &serde_json::to_value(&response.result).map_err(|error| {
                CliError::Runtime(format!("Failed to read the search result: {error}"))
            })?,
            diagram_out.as_deref(),
        )? {
            println!("{line}");
        }
        Ok(())
    })
}

fn map_query_type(query_type: QueryTypeArg) -> SearchType {
    match query_type {
        QueryTypeArg::GraphCompletion => SearchType::GraphCompletion,
        QueryTypeArg::HybridCompletion => SearchType::HybridCompletion,
        QueryTypeArg::RagCompletion => SearchType::RagCompletion,
        QueryTypeArg::Chunks => SearchType::Chunks,
        QueryTypeArg::Summaries => SearchType::Summaries,
        QueryTypeArg::Code => SearchType::Code,
        QueryTypeArg::Cypher => SearchType::Cypher,
        QueryTypeArg::Temporal => SearchType::Temporal,
    }
}

fn requires_llm(search_type: SearchType) -> bool {
    matches!(
        search_type,
        SearchType::GraphCompletion
            | SearchType::HybridCompletion
            | SearchType::RagCompletion
            | SearchType::GraphSummaryCompletion
            | SearchType::GraphCompletionContextExtension
            | SearchType::GraphCompletionCot
            | SearchType::GraphCompletionDecomposition
            | SearchType::GraphReport
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
    // Search results are the command's primary output and must reach stdout
    // regardless of the active log level (RUST_LOG), so they use `println!`
    // rather than the tracing logger.
    match output_format {
        OutputFormatArg::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(response).map_err(|error| {
                    CliError::Runtime(format!("Failed to render JSON output: {error}"))
                })?
            );
        }
        OutputFormatArg::Simple => match &response.result {
            SearchOutput::Text(text) => println!("{text}"),
            SearchOutput::Texts(items) => {
                for item in items {
                    println!("{item}");
                }
            }
            SearchOutput::Items(items) => {
                for item in items {
                    println!("{}", item.payload);
                }
            }
            other => println!("{:?}", other),
        },
        OutputFormatArg::Pretty => match &response.result {
            SearchOutput::Text(text) => {
                println!("Response: {text}");
            }
            SearchOutput::Texts(items) => {
                if items.is_empty() {
                    println!("No results found for your query.");
                } else {
                    println!("Found {} result(s):", items.len());
                    for (index, item) in items.iter().enumerate() {
                        println!("{}. {}", index + 1, item);
                    }
                }
            }
            SearchOutput::Items(items) => {
                if items.is_empty() {
                    println!("No results found for your query.");
                } else {
                    println!("Found {} result(s):", items.len());
                    for (index, item) in items.iter().enumerate() {
                        println!("Result {}:", index + 1);
                        println!("  Score: {:?}", item.score);
                        println!("  Payload: {}", item.payload);
                    }
                }
            }
            SearchOutput::GraphQueryRows(rows) => {
                if rows.is_empty() {
                    println!("No rows returned.");
                } else {
                    println!("Returned {} row(s):", rows.len());
                    for (index, row) in rows.iter().enumerate() {
                        println!(
                            "Row {}: {}",
                            index + 1,
                            serde_json::Value::Array(row.clone())
                        );
                    }
                }
            }
            SearchOutput::Rules(rules) => {
                if rules.is_empty() {
                    println!("No rules returned.");
                } else {
                    println!("Found {} rule(s):", rules.len());
                    for (index, rule) in rules.iter().enumerate() {
                        println!("{}. [{}] {}", index + 1, rule.node_set, rule.text);
                    }
                }
            }
            SearchOutput::Ack { message } => println!("{message}"),
            // A CODE result: each operation result, then its diagram fenced
            // (Python's `print_recall_results` -> `print_code_results`).
            SearchOutput::Structured(value) if response.search_type == SearchType::Code => {
                print!("{}", pretty_code_output(value));
            }
            SearchOutput::Structured(value) => println!("{}", value),
        },
    }

    Ok(())
}

/// A search failure as a CLI error: a CODE operation or argument the code
/// graph refused, and arguments that do not fit together (Python's
/// `CogneeValidationError` / `QueryValidationError`, e.g. `--code-query`
/// without `-t CODE`), are the caller's (a validation error, as recall
/// reports it); anything else is a runtime failure.
fn search_error(error: SearchError) -> CliError {
    match error {
        SearchError::Code(error) => CliError::Validation(error.to_string()),
        SearchError::QueryValidation(message) => CliError::Validation(message),
        other => CliError::Runtime(format!("Search execution failed: {other}")),
    }
}

/// The pretty text of a CODE search result: Python's `_print_graph_results`
/// header for the CODE type (one result per dataset searched), then
/// [`crate::code_search::render_code_results`]; the JSON itself when it holds
/// no operation result.
fn pretty_code_output(value: &serde_json::Value) -> String {
    let count = match value {
        serde_json::Value::Array(results) => results.len(),
        _ => 1,
    };
    match crate::code_search::render_code_results(value) {
        Some(text) => format!(
            "\nFound {count} result(s) using CODE:\n{}\n{text}",
            "=".repeat(60)
        ),
        None => format!(
            "{}\n",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        ),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn code_is_the_code_search_type_and_needs_no_llm() {
        assert_eq!(map_query_type(QueryTypeArg::Code), SearchType::Code);
        assert!(!requires_llm(SearchType::Code));
    }

    /// A refused CODE operation is a validation error with the bare
    /// message, as in recall; other failures stay runtime ones.
    #[test]
    fn refused_code_operations_are_validation_errors() {
        use cognee::cognee_code_graph::query::CodeQueryError;
        let refused = CodeQueryError::unknown_operation("Unknown CODE operation 'teleport'.");
        match search_error(SearchError::Code(refused)) {
            CliError::Validation(message) => {
                assert_eq!(message, "Unknown CODE operation 'teleport'.")
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
        let misplaced = "code_query requires query_type=SearchType.CODE.";
        match search_error(SearchError::QueryValidation(misplaced.to_string())) {
            CliError::Validation(message) => assert_eq!(message, misplaced),
            other => panic!("expected a validation error, got {other:?}"),
        }
        match search_error(SearchError::GraphError("down".to_string())) {
            CliError::Runtime(message) => {
                assert_eq!(message, "Search execution failed: graph error: down")
            }
            other => panic!("expected a runtime error, got {other:?}"),
        }
    }

    #[test]
    fn pretty_code_output_prints_each_result_and_its_diagram() {
        let value = serde_json::json!({
            "operation": "architecture",
            "diagram": {"format": "mermaid", "source": "flowchart LR\n  a --> b\n", "nodes": 2, "edges": 1},
        });
        let text = pretty_code_output(&value);
        assert!(
            text.starts_with(&format!(
                "\nFound 1 result(s) using CODE:\n{}\nResult 1: architecture\n",
                "=".repeat(60)
            )),
            "{text}"
        );
        assert!(
            text.contains("```mermaid\nflowchart LR\n  a --> b\n```\n"),
            "{text}"
        );

        // One result per dataset searched.
        let per_dataset = serde_json::json!([
            {"operation": "explore", "seeds": []},
            {"seed_not_found": true, "error": "x", "code": "not_found"},
        ]);
        assert!(pretty_code_output(&per_dataset).starts_with("\nFound 2 result(s) using CODE:"));
        // No operation result: the JSON as is.
        assert_eq!(
            pretty_code_output(&serde_json::json!({"a": 1})),
            "{\n  \"a\": 1\n}\n"
        );
    }
}
