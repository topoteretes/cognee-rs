use std::sync::Arc;

use async_trait::async_trait;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_models::{INTERNAL_PROPERTY, is_internal_node};
use cognee_session::SessionContext;
use serde_json::{Value, json};

use crate::retrievers::SearchRetriever;
use crate::types::{
    SearchContext, SearchError, SearchItem, SearchOutput, SearchParams, SearchType,
};

const DEFAULT_NL_MAX_ATTEMPTS: usize = 3;
const NL_SYSTEM_PROMPT_TEMPLATE: &str = "\
You are an expert Neo4j Cypher query generator tasked with translating natural language questions into precise, optimized Cypher queries.

TASK:
Generate a valid, executable Cypher query that accurately answers the user's question based on the provided graph schema.

GRAPH SCHEMA INFORMATION:
- You will be given node labels and their properties in format: NodeLabels [list of properties]
- You will be given relationship types between nodes
- ONLY use node labels, properties, and relationship types that exist in the provided schema
- Respect relationship directions (source→target) exactly as specified in the schema
- Properties may have specific formats (e.g., dates, codes) - infer these from examples when possible

QUERY REQUIREMENTS:
1. Return ONLY the exact Cypher query with NO explanations, comments, or markdown
2. Generate syntactically correct Neo4j Cypher code (Neo4j 4.4+ compatible)
3. Be precise - match the exact property names and relationship types from the schema
4. Handle complex queries by breaking them into logical pattern matching parts
5. Use parameters (e.g., $name) for literal values when appropriate
6. Use appropriate data types for parameters (strings, numbers, booleans)

PERFORMANCE OPTIMIZATION:
1. Use indexes and constraints when available (assume they exist on ID properties)
2. Include LIMIT clauses for queries that could return large result sets
3. Use efficient patterns - avoid unnecessary pattern complexity
4. Consider using OPTIONAL MATCH for parts that might not exist
5. For aggregation, use efficient aggregation functions (count, sum, avg)
6. For pathfinding, consider using shortestPath() or apoc.algo.* procedures

ERROR PREVENTION:
1. Validate your query steps mentally before finalizing
2. Ensure relationship directions match schema
3. Check property names match exactly what's in the schema
4. Use pattern variables consistently throughout the query
5. If previous attempts failed, analyze the failures and adjust your approach

Node schemas:
- EntityType
Properties: description, ontology_valid, name, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents the categories or classifications for entities in the database.

- Entity
Properties: description, ontology_valid, name, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents individual entities that belong to a specific type or classification.

- TextDocument
Properties: raw_data_location, name, mime_type, external_metadata, created_at, type, version, topological_rank, updated_at, metadata, id
Purpose: Represents documents containing text data, along with metadata about their storage and format.

- DocumentChunk
Properties: version, created_at, type, topological_rank, cut_type, text, metadata, chunk_index, chunk_size, updated_at, id
Purpose: Represents segmented portions of larger documents, useful for processing or analysis at a more granular level.

- TextSummary
Properties: topological_rank, metadata, id, type, updated_at, created_at, text, version
Purpose: Represents summarized content generated from larger text documents, retaining essential information and metadata.

Edge schema (relationship properties):
`{edge_schemas}`

This queries doesn't work. Do NOT use them:
`{previous_attempts}`

Example 1:
Get all nodes connected to John
MATCH (n:Entity {'name': 'John'})--(neighbor)
RETURN n, neighbor";

/// Refuse when graph-query search types are disabled (`search.allow_cypher_query`,
/// Python's `ALLOW_CYPHER_QUERY`).
fn ensure_cypher_queries_enabled(allowed: bool) -> Result<(), SearchError> {
    if allowed {
        Ok(())
    } else {
        Err(SearchError::InvalidInput(
            "Cypher query search types are disabled (search.allow_cypher_query / \
             ALLOW_CYPHER_QUERY=false)"
                .to_string(),
        ))
    }
}

