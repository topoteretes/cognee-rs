use std::sync::Arc;

use cognee::api::{ScopeInput, normalize_scope, recall};
use cognee::database::IngestDb;
use cognee::search::graph_warmup::GraphWarmup;
use cognee::search::{SearchBuilder, SearchType, SessionManager};
use cognee::session::SessionStore;
use cognee::{ComponentManager, PipelineContext};
use uuid::Uuid;

use crate::cli::{OutputFormatArg, QueryTypeArg, RecallArgs};
use crate::error::CliError;

pub fn run(args: RecallArgs, cm: Arc<ComponentManager>) -> Result<(), CliError> {
    let settings = cm.settings();
    let log_search_history = settings.log_search_history;

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
    let tenant_id = crate::commands::parse_tenant_id(args.tenant_id.as_deref())?;
    let scope = recall_scope(&args.scope)?;

    // When `--query-type` is omitted, recall auto-routes the search type.
    let (query_type, auto_route): (Option<SearchType>, bool) = match args.query_type {
        Some(qt) => (Some(map_query_type(qt)), false),
        None => (None, true),
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

        let session_store: Arc<dyn SessionStore> = cm
            .session_store()
            .await
            .map_err(|e| CliError::Runtime(format!("session store init failed: {e}")))?;
        let feedback_llm_timeout = cm.settings().feedback_llm_timeout();
        let session_manager = Arc::new(
            SessionManager::new(Arc::clone(&session_store))
                .with_feedback_llm_timeout(feedback_llm_timeout),
        );

        let search_tuning = cm.settings().search_tuning();
        // Per-dataset storage: every dataset is searched in its own databases.
        let per_dataset_storage = cm
            .dataset_storage()
            .await
            .map_err(|e| CliError::Runtime(format!("{e}")))?
            .is_some();
        let graph_warmup = Arc::new(GraphWarmup::new(
            Arc::clone(&database).pipeline_runs(),
            cm.settings().recall_warmup_config(),
        ));
        let orchestrator =
            SearchBuilder::new(vector_db, embedding_engine, graph_db, llm, database.clone())
                .with_tuning(search_tuning)
                .with_session_manager(Arc::clone(&session_manager))
                .with_dataset_resolver(database as Arc<dyn IngestDb>)
                .with_search_history_logging(log_search_history)
                .with_per_dataset_storage(per_dataset_storage)
                .with_graph_warmup(graph_warmup)
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
            None,
            args.top_k,
            auto_route,
            args.session_id.as_deref(),
            Some(user_id_str.as_str()),
            &orchestrator,
            Some(session_store.as_ref()),
            Some(session_manager.as_ref()),
            scope,
            // Only the flags this command exposes. Always `Some`: recall reads
            // every option field-by-field, so an all-default value means the
            // same as `None` and a new flag cannot be dropped by a stale guard.
            Some(cognee::api::RecallOptions {
                tenant_id,
                neighborhood_depth: args.neighborhood_depth.map(usize::from),
                neighborhood_seed_top_k: args.neighborhood_seed_top_k.map(usize::from),
                tools_trigger: args.tools_trigger,
                context_profile: args.context_profile,
                node_type: args.node_type,
                node_name: (!args.node_name.is_empty()).then_some(args.node_name),
                verbose: args.verbose.then_some(true),
                ..Default::default()
            }),
        )
        .await
        .map_err(|error| match error {
            // The caller's own input, refused before any source runs (an
            // unsupported scope, node names with an empty `--node-type`).
            cognee::api::ApiError::InvalidArgument(message) => CliError::Validation(message),
            other => CliError::Runtime(format!("Recall failed: {other}")),
        })?;

        render_output(&result, args.output_format, args.verbose)?;
        Ok(())
    })
}

/// The repeated `--scope` values, normalized as every other recall boundary
/// does. None given is `None`, which recall treats as `auto`.
fn recall_scope(scope: &[String]) -> Result<Option<Vec<cognee::api::RecallScope>>, CliError> {
    if scope.is_empty() {
        return Ok(None);
    }
    normalize_scope(Some(ScopeInput::Many(scope.to_vec())))
        .map(Some)
        .map_err(|error| CliError::Validation(format!("Invalid --scope: {error}")))
}

fn map_query_type(query_type: QueryTypeArg) -> SearchType {
    match query_type {
        QueryTypeArg::GraphCompletion => SearchType::GraphCompletion,
        QueryTypeArg::HybridCompletion => SearchType::HybridCompletion,
        QueryTypeArg::RagCompletion => SearchType::RagCompletion,
        QueryTypeArg::Chunks => SearchType::Chunks,
        QueryTypeArg::Summaries => SearchType::Summaries,
        QueryTypeArg::Code => SearchType::CodingRules,
        QueryTypeArg::Cypher => SearchType::Cypher,
        QueryTypeArg::Temporal => SearchType::Temporal,
    }
}

