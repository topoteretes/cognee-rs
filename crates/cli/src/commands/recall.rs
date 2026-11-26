use std::sync::Arc;

use cognee::api::{ScopeInput, normalize_scope, recall};
use cognee::database::IngestDb;
use cognee::search::graph_warmup::GraphWarmup;
use cognee::search::{RecallSource, SearchBuilder, SearchType};
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

    // When `--query-type` is omitted, recall auto-routes the search type.
    let (query_type, auto_route): (Option<SearchType>, bool) = match args.query_type {
        Some(qt) => (Some(map_query_type(qt)), false),
        None => (None, true),
    };
    // `--code-query` / `--diagram` / `--diagram-out` need `-t CODE`, and run
    // the query in recall's dedicated `code` source, which no other scope
    // implies (Python's recall command, `recall_command.py:95`, `:137-141`).
    let code_query = crate::code_search::build_code_query(
        &args.code.options(),
        query_type == Some(SearchType::Code),
    )?;
    let scope = effective_scope(&args.scope, code_query.is_some())?;
    let diagram_out = args.code.diagram_out.clone();

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
        let session_manager = Arc::new(cm.settings().session_manager(Arc::clone(&session_store)));

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
                code_query,
                ..Default::default()
            }),
        )
        .await
        .map_err(|error| match error {
            // The caller's own input, refused before any source runs (an
            // unsupported scope, node names with an empty `--node-type`,
            // a code query without the code scope), or a CODE operation or
            // argument the code source refused.
            cognee::api::ApiError::InvalidArgument(message) => CliError::Validation(message),
            cognee::api::ApiError::CodeQuery(error) => CliError::Validation(error.to_string()),
            other => CliError::Runtime(format!("Recall failed: {other}")),
        })?;

        render_output(&result, args.output_format, args.verbose)?;
        // After the results, as Python's `handle_diagram_out` runs.
        for line in
            crate::code_search::handle_diagram_out(&items_json(&result), diagram_out.as_deref())?
        {
            println!("{line}");
        }
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

/// The scope recall runs: `code` alone when a CODE option built a code
/// query (Python's recall command sets `scope=["code"]`,
/// `recall_command.py:137-141`), else the `--scope` flags.
fn effective_scope(
    scope: &[String],
    has_code_query: bool,
) -> Result<Option<Vec<cognee::api::RecallScope>>, CliError> {
    if has_code_query {
        return Ok(Some(vec![cognee::api::RecallScope::Code]));
    }
    recall_scope(scope)
}

