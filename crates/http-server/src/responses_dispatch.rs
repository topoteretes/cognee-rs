//! Function-call dispatcher for `POST /api/v1/responses`.
//!
//! Mirrors Python's [`cognee.api.v1.responses.dispatch_function.dispatch_function`].
//! When the OpenAI Responses API returns a function-call item in `output`, this
//! module routes it to the in-process pipeline (search / cognify) and returns
//! a JSON-serialisable result string the handler can fold back into the
//! response body as a `ToolCallOutput`.

use std::sync::Arc;

use serde_json::{Value, json};
use tracing::warn;

use cognee_search::types::{SearchOutput, SearchRequest, SearchType};

use crate::auth::AuthenticatedUser;
use crate::components::ComponentHandles;

/// Default tools advertised to the upstream OpenAI Responses API.
///
/// Wire-shape parity with Python's
/// [`cognee.api.v1.responses.default_tools.DEFAULT_TOOLS`]. The `"prune"` tool
/// is intentionally omitted (commented as dangerous in the Python source).
pub fn default_tools() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "name": "search",
            "description": "Search for information within the knowledge graph",
            "parameters": {
                "type": "object",
                "properties": {
                    "search_query": {
                        "type": "string",
                        "description": "The query to search for in the knowledge graph",
                    },
                    "search_type": {
                        "type": "string",
                        "description": "Type of search to perform",
                        "enum": ["CODE", "GRAPH_COMPLETION", "NATURAL_LANGUAGE"],
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Maximum number of results to return",
                        "default": 10,
                    },
                    "datasets": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional list of dataset names to search within",
                    },
                },
                "required": ["search_query"],
            },
        }),
        json!({
            "type": "function",
            "name": "cognify",
            "description": "Convert text into a knowledge graph or process all added content",
            "parameters": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text content to be converted into a knowledge graph",
                    },
                    "ontology_file_path": {
                        "type": "string",
                        "description": "Path to a custom ontology file",
                    },
                    "custom_prompt": {
                        "type": "string",
                        "description": "Custom prompt for entity extraction and graph generation. If provided, this prompt will be used instead of the default prompts.",
                    },
                },
                "required": ["text"],
            },
        }),
    ]
}

/// A parsed function-call item extracted from the Responses API `output` array.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// `call_id` from the upstream output (or a generated `call_*` fallback).
    pub id: String,
    /// Function name.
    pub name: String,
    /// JSON-string arguments. Stored as a string for wire-parity with Python.
    pub arguments: String,
}

/// Outcome of dispatching one tool call.
#[derive(Debug, Clone)]
pub struct ToolDispatchResult {
    /// `"success"` or `"error"` — fed into `ToolCallOutputDTO.status`.
    pub status: String,
    /// The function's structured result, wrapped in `{"result": ...}` to match
    /// Python's `ToolCallOutput(data={"result": function_result})` shape.
    pub data: Value,
}

impl ToolDispatchResult {
    fn success(result: Value) -> Self {
        Self {
            status: "success".into(),
            data: json!({ "result": result }),
        }
    }

    fn error(msg: impl Into<String>) -> Self {
        Self {
            status: "error".into(),
            data: json!({ "result": msg.into() }),
        }
    }
}

/// Trait used to fan out tool calls. Implementations may run real cognee
/// pipelines or canned stubs (the mocked dispatcher tests use a stub
/// `ToolDispatcher`).
#[async_trait::async_trait]
pub trait ToolDispatcher: Send + Sync {
    async fn dispatch_search(
        &self,
        arguments: &Value,
        user: &AuthenticatedUser,
    ) -> ToolDispatchResult;

    async fn dispatch_cognify(
        &self,
        arguments: &Value,
        user: &AuthenticatedUser,
    ) -> ToolDispatchResult;
}

/// Production dispatcher backed by the wired `ComponentHandles`.
///
/// `search` is routed to the `SearchOrchestrator`; `cognify` returns an
/// explicit `error` `ToolDispatchResult` pending end-to-end wiring of the
/// cognify pipeline (see [`Self::dispatch_cognify`]).
pub struct ComponentHandlesDispatcher {
    components: Arc<ComponentHandles>,
}

