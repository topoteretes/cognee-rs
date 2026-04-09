use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm};
use cognee_vector::VectorDB;
use serde_json::json;
use tracing::debug;

use cognee_session::SessionContext;

use crate::graph_retrieval::{
    DEFAULT_TRIPLET_DISTANCE_PENALTY, GraphRetrievalConfig, brute_force_triplet_search,
};
use crate::retrievers::SearchRetriever;
use crate::types::{SearchContext, SearchError, SearchItem, SearchOutput, SearchType};
use crate::utils::{
    build_messages_with_history, render_edges_context, render_graph_user_prompt,
    resolve_system_prompt,
};

const DEFAULT_TOP_K: usize = 5;
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;

pub struct GraphCompletionRetriever {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    graph_db: Arc<dyn GraphDBTrait>,
    llm: Arc<dyn Llm>,
    top_k: usize,
    wide_search_top_k: usize,
    triplet_distance_penalty: f32,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl GraphCompletionRetriever {
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
            vector_db,
            embedding_engine,
            graph_db,
            llm,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            wide_search_top_k: wide_search_top_k.unwrap_or(DEFAULT_WIDE_SEARCH_TOP_K),
            triplet_distance_penalty: triplet_distance_penalty
                .unwrap_or(DEFAULT_TRIPLET_DISTANCE_PENALTY),
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }
}

#[async_trait]
impl SearchRetriever for GraphCompletionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletion
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        if self.graph_db.is_empty().await? {
            debug!("graph is empty — returning empty context");
            return Ok(vec![]);
        }

        let config = GraphRetrievalConfig {
            top_k: self.top_k,
            wide_search_top_k: self.wide_search_top_k,
            triplet_distance_penalty: self.triplet_distance_penalty,
        };

        let ranked_edges = brute_force_triplet_search(
            query,
            self.vector_db.as_ref(),
            self.embedding_engine.as_ref(),
            self.graph_db.as_ref(),
            &config,
        )
        .await?;

        Ok(ranked_edges
            .into_iter()
            .map(|edge| SearchItem {
                id: None,
                score: Some(edge.score),
                payload: json!({
                    "source_id": edge.source_id,
                    "target_id": edge.target_id,
                    "relationship": edge.relationship_name,
                    "source_name": edge.source_name,
                    "target_name": edge.target_name,
                    "source_text": edge.source_text,
                    "target_text": edge.target_text,
                    "source_description": edge.source_description,
                    "target_description": edge.target_description,
                    "dataset_id": edge.dataset_id,
                }),
            })
            .collect())
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
    ) -> Result<SearchOutput, SearchError> {
        let completion_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        let graph_context_text = render_edges_context(&completion_context);

        let system_prompt = resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_path.as_deref(),
        )?;

        let user_prompt = render_graph_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &graph_context_text,
        );

        debug!(
            context_items = completion_context.len(),
            "Graph context assembled:\n{graph_context_text}"
        );
        debug!("LLM user prompt:\n{user_prompt}");

        let completion = self
            .llm
            .generate(
                build_messages_with_history(system_prompt, user_prompt, session),
                self.generation_options.clone(),
            )
            .await?;

        Ok(SearchOutput::Text(completion.content))
    }
}

#[cfg(test)]
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
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};

    use serde_json::json;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use crate::retrievers::{GraphCompletionRetriever, SearchRetriever};
    use crate::types::SearchOutput;

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
            // Use the default penalty (3.5) — unmatched edge types get this distance.
            // Alice (dist 0.05) + Bob (dist 0.20) + KNOWS (unmatched: 3.5) = 3.75
            // Bob (dist 0.20) + Charlie (dist 0.60) + WORKS_WITH (unmatched: 3.5) = 4.30
            // Sort ascending: KNOWS (3.75) first, WORKS_WITH (4.30) second.
            None,
            None,
            None,
            None,
            None,
        );

        let context = retriever.get_context("query").await.unwrap();

        assert_eq!(context.len(), 2);
        assert_eq!(context[0].payload["relationship"], "KNOWS");
        assert_eq!(context[0].payload["source_name"], "Alice");
        assert_eq!(context[0].payload["target_name"], "Bob");
        assert_eq!(context[1].payload["relationship"], "WORKS_WITH");
        // Verify distance-based scores (lower = better):
        // KNOWS: 0.05 + 0.20 + 3.5 = 3.75; WORKS_WITH: 0.20 + 0.60 + 3.5 = 4.30
        let score_knows = context[0].score.unwrap();
        let score_works_with = context[1].score.unwrap();
        assert!(
            score_knows < score_works_with,
            "KNOWS distance ({score_knows}) should be less than WORKS_WITH distance ({score_works_with})"
        );
        assert!(
            (score_knows - 3.75).abs() < 1e-5,
            "KNOWS expected score 3.75, got {score_knows}"
        );
        assert!(
            (score_works_with - 4.30).abs() < 1e-5,
            "WORKS_WITH expected score 4.30, got {score_works_with}"
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
            .get_completion("Who knows Bob?", Some(context), &SessionContext::default())
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
}