fn map_query_type(query_type: QueryTypeArg) -> SearchType {
    match query_type {
        QueryTypeArg::GraphCompletion => SearchType::GraphCompletion,
        QueryTypeArg::HybridCompletion => SearchType::HybridCompletion,
        QueryTypeArg::RagCompletion => SearchType::RagCompletion,
        QueryTypeArg::Chunks => SearchType::Chunks,
        QueryTypeArg::Summaries => SearchType::Summaries,
        // The real CODE type: the graph source runs it pinned, or, with a
        // CODE option, the `code` source does.
        QueryTypeArg::Code => SearchType::Code,
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

/// Every item as its wire form (`source`, `content`, `score`): what
/// `--diagram-out` searches for the diagram.
fn items_json(result: &cognee::api::RecallResult) -> serde_json::Value {
    serde_json::to_value(&result.items).unwrap_or(serde_json::Value::Null)
}

/// Whether an item holds CODE results: the `code` source's facts, or the
/// graph source's answer when it ran the CODE type.
fn is_code_item(item: &cognee::search::RecallItem, search_type_used: Option<SearchType>) -> bool {
    match item.source {
        RecallSource::Code => true,
        RecallSource::Graph => search_type_used == Some(SearchType::Code),
        _ => false,
    }
}

/// The pretty listing: CODE items through the code printer (Python's
/// `_print_graph_results` for the CODE type -> `print_code_results`), the
/// others one line each as before. Code items whose contents hold no
/// operation result fall back to the generic lines.
fn pretty_output(result: &cognee::api::RecallResult) -> String {
    if result.items.is_empty() {
        return "No results found for your query.\n".to_string();
    }
    let (code, other): (Vec<_>, Vec<_>) = result
        .items
        .iter()
        .partition(|item| is_code_item(item, result.search_type_used));
    let code_contents =
        serde_json::Value::Array(code.iter().map(|item| item.content.clone()).collect());
    let rendered = (!code.is_empty())
        .then(|| crate::code_search::render_code_results(&code_contents))
        .flatten();
    let listed: Vec<&cognee::search::RecallItem> = match rendered {
        Some(_) => other,
        None => result.items.iter().collect(),
    };
    let mut out = String::new();
    if !listed.is_empty() {
        out.push_str(&format!("Found {} result(s):\n", listed.len()));
        for (index, item) in listed.iter().enumerate() {
            out.push_str(&format!(
                "{}. [{}] {}\n",
                index + 1,
                item.source.as_str(),
                item.content
            ));
        }
    }
    if let Some(text) = rendered {
        out.push_str(&format!(
            "\nFound {} result(s) using CODE:\n{}\n{text}",
            code.len(),
            "=".repeat(60)
        ));
    }
    out
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
        OutputFormatArg::Pretty => print!("{}", pretty_output(result)),
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

    /// `test_recall_command_routes_code_query_through_the_code_scope`: a
    /// CODE option runs the `code` source alone, whatever `--scope` says;
    /// `--scope code` is a valid scope of its own.
    #[test]
    fn a_code_query_runs_the_code_scope() {
        assert_eq!(
            effective_scope(&owned(&["graph"]), true).unwrap(),
            Some(vec![RecallScope::Code])
        );
        assert_eq!(
            effective_scope(&owned(&["graph", "code"]), false).unwrap(),
            Some(vec![RecallScope::Graph, RecallScope::Code])
        );
        assert_eq!(effective_scope(&[], false).unwrap(), None);
        assert_eq!(map_query_type(QueryTypeArg::Code), SearchType::Code);
    }

    /// The recall and search parsers take the CODE options.
    #[test]
    fn recall_and_search_parse_the_code_options() {
        use clap::Parser;

        use crate::cli::{Cli, Commands};
        use crate::code_search::DiagramFormatArg;
        let cli = Cli::try_parse_from([
            "cognee-cli",
            "recall",
            "",
            "-t",
            "CODE",
            "--code-query",
            r#"{"operation": "architecture"}"#,
            "--diagram",
            "dot",
            "--diagram-out",
            "out/arch.svg",
        ])
        .expect("parses");
        let Commands::Recall(args) = cli.command else {
            panic!("a recall command");
        };
        assert_eq!(
            args.code.code_query.as_deref(),
            Some(r#"{"operation": "architecture"}"#)
        );
        assert_eq!(args.code.diagram, Some(DiagramFormatArg::Dot));
        assert_eq!(
            args.code.diagram_out.as_deref(),
            Some(std::path::Path::new("out/arch.svg"))
        );

        let cli = Cli::try_parse_from(["cognee-cli", "search", "q", "--diagram", "mermaid"])
            .expect("parses");
        let Commands::Search(args) = cli.command else {
            panic!("a search command");
        };
        assert_eq!(args.code.diagram, Some(DiagramFormatArg::Mermaid));
        assert!(
            Cli::try_parse_from(["cognee-cli", "search", "q", "--diagram", "svg"]).is_err(),
            "only mermaid and dot"
        );
    }

    fn code_item(operation: &str) -> cognee::search::RecallItem {
        cognee::search::RecallItem {
            source: RecallSource::Code,
            content: serde_json::json!({
                "kind": "code",
                "text": "x",
                "raw": {
                    "operation": operation,
                    "diagram": {"format": "mermaid", "source": "flowchart LR\n", "nodes": 0, "edges": 0},
                },
            }),
            score: 1.0,
        }
    }

    /// Code facts go through the code printer, the other sources keep their
    /// one-line listing.
    #[test]
    fn pretty_output_prints_code_results_with_their_diagram() {
        let mut result = result_with_response();
        result.items.push(code_item("architecture"));
        let text = pretty_output(&result);
        assert!(
            text.starts_with("Found 1 result(s):\n1. [graph] \"answer\"\n"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "Found 1 result(s) using CODE:\n{}\nResult 1: architecture\n",
                "=".repeat(60)
            )),
            "{text}"
        );
        assert!(text.contains("```mermaid\nflowchart LR\n```"), "{text}");

        // A graph answer of the CODE type is a code result too.
        let graph_code = cognee::api::RecallResult {
            items: vec![cognee::search::RecallItem {
                source: RecallSource::Graph,
                content: serde_json::json!({"operation": "explore", "seeds": []}),
                score: 1.0,
            }],
            search_type_used: Some(SearchType::Code),
            auto_routed: false,
            search_response: None,
        };
        assert!(pretty_output(&graph_code).starts_with("\nFound 1 result(s) using CODE:"));

        let empty = cognee::api::RecallResult {
            items: Vec::new(),
            ..graph_code
        };
        assert_eq!(pretty_output(&empty), "No results found for your query.\n");
        // Without code results the listing is unchanged.
        assert_eq!(
            pretty_output(&result_with_response()),
            "Found 1 result(s):\n1. [graph] \"answer\"\n"
        );
    }

    #[test]
    fn diagram_out_finds_the_diagram_in_the_items() {
        let mut result = result_with_response();
        result.items.push(code_item("architecture"));
        let items = items_json(&result);
        assert_eq!(
            crate::code_search::first_diagram(&items).map(|diagram| diagram["format"].clone()),
            Some(serde_json::json!("mermaid"))
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