/// True for a node-schema row that describes internal nodes — one whose
/// collected property list holds the `is_internal` key.
///
/// Port of `_is_internal_schema_row` (`natural_language_retriever.py:16-37`).
/// On backends with per-node property keys (Neo4j) internal nodes are the only
/// ones carrying the key; on fixed-column backends (Ladybug/Kuzu) the marker
/// lives inside the serialized properties and this is a no-op.
fn is_internal_schema_row(row: &[Value]) -> bool {
    row.iter().any(|value| match value {
        Value::String(s) => s == INTERNAL_PROPERTY,
        Value::Array(items) => items
            .iter()
            .any(|item| item.as_str() == Some(INTERNAL_PROPERTY)),
        _ => false,
    })
}

/// True when a raw query-result value contains an internal graph node:
/// a node object marked `is_internal` at any depth, or a serialized JSON node
/// in a string column (Ladybug/Kuzu raw rows).
///
/// Port of `_contains_internal_node` (`natural_language_retriever.py:40-58`).
fn contains_internal_node(value: &Value) -> bool {
    match value {
        Value::Object(map) => is_internal_node(map) || map.values().any(contains_internal_node),
        Value::Array(items) => items.iter().any(contains_internal_node),
        Value::String(s) if s.contains(INTERNAL_PROPERTY) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|parsed| parsed.as_object().map(is_internal_node))
            .unwrap_or(false),
        _ => false,
    }
}

fn rows_to_context(rows: Vec<Vec<Value>>) -> SearchContext {
    rows.into_iter()
        .map(|row| SearchItem {
            id: None,
            score: None,
            payload: json!({ "row": row }),
        })
        .collect()
}

fn context_to_rows(context: SearchContext) -> Vec<Vec<Value>> {
    context
        .into_iter()
        .filter_map(|item| item.payload.get("row").and_then(Value::as_array).cloned())
        .collect()
}

pub struct CypherSearchRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
    allow_cypher_query: bool,
}

impl CypherSearchRetriever {
    pub fn new(graph_db: Arc<dyn GraphDBTrait>) -> Self {
        Self {
            graph_db,
            allow_cypher_query: true,
        }
    }

    /// Allow or refuse running queries (`search.allow_cypher_query`;
    /// allowed by default).
    #[must_use]
    pub fn with_allow_cypher_query(mut self, allowed: bool) -> Self {
        self.allow_cypher_query = allowed;
        self
    }
}

#[async_trait]
impl SearchRetriever for CypherSearchRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::Cypher
    }

    fn supports_session_turn_preparation(&self) -> bool {
        false
    }

    #[cfg(test)]
    fn applied_tuning(&self) -> crate::retrievers::AppliedTuning {
        crate::retrievers::AppliedTuning {
            allow_cypher_query: Some(self.allow_cypher_query),
            ..Default::default()
        }
    }

    async fn get_context(
        &self,
        query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        ensure_cypher_queries_enabled(self.allow_cypher_query)?;

        if self.graph_db.is_empty().await? {
            return Ok(vec![]);
        }

        let rows = self.graph_db.query(query, None).await?;
        Ok(rows_to_context(rows))
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let output_context = match context {
            Some(existing_context) => existing_context,
            None => {
                ensure_cypher_queries_enabled(self.allow_cypher_query)?;
                if self.graph_db.is_empty().await? {
                    vec![]
                } else {
                    let rows = self.graph_db.query(query, None).await?;
                    rows_to_context(rows)
                }
            }
        };

        Ok(SearchOutput::GraphQueryRows(context_to_rows(
            output_context,
        )))
    }
}

pub struct NaturalLanguageRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
    llm: Arc<dyn Llm>,
    max_attempts: usize,
    generation_options: Option<GenerationOptions>,
    allow_cypher_query: bool,
}

impl NaturalLanguageRetriever {
    pub fn new(
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        max_attempts: Option<usize>,
        generation_options: Option<GenerationOptions>,
    ) -> Self {
        Self {
            graph_db,
            llm,
            max_attempts: max_attempts.unwrap_or(DEFAULT_NL_MAX_ATTEMPTS),
            generation_options,
            allow_cypher_query: true,
        }
    }