impl ComponentHandlesDispatcher {
    pub fn new(components: Arc<ComponentHandles>) -> Self {
        Self { components }
    }
}

#[async_trait::async_trait]
impl ToolDispatcher for ComponentHandlesDispatcher {
    async fn dispatch_search(
        &self,
        arguments: &Value,
        user: &AuthenticatedUser,
    ) -> ToolDispatchResult {
        let Some(query) = arguments
            .get("search_query")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return ToolDispatchResult::error(
                "Error: Missing required 'search_query' parameter".to_string(),
            );
        };

        let search_type = parse_search_type(arguments.get("search_type").and_then(Value::as_str));
        let top_k = arguments
            .get("top_k")
            .and_then(Value::as_i64)
            .and_then(|n| if n > 0 { Some(n as usize) } else { None })
            .or(Some(10));
        let datasets = arguments
            .get("datasets")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            });

        let Some(orchestrator) = self.components.search_orchestrator.clone() else {
            return ToolDispatchResult::error(
                "Error: search orchestrator is not wired in this build",
            );
        };

        let request = SearchRequest {
            query_text: query.to_string(),
            search_type,
            top_k,
            datasets,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: Some("answer_simple_question.txt".to_string()),
            only_context: None,
            use_combined_context: None,
            session_id: None,
            node_type: None,
            node_name: None,
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            user_id: Some(user.id),
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
        };

        match orchestrator.search(&request).await {
            Ok(response) => ToolDispatchResult::success(search_output_to_json(response.result)),
            Err(e) => ToolDispatchResult::error(format!("Error executing search: {e}")),
        }
    }

    async fn dispatch_cognify(
        &self,
        arguments: &Value,
        _user: &AuthenticatedUser,
    ) -> ToolDispatchResult {
        // KNOWN PARITY GAP — see docs/http-server/gaps/impl/07-responses-openai.md
        // followup. Python's `handle_cognify` calls `add()` + `cognify()` end
        // to end (`/tmp/cognee-python/cognee/api/v1/responses/dispatch_function.py:87-106`).
        // Wiring that here requires the same multi-handle plumbing the
        // `remember.rs` router uses (graph_db / vector_db / embedding_engine /
        // thread_pool, plus an ingestion pipeline). Scoping that into the
        // tool-dispatch surface is tracked as a follow-up; for now we return
        // an explicit error so the upstream model is not silently misled
        // into believing the knowledge graph has been updated.
        let _text = arguments
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let _ = self.components.database.as_ref();
        ToolDispatchResult::error(
            "Error: cognify tool dispatch is not yet wired in this build; \
             call POST /api/v1/cognify directly",
        )
    }
}

/// Parse a `SearchType` string from the tool arguments. Falls back to
/// `GraphCompletion` for unknown or missing values (matches Python parity).
fn parse_search_type(s: Option<&str>) -> SearchType {
    let raw = s.unwrap_or("GRAPH_COMPLETION");
    if raw == "CODE" {
        // Python's tool description advertises "CODE" but there is no such
        // SearchType variant in either runtime today — fall back to the
        // default (Python uses the same default for unrecognised values).
        warn!(
            value = raw,
            "responses tool: 'CODE' not supported, falling back to GRAPH_COMPLETION"
        );
        return SearchType::GraphCompletion;
    }
    serde_json::from_value::<SearchType>(Value::String(raw.to_string())).unwrap_or_else(|_| {
        warn!(
            value = raw,
            "responses tool: invalid search_type, defaulting to GRAPH_COMPLETION"
        );
        SearchType::GraphCompletion
    })
}

