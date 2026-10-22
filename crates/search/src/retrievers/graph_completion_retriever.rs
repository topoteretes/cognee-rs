use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_vector::VectorDB;
use tracing::debug;

use cognee_session::SessionContext;

use crate::retrievers::SearchRetriever;
use crate::retrievers::graph_core::{GraphPrompts, GraphRetrieverCore, complete};
use crate::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};
use crate::utils::{render_edges_context, skip_completion_on_empty_context};

pub struct GraphCompletionRetriever {
    core: GraphRetrieverCore,
    llm: Arc<dyn Llm>,
    prompts: GraphPrompts,
    generation_options: Option<GenerationOptions>,
}

impl GraphCompletionRetriever {
    /// The system + user messages a completion over `context` sends, or
    /// `None` when the rendered context is empty. Shared by `get_completion`
    /// and the `only_context` preview.
    fn build_completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        let graph_context_text = render_edges_context(context);
        if graph_context_text.is_empty() {
            return Ok(None);
        }
        debug!(
            context_items = context.len(),
            "Graph context assembled:\n{graph_context_text}"
        );
        self.prompts
            .messages(query, &graph_context_text, session, params)
            .map(Some)
    }

    /// Cap the neighbours expanded per seed node in triplet search
    /// (`search.triplet_neighbor_cap`); `None` is uncapped.
    #[must_use]
    pub fn with_neighbor_cap(mut self, cap: Option<usize>) -> Self {
        self.core.neighbor_cap = cap;
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        top_k: Option<usize>,
        wide_search_top_k: Option<usize>,
        triplet_distance_penalty: Option<f32>,
        system_prompt: Option<String>,
        system_prompt_path: Option<String>,
        user_prompt_template: Option<String>,
        generation_options: Option<GenerationOptions>,
    ) -> Self {
        Self {
            core: GraphRetrieverCore::new(
                vector_db,
                embedding_engine,
                graph_db,
                top_k,
                wide_search_top_k,
                triplet_distance_penalty,
            ),
            llm,
            prompts: GraphPrompts {
                system_prompt,
                system_prompt_path,
                user_prompt_template,
            },
            generation_options,
        }
    }
}