    /// Allow or refuse running generated queries (`search.allow_cypher_query`;
    /// allowed by default).
    #[must_use]
    pub fn with_allow_cypher_query(mut self, allowed: bool) -> Self {
        self.allow_cypher_query = allowed;
        self
    }

    async fn get_graph_schema(&self) -> Result<(Vec<Vec<Value>>, Vec<Vec<Value>>), SearchError> {
        let node_schemas = self
            .graph_db
            .query(
                "\n            MATCH (n)\n            UNWIND keys(n) AS prop\n            RETURN DISTINCT labels(n) AS NodeLabels, collect(DISTINCT prop) AS Properties;\n            ",
                None,
            )
            .await?;

        let edge_schemas = self
            .graph_db
            .query(
                "\n            MATCH ()-[r]->()\n            UNWIND keys(r) AS key\n            RETURN DISTINCT key;\n            ",
                None,
            )
            .await?;

        // Internal nodes (e.g. Python's per-user preference state) must never
        // be surfaced, so their labels stay out of the schema
        // (`natural_language_retriever.py:106-108`).
        let node_schemas = node_schemas
            .into_iter()
            .filter(|row| !is_internal_schema_row(row))
            .collect();

        Ok((node_schemas, edge_schemas))
    }

    async fn generate_cypher_query(
        &self,
        query: &str,
        edge_schemas: &[Vec<Value>],
        previous_attempts: &str,
    ) -> Result<String, SearchError> {
        let edge_schema_text = serde_json::to_string(edge_schemas)?;
        let system_prompt = NL_SYSTEM_PROMPT_TEMPLATE
            .replace("{edge_schemas}", &edge_schema_text)
            // Python: `previous_attempts or "No attempts yet"`.
            .replace(
                "{previous_attempts}",
                if previous_attempts.is_empty() {
                    "No attempts yet"
                } else {
                    previous_attempts
                },
            );

        let response = self
            .llm
            .generate(
                vec![
                    Message::system(system_prompt),
                    Message::user(query.to_string()),
                ],
                self.generation_options.clone(),
            )
            .await?;

        Ok(response.content.trim().to_string())
    }

    async fn execute_nl_query(&self, query: &str) -> Result<Vec<Vec<Value>>, SearchError> {
        let (_node_schemas, edge_schemas) = self.get_graph_schema().await?;
        let mut previous_attempts = String::new();
        // Track whether any generated query actually executed against the graph
        // (Ok, even with zero rows) versus every one being rejected. See the
        // fail-loudly branch after the loop.
        let mut any_query_executed = false;
        let mut last_query_error: Option<String> = None;

        for _ in 0..self.max_attempts {
            let cypher_query = match self
                .generate_cypher_query(query, &edge_schemas, &previous_attempts)
                .await
            {
                Ok(cq) => cq,
                Err(error) => {
                    previous_attempts.push_str(&format!(
                        "Query: Not generated -> Executed with error: {error}\n"
                    ));
                    continue;
                }
            };

            if cypher_query.is_empty() {
                previous_attempts.push_str("Query: <empty> -> Result: None\n");
                continue;
            }

            // Drop every row that contains an internal node
            // (`natural_language_retriever.py:145-148`); a query left with no
            // rows counts as an empty result and is retried.
            let filtered = self.graph_db.query(&cypher_query, None).await.map(|rows| {
                rows.into_iter()
                    .filter(|row| !row.iter().any(contains_internal_node))
                    .collect::<Vec<_>>()
            });
            match filtered {
                Ok(context) if !context.is_empty() => return Ok(context),
                Ok(_) => {
                    any_query_executed = true;
                    previous_attempts.push_str(&format!("Query: {cypher_query} -> Result: None\n"));
                }
                Err(error) => {
                    last_query_error = Some(error.to_string());
                    previous_attempts.push_str(&format!(
                        "Query: {cypher_query} -> Executed with error: {error}\n"
                    ));
                }
            }
        }

        // Fail loudly when every generated query that reached the graph was
        // rejected by the backend and none ever executed. The dominant cause is
        // a schema-model mismatch: NATURAL_LANGUAGE emits Neo4j-style Cypher
        // against typed node labels (`:Entity`, `:TextDocument`, …; see
        // NL_SYSTEM_PROMPT_TEMPLATE), whereas the default local ladybug/kuzu
        // store keeps every node under a single generic `:Node` label (the real
        // kind lives in a `type` property). Returning an empty result here would
        // masquerade as "no results found" on a populated graph, which is what
        // the Python SDK does today — we surface a clear error instead.
        //
        // A pure LLM failure (no query ever reached the graph) still returns an
        // empty result, matching the historical contract.
        if !any_query_executed && let Some(err) = last_query_error {
            return Err(SearchError::InvalidInput(format!(
                "NATURAL_LANGUAGE search generated Cypher that this graph backend \
                 rejected on all {} attempt(s). This search type emits Neo4j-style \
                 Cypher against typed node labels (e.g. `:Entity`), which the local \
                 ladybug/kuzu backend does not use — it stores every node under a \
                 single generic `:Node` label. Use a Neo4j graph backend for \
                 NATURAL_LANGUAGE search, or a different search type such as \
                 GRAPH_COMPLETION with the local backend. Last backend error: {err}",
                self.max_attempts
            )));
        }

        Ok(vec![])
    }
}