/// Flatten a `SearchOutput` into a single JSON value for the tool result.
fn search_output_to_json(out: SearchOutput) -> Value {
    match out {
        SearchOutput::Text(s) => Value::String(s),
        SearchOutput::Texts(v) => Value::Array(v.into_iter().map(Value::String).collect()),
        SearchOutput::Items(items) => Value::Array(items.into_iter().map(|i| i.payload).collect()),
        SearchOutput::GraphQueryRows(rows) => Value::Array(
            rows.into_iter()
                .map(|row| Value::Array(row.into_iter().collect()))
                .collect(),
        ),
        SearchOutput::Rules(rules) => Value::Array(
            rules
                .into_iter()
                .map(|r| json!({"node_set": r.node_set, "text": r.text}))
                .collect(),
        ),
        SearchOutput::Ack { message } => json!({"message": message}),
        SearchOutput::Structured(v) => v,
    }
}

/// Extract function-call entries from a Responses API `output` array.
///
/// Mirrors Python's iteration in `get_responses_router.py:124-138` which
/// inspects each `output` item for `type == "function_call"`.
pub fn extract_tool_calls(output: &Value) -> Vec<ToolCall> {
    let Some(items) = output.as_array() else {
        return Vec::new();
    };
    let mut calls = Vec::new();
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}")
            .to_string();
        let id = item
            .get("call_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()));
        calls.push(ToolCall {
            id,
            name,
            arguments,
        });
    }
    calls
}