/// The graph search's response, printed after the items under `--verbose`:
/// that flag keeps the response's context and graph, which the items alone
/// do not show. `None` without the flag or when the graph source did not run.
fn verbose_response(
    result: &cognee::api::RecallResult,
    verbose: bool,
) -> Result<Option<serde_json::Value>, CliError> {
    match (&result.search_response, verbose) {
        (Some(response), true) => serde_json::to_value(response).map(Some).map_err(|error| {
            CliError::Runtime(format!("Failed to render the search response: {error}"))
        }),
        _ => Ok(None),
    }
}

/// The `--output-format json` document: the item contents, or under
/// `--verbose` an object with those `items` and the graph `search_response`.
fn json_output(
    result: &cognee::api::RecallResult,
    verbose: bool,
) -> Result<serde_json::Value, CliError> {
    let items: Vec<&serde_json::Value> = result.items.iter().map(|i| &i.content).collect();
    Ok(match verbose_response(result, verbose)? {
        Some(response) => serde_json::json!({ "items": items, "search_response": response }),
        None => serde_json::json!(items),
    })
}

fn render_output(
    result: &cognee::api::RecallResult,
    output_format: OutputFormatArg,
    verbose: bool,
) -> Result<(), CliError> {
    // Recall results are the command's primary output and must reach stdout
    // regardless of the active log level, so they use `println!` rather than
    // the tracing logger (matches `search`).
    let render = |value: &serde_json::Value| {
        serde_json::to_string_pretty(value)
            .map_err(|error| CliError::Runtime(format!("Failed to render JSON output: {error}")))
    };
    match output_format {
        OutputFormatArg::Json => {
            println!("{}", render(&json_output(result, verbose)?)?);
            return Ok(());
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
    if let Some(response) = verbose_response(result, verbose)? {
        println!("Search response:\n{}", render(&response)?);
    }

    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee::api::RecallScope;

    use super::*;

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn no_scope_flag_is_auto() {
        assert_eq!(recall_scope(&[]).expect("no flag parses"), None);
    }

    #[test]
    fn scope_flags_are_normalized() {
        assert_eq!(
            recall_scope(&owned(&["session_first", "graph_context"])).expect("known names parse"),
            Some(vec![RecallScope::SessionFirst, RecallScope::Graph])
        );
        assert_eq!(
            recall_scope(&owned(&["all"])).expect("all parses"),
            Some(RecallScope::ALL.to_vec())
        );
    }

    fn result_with_response() -> cognee::api::RecallResult {
        cognee::api::RecallResult {
            items: vec![cognee::search::RecallItem {
                source: cognee::search::RecallSource::Graph,
                content: serde_json::json!("answer"),
                score: 1.0,
            }],
            search_type_used: Some(SearchType::GraphCompletion),
            auto_routed: false,
            search_response: Some(
                serde_json::from_value(serde_json::json!({
                    "search_type": "GRAPH_COMPLETION",
                    "result": { "kind": "Text", "data": "answer" },
                    "context": {
                        "all": [{ "id": null, "score": null, "payload": "the context" }]
                    },
                    "graphs": null,
                    "diagnostics": null,
                    "datasets": null,
                    "only_context": false,
                    "use_combined_context": false,
                    "verbose": true,
                }))
                .expect("a valid search response"),
            ),
        }
    }

    /// Without `--verbose` the JSON output is the item contents, as before;
    /// with it, the graph search's response (its context) comes along.
    #[test]
    fn verbose_json_output_carries_the_search_response() {
        let result = result_with_response();
        assert_eq!(
            json_output(&result, false).expect("renders"),
            serde_json::json!(["answer"])
        );
        let verbose = json_output(&result, true).expect("renders");
        assert_eq!(verbose["items"], serde_json::json!(["answer"]));
        assert_eq!(verbose["search_response"]["verbose"], true);
        assert!(verbose["search_response"]["context"].is_object());

        let no_graph = cognee::api::RecallResult {
            search_response: None,
            ..result_with_response()
        };
        assert_eq!(
            json_output(&no_graph, true).expect("renders"),
            serde_json::json!(["answer"])
        );
    }

    #[test]
    fn an_unknown_scope_is_a_validation_error() {
        match recall_scope(&owned(&["everything"])) {
            Err(CliError::Validation(message)) => {
                assert!(message.starts_with("Invalid --scope: "), "{message}")
            }
            other => panic!("expected a validation error, got {other:?}"),
        }
    }
}