#[async_trait]
impl SearchRetriever for NaturalLanguageRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::NaturalLanguage
    }

    fn supports_session_turn_preparation(&self) -> bool {
        false
    }

    #[cfg(test)]
    fn applied_tuning(&self) -> crate::retrievers::AppliedTuning {
        crate::retrievers::AppliedTuning {
            allow_cypher_query: Some(self.allow_cypher_query),
            ..Default::default()
        }
    }

    async fn get_context(
        &self,
        query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        ensure_cypher_queries_enabled(self.allow_cypher_query)?;

        if self.graph_db.is_empty().await? {
            return Ok(vec![]);
        }

        let rows = self.execute_nl_query(query).await?;
        Ok(rows_to_context(rows))
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let output_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query, params).await?,
        };

        Ok(SearchOutput::GraphQueryRows(context_to_rows(
            output_context,
        )))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::borrow::Cow;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_graph::{EdgeData, GraphDBError, GraphDBResult, GraphDBTrait, GraphNode, NodeData};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };

    use serde_json::json;

    use cognee_session::SessionContext;

    use super::{CypherSearchRetriever, NaturalLanguageRetriever};
    use crate::retrievers::SearchRetriever;
    use crate::types::{SearchError, SearchOutput, SearchParams};

    struct TestGraphDb {
        empty: bool,
        rows_by_query: std::collections::HashMap<String, Vec<Vec<Value>>>,
        /// When true, any query not present in `rows_by_query` returns a
        /// backend error (simulating a ladybug/kuzu binder rejecting a
        /// generated query that references a label the store doesn't have).
        reject_unseeded: bool,
    }

    use serde_json::Value;

    #[async_trait]
    impl GraphDBTrait for TestGraphDb {
        async fn initialize(&self) -> GraphDBResult<()> {
            Ok(())
        }

        async fn is_empty(&self) -> GraphDBResult<bool> {
            Ok(self.empty)
        }

        async fn query(
            &self,
            query: &str,
            _params: Option<std::collections::HashMap<Cow<'static, str>, Value>>,
        ) -> GraphDBResult<Vec<Vec<Value>>> {
            match self.rows_by_query.get(query) {
                Some(rows) => Ok(rows.clone()),
                None if self.reject_unseeded => Err(GraphDBError::QueryError(
                    "Binder exception: Table Entity does not exist.".to_string(),
                )),
                None => Ok(vec![]),
            }
        }

        async fn delete_graph(&self) -> GraphDBResult<()> {
            Ok(())
        }

        async fn has_node(&self, _node_id: &str) -> GraphDBResult<bool> {
            Ok(false)
        }

        async fn add_node_raw(&self, _node: serde_json::Value) -> GraphDBResult<()> {
            Ok(())
        }

        async fn add_nodes_raw(&self, _nodes: Vec<serde_json::Value>) -> GraphDBResult<()> {
            Ok(())
        }

        async fn delete_node(&self, _node_id: &str) -> GraphDBResult<()> {
            Ok(())
        }

        async fn delete_nodes(&self, _node_ids: &[String]) -> GraphDBResult<()> {
            Ok(())
        }

        async fn get_node(&self, _node_id: &str) -> GraphDBResult<Option<NodeData>> {
            Ok(None)
        }

        async fn get_nodes(&self, _node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
            Ok(vec![])
        }

        async fn has_edge(
            &self,
            _source_id: &str,
            _target_id: &str,
            _relationship_name: &str,
        ) -> GraphDBResult<bool> {
            Ok(false)
        }

        async fn has_edges(&self, _edges: &[EdgeData]) -> GraphDBResult<Vec<EdgeData>> {
            Ok(vec![])
        }

        async fn add_edge(
            &self,
            _source_id: &str,
            _target_id: &str,
            _relationship_name: &str,
            _properties: Option<std::collections::HashMap<Cow<'static, str>, Value>>,
        ) -> GraphDBResult<()> {
            Ok(())
        }

        async fn add_edges(&self, _edges: &[EdgeData]) -> GraphDBResult<()> {
            Ok(())
        }

        async fn get_edges(&self, _node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
            Ok(vec![])
        }

        async fn get_neighbors(&self, _node_id: &str) -> GraphDBResult<Vec<NodeData>> {
            Ok(vec![])
        }

        async fn get_connections(
            &self,
            _node_id: &str,
        ) -> GraphDBResult<
            Vec<(
                NodeData,
                std::collections::HashMap<Cow<'static, str>, Value>,
                NodeData,
            )>,
        > {
            Ok(vec![])
        }

        async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok((vec![], vec![]))
        }

        async fn get_graph_metrics(
            &self,
            _include_optional: bool,
        ) -> GraphDBResult<std::collections::HashMap<Cow<'static, str>, Value>> {
            Ok(std::collections::HashMap::new())
        }

        async fn get_filtered_graph_data(
            &self,
            _attribute_filters: &std::collections::HashMap<Cow<'static, str>, Vec<Value>>,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok((vec![], vec![]))
        }

        async fn get_nodeset_subgraph(
            &self,
            _node_type: &str,
            _node_names: &[String],
            _node_name_filter_operator: &str,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok((vec![], vec![]))
        }
    }

    struct TestLlm {
        responses: Mutex<VecDeque<String>>,
        /// The system prompt of every call, in order.
        system_prompts: Mutex<Vec<String>>,
    }

    impl TestLlm {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(ToString::to_string)
                        .collect::<VecDeque<_>>(),
                ),
                system_prompts: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl Llm for TestLlm {
        async fn generate(
            &self,
            messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            self.system_prompts
                .lock()
                .unwrap()
                .push(messages[0].content.clone());
            Ok(GenerationResponse {
                content: self
                    .responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_default(),
                model: "test".to_string(),
                usage: Some(TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                }),
                finish_reason: Some("stop".to_string()),
            })
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<serde_json::Value> {
            Err(LlmError::ConfigError("not implemented".to_string()))
        }

        fn model(&self) -> &str {
            "test"
        }
    }

    #[tokio::test]
    async fn cypher_retriever_returns_query_rows() {
        let graph_db = Arc::new(TestGraphDb {
            empty: false,
            reject_unseeded: false,
            rows_by_query: std::collections::HashMap::from([(
                "MATCH (n) RETURN n".to_string(),
                vec![vec![json!({"name": "Alice"})]],
            )]),
        });

        let retriever = CypherSearchRetriever::new(graph_db);
        let output = retriever
            .get_completion(
                "MATCH (n) RETURN n",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::GraphQueryRows(rows) => assert_eq!(rows.len(), 1),
            _ => panic!("expected graph query rows"),
        }
    }

    /// `search.allow_cypher_query = false` refuses the query instead of
    /// running it.
    #[tokio::test]
    async fn a_disallowed_cypher_query_is_refused() {
        let graph_db = Arc::new(TestGraphDb {
            empty: false,
            reject_unseeded: false,
            rows_by_query: std::collections::HashMap::from([(
                "MATCH (n) RETURN n".to_string(),
                vec![vec![json!({"name": "Alice"})]],
            )]),
        });

        let retriever = CypherSearchRetriever::new(graph_db).with_allow_cypher_query(false);
        let err = retriever
            .get_completion(
                "MATCH (n) RETURN n",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .expect_err("disallowed");
        assert!(matches!(err, SearchError::InvalidInput(_)), "{err:?}");
    }

    #[tokio::test]
    async fn natural_language_retriever_retries_until_results() {
        let graph_db = Arc::new(TestGraphDb {
            empty: false,
            reject_unseeded: false,
            rows_by_query: std::collections::HashMap::from([
                (
                    "\n            MATCH (n)\n            UNWIND keys(n) AS prop\n            RETURN DISTINCT labels(n) AS NodeLabels, collect(DISTINCT prop) AS Properties;\n            "
                        .to_string(),
                    vec![vec![json!(["Entity"]), json!(["name"])]],
                ),
                (
                    "\n            MATCH ()-[r]->()\n            UNWIND keys(r) AS key\n            RETURN DISTINCT key;\n            "
                        .to_string(),
                    vec![vec![json!("relationship")]],
                ),
                ("MATCH (n) WHERE n.name = 'Missing' RETURN n".to_string(), vec![]),
                (
                    "MATCH (n) WHERE n.name = 'Alice' RETURN n".to_string(),
                    vec![vec![json!({"name": "Alice"})]],
                ),
            ]),
        });

        let llm = Arc::new(TestLlm::new(vec![
            "MATCH (n) WHERE n.name = 'Missing' RETURN n",
            "MATCH (n) WHERE n.name = 'Alice' RETURN n",
        ]));

        let retriever = NaturalLanguageRetriever::new(graph_db, llm, Some(3), None);
        let output = retriever
            .get_completion(
                "Find Alice",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::GraphQueryRows(rows) => assert_eq!(rows.len(), 1),
            _ => panic!("expected graph query rows"),
        }
    }

    struct FailThenSucceedLlm {
        fail_count: Mutex<usize>,
        success_response: String,
    }

    impl FailThenSucceedLlm {
        fn new(fail_count: usize, success_response: &str) -> Self {
            Self {
                fail_count: Mutex::new(fail_count),
                success_response: success_response.to_string(),
            }
        }
    }

    #[async_trait]
    impl Llm for FailThenSucceedLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            let mut remaining = self.fail_count.lock().unwrap(); // lock poison is unrecoverable
            if *remaining > 0 {
                *remaining -= 1;
                return Err(LlmError::ApiError("simulated LLM failure".to_string()));
            }
            Ok(GenerationResponse {
                content: self.success_response.clone(),
                model: "test".to_string(),
                usage: Some(TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                }),
                finish_reason: Some("stop".to_string()),
            })
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<serde_json::Value> {
            Err(LlmError::ConfigError("not implemented".to_string()))
        }

        fn model(&self) -> &str {
            "test"
        }
    }

    #[tokio::test]
    async fn natural_language_retriever_retries_on_llm_error() {
        let graph_db = Arc::new(TestGraphDb {
            empty: false,
            reject_unseeded: false,
            rows_by_query: std::collections::HashMap::from([
                (
                    "\n            MATCH (n)\n            UNWIND keys(n) AS prop\n            RETURN DISTINCT labels(n) AS NodeLabels, collect(DISTINCT prop) AS Properties;\n            "
                        .to_string(),
                    vec![vec![json!(["Entity"]), json!(["name"])]],
                ),
                (
                    "\n            MATCH ()-[r]->()\n            UNWIND keys(r) AS key\n            RETURN DISTINCT key;\n            "
                        .to_string(),
                    vec![vec![json!("relationship")]],
                ),
                (
                    "MATCH (n) WHERE n.name = 'Alice' RETURN n".to_string(),
                    vec![vec![json!({"name": "Alice"})]],
                ),
            ]),
        });

        // LLM fails on first call, succeeds on second with a valid query
        let llm = Arc::new(FailThenSucceedLlm::new(
            1,
            "MATCH (n) WHERE n.name = 'Alice' RETURN n",
        ));

        let retriever = NaturalLanguageRetriever::new(graph_db, llm, Some(3), None);
        let output = retriever
            .get_completion(
                "Find Alice",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::GraphQueryRows(rows) => {
                assert_eq!(
                    rows.len(),
                    1,
                    "should return results after recovering from LLM error"
                );
            }
            _ => panic!("expected graph query rows"),
        }
    }

    #[tokio::test]
    async fn natural_language_retriever_returns_empty_when_all_llm_attempts_fail() {
        let graph_db = Arc::new(TestGraphDb {
            empty: false,
            reject_unseeded: false,
            rows_by_query: std::collections::HashMap::from([
                (
                    "\n            MATCH (n)\n            UNWIND keys(n) AS prop\n            RETURN DISTINCT labels(n) AS NodeLabels, collect(DISTINCT prop) AS Properties;\n            "
                        .to_string(),
                    vec![vec![json!(["Entity"]), json!(["name"])]],
                ),
                (
                    "\n            MATCH ()-[r]->()\n            UNWIND keys(r) AS key\n            RETURN DISTINCT key;\n            "
                        .to_string(),
                    vec![vec![json!("relationship")]],
                ),
            ]),
        });

        // LLM fails on all 3 attempts
        let llm = Arc::new(FailThenSucceedLlm::new(3, "should not reach this"));

        let retriever = NaturalLanguageRetriever::new(graph_db, llm, Some(3), None);
        let output = retriever
            .get_completion(
                "Find Alice",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::GraphQueryRows(rows) => {
                assert!(
                    rows.is_empty(),
                    "should return empty when all LLM attempts fail"
                );
            }
            _ => panic!("expected graph query rows"),
        }
    }

    #[tokio::test]
    async fn natural_language_retriever_errors_when_backend_rejects_all_queries() {
        // Simulates the default local ladybug/kuzu backend: schema
        // introspection succeeds (reporting the generic `:Node` label), but the
        // Neo4j-style Cypher the retriever generates references a typed label
        // (`:Entity`) the store has no table for, so every attempt is rejected.
        // The retriever must surface a clear error, not a misleading empty
        // result on a populated graph.
        let graph_db = Arc::new(TestGraphDb {
            empty: false,
            reject_unseeded: true,
            rows_by_query: std::collections::HashMap::from([
                (
                    "\n            MATCH (n)\n            UNWIND keys(n) AS prop\n            RETURN DISTINCT labels(n) AS NodeLabels, collect(DISTINCT prop) AS Properties;\n            "
                        .to_string(),
                    // kuzu-style: labels(n) is the scalar table name "Node".
                    vec![vec![json!("Node"), json!(["id", "name", "type"])]],
                ),
                (
                    "\n            MATCH ()-[r]->()\n            UNWIND keys(r) AS key\n            RETURN DISTINCT key;\n            "
                        .to_string(),
                    vec![vec![json!("relationship_name")]],
                ),
            ]),
        });

        // The LLM emits Neo4j-style typed-label Cypher on every attempt.
        let llm = Arc::new(TestLlm::new(vec![
            "MATCH (n:Entity {name: 'John'}) RETURN n",
            "MATCH (n:Entity {name: 'John'}) RETURN n",
            "MATCH (n:Entity {name: 'John'}) RETURN n",
        ]));

        let retriever = NaturalLanguageRetriever::new(graph_db, llm, Some(3), None);
        let err = retriever
            .get_context("Where does John work?", &SearchParams::default())
            .await
            .expect_err("must fail loudly when the backend rejects every generated query");

        match err {
            SearchError::InvalidInput(msg) => {
                assert!(
                    msg.contains("NATURAL_LANGUAGE") && msg.contains(":Node"),
                    "error should explain the label-model mismatch, got: {msg}"
                );
            }
            other => panic!("expected SearchError::InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn nl_system_prompt_contains_required_sections() {
        use super::NL_SYSTEM_PROMPT_TEMPLATE;
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("{edge_schemas}"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("{previous_attempts}"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("QUERY REQUIREMENTS:"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("PERFORMANCE OPTIMIZATION:"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("ERROR PREVENTION:"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("Node schemas:"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("EntityType"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("Entity"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("TextDocument"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("DocumentChunk"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("TextSummary"));
        assert!(NL_SYSTEM_PROMPT_TEMPLATE.contains("Example 1:"));
    }

    const NODE_SCHEMA_QUERY: &str = "\n            MATCH (n)\n            UNWIND keys(n) AS prop\n            RETURN DISTINCT labels(n) AS NodeLabels, collect(DISTINCT prop) AS Properties;\n            ";
    const EDGE_SCHEMA_QUERY: &str = "\n            MATCH ()-[r]->()\n            UNWIND keys(r) AS key\n            RETURN DISTINCT key;\n            ";

    #[test]
    fn internal_schema_rows_are_recognized() {
        use super::is_internal_schema_row;

        assert!(is_internal_schema_row(&[
            json!(["UserPreference"]),
            json!(["id", "is_internal"])
        ]));
        assert!(!is_internal_schema_row(&[
            json!(["Entity"]),
            json!(["id", "name"])
        ]));
    }

    #[test]
    fn internal_nodes_are_found_at_any_depth_and_in_serialized_columns() {
        use super::contains_internal_node;

        assert!(contains_internal_node(&json!({ "is_internal": true })));
        assert!(contains_internal_node(
            &json!([{ "n": { "is_internal": true } }])
        ));
        // Ladybug/Kuzu: node properties come back as a JSON string column.
        assert!(contains_internal_node(&json!(
            r#"{"id": "p1", "is_internal": true}"#
        )));
        assert!(!contains_internal_node(&json!({ "is_internal": false })));
        assert!(!contains_internal_node(&json!({ "name": "Alice" })));
        // A string that merely mentions the key is not a node.
        assert!(!contains_internal_node(&json!("about is_internal")));
    }

    /// Rows holding an internal node are dropped from NATURAL_LANGUAGE results
    /// (`natural_language_retriever.py:145-148`); a query left with no rows is
    /// retried like any empty result.
    #[tokio::test]
    async fn natural_language_retriever_hides_internal_nodes() {
        let graph_db = Arc::new(TestGraphDb {
            empty: false,
            reject_unseeded: false,
            rows_by_query: std::collections::HashMap::from([
                (
                    NODE_SCHEMA_QUERY.to_string(),
                    vec![
                        vec![json!(["Entity"]), json!(["name"])],
                        vec![json!(["UserPreference"]), json!(["is_internal"])],
                    ],
                ),
                (
                    EDGE_SCHEMA_QUERY.to_string(),
                    vec![vec![json!("relationship")]],
                ),
                (
                    "MATCH (p:UserPreference) RETURN p".to_string(),
                    vec![vec![json!({ "id": "pref", "is_internal": true })]],
                ),
                (
                    "MATCH (n) RETURN n".to_string(),
                    vec![
                        vec![json!({ "name": "Alice" })],
                        vec![json!({ "id": "pref", "is_internal": true })],
                    ],
                ),
            ]),
        });

        let llm = Arc::new(TestLlm::new(vec![
            "MATCH (p:UserPreference) RETURN p",
            "MATCH (n) RETURN n",
        ]));

        let retriever = NaturalLanguageRetriever::new(
            graph_db,
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(3),
            None,
        );
        let output = retriever
            .get_completion(
                "What is in the graph?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        let SearchOutput::GraphQueryRows(rows) = output else {
            panic!("expected graph query rows");
        };
        assert_eq!(rows, vec![vec![json!({ "name": "Alice" })]]);

        // The retry prompt lists the failed attempt on its own line, after
        // Python's "No attempts yet" placeholder has been replaced.
        let prompts = llm.system_prompts.lock().unwrap().clone();
        assert!(prompts[0].contains("No attempts yet"));
        assert!(
            prompts[1].contains("Query: MATCH (p:UserPreference) RETURN p -> Result: None\n"),
            "got: {}",
            prompts[1]
        );
        assert!(!prompts[1].contains("No attempts yet"));
        assert!(!prompts[1].contains("\\n"), "no literal backslash-n");
    }
}
