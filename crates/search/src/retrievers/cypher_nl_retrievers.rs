use std::env;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_session::SessionContext;
use serde_json::{Value, json};

use crate::retrievers::SearchRetriever;
use crate::types::{SearchContext, SearchError, SearchItem, SearchOutput, SearchType};

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

fn cypher_queries_enabled() -> bool {
    env::var("ALLOW_CYPHER_QUERY")
        .unwrap_or_else(|_| "true".to_string())
        .to_lowercase()
        != "false"
}

fn ensure_cypher_queries_enabled() -> Result<(), SearchError> {
    if cypher_queries_enabled() {
        Ok(())
    } else {
        Err(SearchError::InvalidInput(
            "Cypher query search types are disabled via ALLOW_CYPHER_QUERY=false".to_string(),
        ))
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
}

impl CypherSearchRetriever {
    pub fn new(graph_db: Arc<dyn GraphDBTrait>) -> Self {
        Self { graph_db }
    }
}

#[async_trait]
impl SearchRetriever for CypherSearchRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::Cypher
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        ensure_cypher_queries_enabled()?;

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
    ) -> Result<SearchOutput, SearchError> {
        let output_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
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
        }
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
            .replace("{previous_attempts}", previous_attempts);

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
        let mut previous_attempts = String::from("No attempts yet.");

        for _ in 0..self.max_attempts {
            let cypher_query = self
                .generate_cypher_query(query, &edge_schemas, &previous_attempts)
                .await?;

            if cypher_query.is_empty() {
                previous_attempts.push_str("Query: <empty> -> Result: None\\n");
                continue;
            }

            match self.graph_db.query(&cypher_query, None).await {
                Ok(context) if !context.is_empty() => return Ok(context),
                Ok(_) => {
                    previous_attempts
                        .push_str(&format!("Query: {cypher_query} -> Result: None\\n"));
                }
                Err(error) => {
                    previous_attempts.push_str(&format!(
                        "Query: {cypher_query} -> Executed with error: {error}\\n"
                    ));
                }
            }
        }

        Ok(vec![])
    }
}

#[async_trait]
impl SearchRetriever for NaturalLanguageRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::NaturalLanguage
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        ensure_cypher_queries_enabled()?;

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
    ) -> Result<SearchOutput, SearchError> {
        let output_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        Ok(SearchOutput::GraphQueryRows(context_to_rows(
            output_context,
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_graph::{EdgeData, GraphDBResult, GraphDBTrait, GraphNode, NodeData};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };

    use serde_json::json;

    use cognee_session::SessionContext;

    use super::{CypherSearchRetriever, NaturalLanguageRetriever};
    use crate::retrievers::SearchRetriever;
    use crate::types::SearchOutput;

    struct TestGraphDb {
        empty: bool,
        rows_by_query: std::collections::HashMap<String, Vec<Vec<Value>>>,
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
            Ok(self.rows_by_query.get(query).cloned().unwrap_or_default())
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
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok((vec![], vec![]))
        }
    }

    struct TestLlm {
        responses: Mutex<VecDeque<String>>,
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
            }
        }
    }

    #[async_trait]
    impl Llm for TestLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
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
            rows_by_query: std::collections::HashMap::from([(
                "MATCH (n) RETURN n".to_string(),
                vec![vec![json!({"name": "Alice"})]],
            )]),
        });

        let retriever = CypherSearchRetriever::new(graph_db);
        let output = retriever
            .get_completion("MATCH (n) RETURN n", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::GraphQueryRows(rows) => assert_eq!(rows.len(), 1),
            _ => panic!("expected graph query rows"),
        }
    }

    #[tokio::test]
    async fn natural_language_retriever_retries_until_results() {
        let graph_db = Arc::new(TestGraphDb {
            empty: false,
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
            .get_completion("Find Alice", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::GraphQueryRows(rows) => assert_eq!(rows.len(), 1),
            _ => panic!("expected graph query rows"),
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
}