/// Dispatch a single tool call to the appropriate handler.
pub async fn dispatch_one(
    call: &ToolCall,
    dispatcher: &dyn ToolDispatcher,
    user: &AuthenticatedUser,
) -> ToolDispatchResult {
    let parsed_args: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
    match call.name.as_str() {
        "search" => dispatcher.dispatch_search(&parsed_args, user).await,
        "cognify" => dispatcher.dispatch_cognify(&parsed_args, user).await,
        other => ToolDispatchResult::error(format!("Error: Unknown function {other}")),
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
    use uuid::Uuid;

    fn fake_user() -> AuthenticatedUser {
        AuthenticatedUser {
            id: Uuid::new_v4(),
            email: "t@example.com".into(),
            is_superuser: false,
            is_verified: true,
            is_active: true,
            tenant_id: Some(Uuid::new_v4()),
            auth_method: crate::auth::AuthMethod::DefaultUser,
        }
    }

    struct StubDispatcher {
        search_result: Value,
        cognify_result: Value,
    }

    #[async_trait::async_trait]
    impl ToolDispatcher for StubDispatcher {
        async fn dispatch_search(
            &self,
            arguments: &Value,
            _user: &AuthenticatedUser,
        ) -> ToolDispatchResult {
            // Echo back arguments so tests can verify the dispatcher routed
            // the right call.
            ToolDispatchResult::success(json!({
                "echo_args": arguments.clone(),
                "result": self.search_result.clone(),
            }))
        }

        async fn dispatch_cognify(
            &self,
            arguments: &Value,
            _user: &AuthenticatedUser,
        ) -> ToolDispatchResult {
            ToolDispatchResult::success(json!({
                "echo_args": arguments.clone(),
                "result": self.cognify_result.clone(),
            }))
        }
    }

    #[test]
    fn default_tools_contains_search_and_cognify() {
        let tools = default_tools();
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert!(names.contains(&"search"));
        assert!(names.contains(&"cognify"));
        // Prune is intentionally omitted per Python parity.
        assert!(!names.contains(&"prune"));
    }

    #[test]
    fn extract_tool_calls_picks_up_function_call_items() {
        let output = json!([
            {"type": "message", "content": "hi"},
            {
                "type": "function_call",
                "name": "search",
                "arguments": "{\"search_query\":\"alice\"}",
                "call_id": "call_abc"
            },
            {
                "type": "function_call",
                "name": "cognify",
                "arguments": "{\"text\":\"foo\"}"
            }
        ]);
        let calls = extract_tool_calls(&output);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[0].id, "call_abc");
        assert!(calls[0].arguments.contains("alice"));
        assert_eq!(calls[1].name, "cognify");
        // Synthesised id when `call_id` is missing.
        assert!(calls[1].id.starts_with("call_"));
    }

    #[test]
    fn extract_tool_calls_on_non_array_returns_empty() {
        let calls = extract_tool_calls(&json!({"foo": "bar"}));
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn dispatch_one_routes_search_to_dispatcher() {
        let stub = StubDispatcher {
            search_result: json!("from-search"),
            cognify_result: json!("from-cognify"),
        };
        let call = ToolCall {
            id: "c1".into(),
            name: "search".into(),
            arguments: r#"{"search_query":"q","top_k":3}"#.into(),
        };
        let user = fake_user();
        let out = dispatch_one(&call, &stub, &user).await;
        assert_eq!(out.status, "success");
        // `data.result` is wrapped in `{"result": ...}`, so the stub's
        // `result.result` lives at `data.result.result`.
        assert_eq!(out.data["result"]["result"], "from-search");
        assert_eq!(out.data["result"]["echo_args"]["search_query"], "q");
        assert_eq!(out.data["result"]["echo_args"]["top_k"], 3);
    }

    #[tokio::test]
    async fn dispatch_one_routes_cognify_to_dispatcher() {
        let stub = StubDispatcher {
            search_result: json!("ignored"),
            cognify_result: json!("from-cognify"),
        };
        let call = ToolCall {
            id: "c2".into(),
            name: "cognify".into(),
            arguments: r#"{"text":"hello"}"#.into(),
        };
        let user = fake_user();
        let out = dispatch_one(&call, &stub, &user).await;
        assert_eq!(out.status, "success");
        assert_eq!(out.data["result"]["result"], "from-cognify");
        assert_eq!(out.data["result"]["echo_args"]["text"], "hello");
    }

    #[tokio::test]
    async fn dispatch_one_unknown_function_returns_error() {
        let stub = StubDispatcher {
            search_result: json!("x"),
            cognify_result: json!("x"),
        };
        let call = ToolCall {
            id: "c3".into(),
            name: "prune".into(),
            arguments: "{}".into(),
        };
        let user = fake_user();
        let out = dispatch_one(&call, &stub, &user).await;
        assert_eq!(out.status, "error");
        assert!(
            out.data["result"]
                .as_str()
                .expect("error msg")
                .contains("Unknown function")
        );
    }

    #[tokio::test]
    async fn dispatch_one_malformed_arguments_becomes_empty_object() {
        // Malformed JSON in `arguments` must not panic — Python parses with
        // `json.loads` and would raise, but we want a defensive default so a
        // single malformed call doesn't kill the whole response.
        let stub = StubDispatcher {
            search_result: json!("ok"),
            cognify_result: json!("ok"),
        };
        let call = ToolCall {
            id: "c4".into(),
            name: "search".into(),
            arguments: "not json".into(),
        };
        let user = fake_user();
        let out = dispatch_one(&call, &stub, &user).await;
        // The stub dispatcher succeeds; the missing search_query is caught
        // by the real dispatcher, not the stub. Either way, no panic.
        assert_eq!(out.status, "success");
    }

    #[test]
    fn parse_search_type_handles_known_values() {
        assert_eq!(
            parse_search_type(Some("GRAPH_COMPLETION")),
            SearchType::GraphCompletion
        );
        assert_eq!(
            parse_search_type(Some("NATURAL_LANGUAGE")),
            SearchType::NaturalLanguage
        );
    }

    #[test]
    fn parse_search_type_falls_back_on_unknown() {
        // "CODE" appears in the Python tool description but no SearchType
        // variant exists — falls back to GraphCompletion.
        assert_eq!(parse_search_type(Some("CODE")), SearchType::GraphCompletion);
        assert_eq!(
            parse_search_type(Some("UNKNOWN_X")),
            SearchType::GraphCompletion
        );
        assert_eq!(parse_search_type(None), SearchType::GraphCompletion);
    }

    #[test]
    fn search_output_to_json_handles_each_variant() {
        let v = search_output_to_json(SearchOutput::Text("hi".into()));
        assert_eq!(v, json!("hi"));

        let v = search_output_to_json(SearchOutput::Items(vec![]));
        assert!(v.is_array());

        let v = search_output_to_json(SearchOutput::Structured(json!({"k":"v"})));
        assert_eq!(v["k"], "v");
    }
}