#[async_trait]
impl SearchRetriever for GraphCompletionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletion
    }

    fn dataset_scoping(&self) -> super::DatasetScoping {
        super::DatasetScoping::Membership
    }

    #[cfg(test)]
    fn applied_tuning(&self) -> crate::retrievers::AppliedTuning {
        crate::retrievers::AppliedTuning {
            neighbor_cap: Some(self.core.neighbor_cap),
            ..Default::default()
        }
    }

    #[tracing::instrument(
        name = "cognee.retrieval.get_context",
        skip(self, params),
        fields(cognee.retrieval.retriever = "GraphCompletionRetriever")
    )]
    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        // A populated graph with no matching triplets is a normal miss and
        // returns an empty context; an empty graph is `NotFound`.
        self.core.ensure_graph_not_empty().await?;
        self.core.search(query, params).await
    }

    async fn completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        self.build_completion_messages(query, context, session, params)
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let completion_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query, params).await?,
        };

        let Some(messages) =
            self.build_completion_messages(query, &completion_context, session, params)?
        else {
            return Ok(skip_completion_on_empty_context());
        };

        complete(
            self.llm.as_ref(),
            messages,
            params,
            self.generation_options.clone(),
        )
        .await
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
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_graph::{EdgeData, GraphDBResult, GraphDBTrait, GraphNode, NodeData};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };

    use crate::test_support::neighborhood_of;
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};

    use serde_json::json;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use crate::retrievers::{GraphCompletionRetriever, SearchRetriever};
    use crate::types::{SearchError, SearchOutput, SearchParams};

    struct TestEmbeddingEngine;

    #[async_trait]
    impl EmbeddingEngine for TestEmbeddingEngine {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![0.8, 0.2]])
        }

        fn dimension(&self) -> usize {
            2
        }

        fn batch_size(&self) -> usize {
            16
        }

        fn max_sequence_length(&self) -> usize {
            512
        }
    }

    struct TestVectorDb {
        collections: HashMap<String, Vec<SearchResult>>,
    }

    impl TestVectorDb {
        fn key(data_type: &str, field_name: &str) -> String {
            format!("{data_type}_{field_name}")
        }
    }

    #[async_trait]
    impl VectorDB for TestVectorDb {
        async fn retrieve(
            &self,
            _data_type: &str,
            _field_name: &str,
            _ids: &[Uuid],
        ) -> VectorDBResult<Vec<SearchResult>> {
            Ok(vec![])
        }

        async fn create_collection(
            &self,
            _data_type: &str,
            _field_name: &str,
            _dimension: usize,
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
            Ok(self
                .collections
                .contains_key(&Self::key(data_type, field_name)))
        }

        async fn index_points(
            &self,
            _data_type: &str,
            _field_name: &str,
            _points: &[VectorPoint],
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn search_similar(
            &self,
            data_type: &str,
            field_name: &str,
            _query_vector: &[f32],
            top_k: usize,
        ) -> VectorDBResult<Vec<SearchResult>> {
            let key = Self::key(data_type, field_name);
            Ok(self
                .collections
                .get(&key)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(top_k)
                .collect())
        }

        async fn delete_collection(
            &self,
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn delete_points(
            &self,
            _data_type: &str,
            _field_name: &str,
            _point_ids: &[Uuid],
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn collection_size(
            &self,
            data_type: &str,
            field_name: &str,
        ) -> VectorDBResult<usize> {
            Ok(self
                .collections
                .get(&Self::key(data_type, field_name))
                .map(|items| items.len())
                .unwrap_or_default())
        }
    }

    #[derive(Default)]
    struct TestLlm {
        response_text: String,
        last_messages: Mutex<Vec<Message>>,
    }

    #[async_trait]
    impl Llm for TestLlm {
        async fn generate(
            &self,
            messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            self.last_messages.lock().unwrap().clone_from(&messages);
            Ok(GenerationResponse {
                content: self.response_text.clone(),
                model: "test-model".to_string(),
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
            Err(LlmError::ConfigError(
                "not implemented for this unit test".to_string(),
            ))
        }

        fn model(&self) -> &str {
            "test-model"
        }
    }

    struct TestGraphDb {
        empty: bool,
        nodes: Vec<GraphNode>,
        edges: Vec<EdgeData>,
    }

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
            _query: &str,
            _params: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
        ) -> GraphDBResult<Vec<Vec<serde_json::Value>>> {
            Ok(vec![])
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
            _properties: Option<HashMap<Cow<'static, str>, serde_json::Value>>,
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
                HashMap<Cow<'static, str>, serde_json::Value>,
                NodeData,
            )>,
        > {
            Ok(vec![])
        }

        async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok((self.nodes.clone(), self.edges.clone()))
        }

        /// Mirrors the real adapters' scoped load, which graph search uses instead
        /// of `get_graph_data`. The trait's default implementation walks
        /// `get_connections`, which this stub returns empty, so without this
        /// override every graph search here would see an empty graph.
        async fn get_neighborhood(
            &self,
            node_ids: &[String],
            depth: usize,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok(neighborhood_of(&self.nodes, &self.edges, node_ids, depth))
        }

        async fn get_graph_metrics(
            &self,
            _include_optional: bool,
        ) -> GraphDBResult<HashMap<Cow<'static, str>, serde_json::Value>> {
            Ok(HashMap::new())
        }

        async fn get_filtered_graph_data(
            &self,
            _attribute_filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
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

    fn node(id: &str, name: &str) -> GraphNode {
        let mut props = HashMap::new();
        props.insert(Cow::Borrowed("name"), json!(name));
        (id.to_string(), props)
    }

    fn entity_hit(id: &str, score: f32) -> SearchResult {
        SearchResult {
            id: Uuid::parse_str(id).unwrap(),
            score,
            metadata: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn ranks_edges_by_candidate_node_scores() {
        let mut collections = HashMap::new();
        collections.insert(
            TestVectorDb::key("Entity", "name"),
            vec![
                entity_hit("00000000-0000-0000-0000-000000000001", 0.95),
                entity_hit("00000000-0000-0000-0000-000000000002", 0.80),
                entity_hit("00000000-0000-0000-0000-000000000003", 0.40),
            ],
        );

        let graph_db = Arc::new(TestGraphDb {
            empty: false,
            nodes: vec![
                node("00000000-0000-0000-0000-000000000001", "Alice"),
                node("00000000-0000-0000-0000-000000000002", "Bob"),
                node("00000000-0000-0000-0000-000000000003", "Charlie"),
            ],
            edges: vec![
                (
                    "00000000-0000-0000-0000-000000000001".to_string(),
                    "00000000-0000-0000-0000-000000000002".to_string(),
                    "KNOWS".to_string(),
                    HashMap::new(),
                ),
                (
                    "00000000-0000-0000-0000-000000000002".to_string(),
                    "00000000-0000-0000-0000-000000000003".to_string(),
                    "WORKS_WITH".to_string(),
                    HashMap::new(),
                ),
            ],
        });

        let retriever = GraphCompletionRetriever::new(
            Arc::new(TestVectorDb { collections }),
            Arc::new(TestEmbeddingEngine),
            graph_db,
            Arc::new(TestLlm {
                response_text: "unused".to_string(),
                ..Default::default()
            }),
            Some(2),
            Some(5),
            // Use the default penalty (6.5) — unmatched edge types get this distance.
            // Every distance is scaled by 2 - importance_weight = 1.5 (the
            // default weight, as in Python):
            // 1.5 * (Alice 0.05 + Bob 0.20 + KNOWS unmatched 6.5) = 10.125
            // 1.5 * (Bob 0.20 + Charlie 0.60 + WORKS_WITH unmatched 6.5) = 10.95
            // Sort ascending: KNOWS first, WORKS_WITH second.
            None,
            None,
            None,
            None,
            None,
        );

        let context = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 2);
        assert_eq!(context[0].payload["relationship"], "KNOWS");
        assert_eq!(context[0].payload["source_name"], "Alice");
        assert_eq!(context[0].payload["target_name"], "Bob");
        assert_eq!(context[1].payload["relationship"], "WORKS_WITH");
        // Verify distance-based scores (lower = better), each scaled by 1.5:
        // KNOWS: 1.5 * (0.05 + 0.20 + 6.5) = 10.125;
        // WORKS_WITH: 1.5 * (0.20 + 0.60 + 6.5) = 10.95
        let score_knows = context[0].score.unwrap();
        let score_works_with = context[1].score.unwrap();
        assert!(
            score_knows < score_works_with,
            "KNOWS distance ({score_knows}) should be less than WORKS_WITH distance ({score_works_with})"
        );
        assert!(
            (score_knows - 10.125).abs() < 1e-4,
            "KNOWS expected score 10.125, got {score_knows}"
        );
        assert!(
            (score_works_with - 10.95).abs() < 1e-4,
            "WORKS_WITH expected score 10.95, got {score_works_with}"
        );
    }

    #[tokio::test]
    async fn renders_graph_context_for_completion() {
        let llm = Arc::new(TestLlm {
            response_text: "graph answer".to_string(),
            ..Default::default()
        });

        let retriever = GraphCompletionRetriever::new(
            Arc::new(TestVectorDb {
                collections: HashMap::new(),
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::new(TestGraphDb {
                empty: true,
                nodes: vec![],
                edges: vec![],
            }),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(2),
            Some(5),
            Some(0.0),
            Some("graph system".to_string()),
            None,
            Some("Question={question}\nGraph={context}".to_string()),
            None,
        );

        let context = vec![crate::types::SearchItem {
            id: None,
            score: Some(1.0),
            payload: json!({
                "source_name": "Alice",
                "target_name": "Bob",
                "relationship": "KNOWS"
            }),
        }];

        let output = retriever
            .get_completion(
                "who does Alice know?",
                Some(context),
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Text(answer) => assert_eq!(answer, "graph answer"),
            _ => panic!("expected text output"),
        }

        let messages = llm.last_messages.lock().unwrap().clone();
        assert_eq!(messages[0].content, "graph system");
        assert!(messages[1].content.contains("Graph="));
        assert!(messages[1].content.contains("Nodes:"));
        assert!(messages[1].content.contains("--[KNOWS]-->"));
    }

    #[tokio::test]
    async fn uses_graph_prompt_template_by_default() {
        let llm = Arc::new(TestLlm {
            response_text: "answer".to_string(),
            ..Default::default()
        });

        let retriever = GraphCompletionRetriever::new(
            Arc::new(TestVectorDb {
                collections: HashMap::new(),
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::new(TestGraphDb {
                empty: true,
                nodes: vec![],
                edges: vec![],
            }),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(2),
            Some(5),
            Some(0.0),
            None,
            None,
            None, // user_prompt_template — should use graph default
            None,
        );

        let context = vec![crate::types::SearchItem {
            id: None,
            score: Some(1.0),
            payload: json!({
                "source_name": "Alice",
                "target_name": "Bob",
                "relationship": "KNOWS"
            }),
        }];

        let _ = retriever
            .get_completion(
                "Who knows Bob?",
                Some(context),
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        let messages = llm.last_messages.lock().unwrap().clone();
        // User message should use graph_context_for_question format
        assert!(
            messages[1]
                .content
                .contains("The question is: `Who knows Bob?`"),
            "expected graph prompt format, got: {}",
            messages[1].content
        );
        assert!(messages[1].content.contains("knowledge graph"));
        // Should NOT use the generic RAG format
        assert!(!messages[1].content.starts_with("Question:\n"));
    }

    fn retriever_over(
        graph_db: TestGraphDb,
        collections: HashMap<String, Vec<SearchResult>>,
        llm: &Arc<TestLlm>,
    ) -> GraphCompletionRetriever {
        GraphCompletionRetriever::new(
            Arc::new(TestVectorDb { collections }),
            Arc::new(TestEmbeddingEngine),
            Arc::new(graph_db),
            Arc::clone(llm) as Arc<dyn Llm>,
            Some(2),
            Some(5),
            None,
            None,
            None,
            None,
            None,
        )
    }

    /// Python raises `NoDataError` on an empty graph (SDK-270); the Rust
    /// counterpart is `NotFound`, the same error the hybrid retriever raises.
    #[tokio::test]
    async fn empty_graph_is_not_found() {
        let llm = Arc::new(TestLlm::default());
        let retriever = retriever_over(
            TestGraphDb {
                empty: true,
                nodes: vec![],
                edges: vec![],
            },
            HashMap::new(),
            &llm,
        );

        let err = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, SearchError::NotFound(ref message) if message.contains("knowledge graph is empty")),
            "expected NotFound, got {err:?}"
        );

        // Through get_completion the error surfaces before any LLM call.
        let err = retriever
            .get_completion(
                "query",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::NotFound(_)));
        assert!(llm.last_messages.lock().unwrap().is_empty(), "no LLM call");
    }

    /// A populated graph with no matching triplet is a normal miss: no error,
    /// and no LLM call — Python's `skip_completion_on_empty_context`.
    #[tokio::test]
    async fn empty_context_skips_the_llm() {
        let llm = Arc::new(TestLlm {
            response_text: "phantom answer".to_string(),
            ..Default::default()
        });
        let retriever = retriever_over(
            TestGraphDb {
                empty: false,
                nodes: vec![],
                edges: vec![],
            },
            HashMap::new(),
            &llm,
        );

        let context = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap();
        assert!(context.is_empty());

        let output = retriever
            .get_completion(
                "query",
                Some(context),
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();
        assert!(matches!(output, SearchOutput::Texts(ref texts) if texts.is_empty()));
        assert!(llm.last_messages.lock().unwrap().is_empty(), "no LLM call");
    }

    /// The edge's `edge_text` property rides into the context payload and is
    /// rendered after the relationship in the prompt (`resolve_edges_to_text.py:
    /// 79-100`).
    #[tokio::test]
    async fn edge_text_reaches_the_prompt() {
        let alice = "00000000-0000-0000-0000-000000000001";
        let acme = "00000000-0000-0000-0000-000000000002";
        let llm = Arc::new(TestLlm {
            response_text: "answer".to_string(),
            ..Default::default()
        });
        let retriever = retriever_over(
            TestGraphDb {
                empty: false,
                nodes: vec![node(alice, "Alice"), node(acme, "Acme")],
                edges: vec![(
                    alice.to_string(),
                    acme.to_string(),
                    "works_at".to_string(),
                    HashMap::from([(
                        Cow::Borrowed("edge_text"),
                        json!("Alice has worked at Acme since 2019"),
                    )]),
                )],
            },
            HashMap::from([(
                TestVectorDb::key("Entity", "name"),
                vec![entity_hit(alice, 0.9)],
            )]),
            &llm,
        );

        let context = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap();
        assert_eq!(
            context[0].payload["edge_text"],
            "Alice has worked at Acme since 2019"
        );

        retriever
            .get_completion(
                "query",
                Some(context),
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();
        let messages = llm.last_messages.lock().unwrap().clone();
        assert!(
            messages[1]
                .content
                .contains("Alice --[works_at]--> Acme  (Alice has worked at Acme since 2019)"),
            "got: {}",
            messages[1].content
        );
    }

    /// Internal nodes (Python's `InternalDataPoint`) are never projected, so
    /// no edge touching one reaches the context (`CogneeGraph.py:192-232`).
    #[tokio::test]
    async fn internal_nodes_never_reach_the_context() {
        let alice = "00000000-0000-0000-0000-000000000001";
        let acme = "00000000-0000-0000-0000-000000000002";
        let preference = "00000000-0000-0000-0000-000000000003";
        let mut internal = node(preference, "Alice prefers terse answers");
        internal.1.insert(Cow::Borrowed("is_internal"), json!(true));

        let llm = Arc::new(TestLlm::default());
        let retriever = retriever_over(
            TestGraphDb {
                empty: false,
                nodes: vec![node(alice, "Alice"), node(acme, "Acme"), internal],
                edges: vec![
                    (
                        alice.to_string(),
                        acme.to_string(),
                        "works_at".to_string(),
                        HashMap::new(),
                    ),
                    (
                        alice.to_string(),
                        preference.to_string(),
                        "prefers".to_string(),
                        HashMap::new(),
                    ),
                ],
            },
            HashMap::from([(
                TestVectorDb::key("Entity", "name"),
                vec![entity_hit(alice, 0.9), entity_hit(preference, 0.95)],
            )]),
            &llm,
        );

        let context = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 1, "only the non-internal edge: {context:?}");
        assert_eq!(context[0].payload["relationship"], "works_at");
    }
}
