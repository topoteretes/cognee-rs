use std::collections::HashMap;
use std::sync::Arc;

use cognee_database::DatabaseTrait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_vector::VectorDB;

use crate::orchestration::{SearchOrchestrator, SearchTypeRegistry};
use crate::retrievers::{
    ChunksRetriever, CodingRulesRetriever, CompletionRetriever, CypherSearchRetriever,
    FeedbackRetriever, FeelingLuckyRetriever, GraphCompletionContextExtensionRetriever,
    GraphCompletionCotRetriever, GraphCompletionRetriever, GraphSummaryCompletionRetriever,
    JaccardChunksRetriever, NaturalLanguageRetriever, SearchRetrieverRef, SummariesRetriever,
    TemporalRetriever, TripletRetriever,
};
use crate::types::SearchType;

pub struct SearchBuilder {
    retrievers: HashMap<SearchType, SearchRetrieverRef>,
    database: Arc<dyn DatabaseTrait>,
}

impl SearchBuilder {
    pub fn new<V, E, G, L>(
        vector_db: Arc<V>,
        embedding_engine: Arc<E>,
        graph_db: Arc<G>,
        llm: Arc<L>,
        database: Arc<dyn DatabaseTrait>,
    ) -> Self
    where
        V: VectorDB + 'static,
        E: EmbeddingEngine + 'static,
        G: GraphDBTrait + 'static,
        L: Llm + 'static,
    {
        Self {
            retrievers: HashMap::new(),
            database,
        }
        .register_standard_retrievers(vector_db, embedding_engine, graph_db, llm)
    }

    pub fn register_retriever(mut self, retriever: SearchRetrieverRef) -> Self {
        self.retrievers.insert(retriever.search_type(), retriever);
        self
    }

    fn register_standard_retrievers<V, E, G, L>(
        mut self,
        vector_db: Arc<V>,
        embedding_engine: Arc<E>,
        graph_db: Arc<G>,
        llm: Arc<L>,
    ) -> Self
    where
        V: VectorDB + 'static,
        E: EmbeddingEngine + 'static,
        G: GraphDBTrait + 'static,
        L: Llm + 'static,
    {
        self.retrievers.insert(
            SearchType::Chunks,
            Arc::new(ChunksRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::Summaries,
            Arc::new(SummariesRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::RagCompletion,
            Arc::new(CompletionRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&llm),
                None,
                None,
                None,
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::TripletCompletion,
            Arc::new(TripletRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&llm),
                None,
                None,
                None,
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::GraphCompletion,
            Arc::new(GraphCompletionRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::GraphSummaryCompletion,
            Arc::new(GraphSummaryCompletionRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::GraphCompletionContextExtension,
            Arc::new(GraphCompletionContextExtensionRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::GraphCompletionCot,
            Arc::new(GraphCompletionCotRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::Cypher,
            Arc::new(CypherSearchRetriever::new(Arc::clone(&graph_db))),
        );

        self.retrievers.insert(
            SearchType::NaturalLanguage,
            Arc::new(NaturalLanguageRetriever::new(
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::Temporal,
            Arc::new(TemporalRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::ChunksLexical,
            Arc::new(JaccardChunksRetriever::new(
                Arc::clone(&graph_db),
                None,
                false,
                None,
                false,
            )),
        );

        self.retrievers.insert(
            SearchType::Feedback,
            Arc::new(FeedbackRetriever::new(
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
                None,
            )),
        );

        self.retrievers.insert(
            SearchType::CodingRules,
            Arc::new(CodingRulesRetriever::new(Arc::clone(&graph_db), None)),
        );

        let feeling_lucky_retrievers = self.retrievers.clone();
        self.retrievers.insert(
            SearchType::FeelingLucky,
            Arc::new(FeelingLuckyRetriever::new(
                llm,
                feeling_lucky_retrievers,
                Some(SearchType::RagCompletion),
                None,
            )),
        );

        self
    }

    pub fn build(self) -> SearchOrchestrator {
        let mut registry = SearchTypeRegistry::new();
        for retriever in self.retrievers.values() {
            registry.register(Arc::clone(retriever));
        }

        SearchOrchestrator::new(registry).with_database(self.database)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use cognee_database::{DatabaseError, DatabaseTrait, SearchHistoryEntry};
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_graph::{EdgeData, GraphDBResult, GraphDBTrait, GraphNode, NodeData};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };
    use cognee_models::{Data, Dataset};
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::borrow::Cow;
    use uuid::Uuid;

    use super::SearchBuilder;
    use crate::retrievers::SearchRetriever;
    use crate::types::{SearchContext, SearchError, SearchOutput, SearchRequest, SearchType};

    struct TestEmbedding;

    #[async_trait]
    impl EmbeddingEngine for TestEmbedding {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![0.1, 0.2]])
        }

        fn dimension(&self) -> usize {
            2
        }

        fn batch_size(&self) -> usize {
            8
        }

        fn max_sequence_length(&self) -> usize {
            128
        }
    }

    struct TestVectorDb;

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

        async fn has_collection(
            &self,
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<bool> {
            Ok(false)
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
            _data_type: &str,
            _field_name: &str,
            _query_vector: &[f32],
            _top_k: usize,
        ) -> VectorDBResult<Vec<SearchResult>> {
            Ok(vec![])
        }

        async fn delete_collection(
            &self,
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<()> {
            Ok(())
        }

        async fn collection_size(
            &self,
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<usize> {
            Ok(0)
        }
    }

    struct TestGraphDb;

    #[async_trait]
    impl GraphDBTrait for TestGraphDb {
        async fn initialize(&self) -> GraphDBResult<()> {
            Ok(())
        }

        async fn is_empty(&self) -> GraphDBResult<bool> {
            Ok(true)
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

        async fn add_node<T: Serialize + Sync>(&self, _node: &T) -> GraphDBResult<()> {
            Ok(())
        }

        async fn add_nodes<T: Serialize + Sync>(&self, _nodes: &[&T]) -> GraphDBResult<()> {
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
            Ok((vec![], vec![]))
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

    struct TestLlm;

    #[async_trait]
    impl Llm for TestLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Ok(GenerationResponse {
                content: "ok".to_string(),
                model: "test".to_string(),
                usage: Some(TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                }),
                finish_reason: Some("stop".to_string()),
            })
        }

        async fn create_structured_output<T>(
            &self,
            _text_input: &str,
            _system_prompt: &str,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<T>
        where
            T: Serialize + for<'de> Deserialize<'de> + JsonSchema + Send,
        {
            Err(LlmError::ConfigError("not used in this test".to_string()))
        }

        async fn create_structured_output_with_messages<T>(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<T>
        where
            T: Serialize + for<'de> Deserialize<'de> + JsonSchema + Send,
        {
            Err(LlmError::ConfigError("not used in this test".to_string()))
        }

        fn model(&self) -> &str {
            "test"
        }
    }

    struct TestDatabase;

    #[async_trait]
    impl DatabaseTrait for TestDatabase {
        async fn create_data(&self, _data: Data) -> Result<Data, DatabaseError> {
            Err(DatabaseError::NotFound("unused".to_string()))
        }

        async fn get_data(&self, _id: Uuid) -> Result<Option<Data>, DatabaseError> {
            Ok(None)
        }

        async fn update_data(&self, _data: Data) -> Result<Data, DatabaseError> {
            Err(DatabaseError::NotFound("unused".to_string()))
        }

        async fn get_dataset_data(&self, _dataset_id: Uuid) -> Result<Vec<Data>, DatabaseError> {
            Ok(vec![])
        }

        async fn create_dataset(&self, _dataset: Dataset) -> Result<Dataset, DatabaseError> {
            Err(DatabaseError::NotFound("unused".to_string()))
        }

        async fn get_dataset(&self, _id: Uuid) -> Result<Option<Dataset>, DatabaseError> {
            Ok(None)
        }

        async fn get_dataset_by_name(
            &self,
            _name: &str,
            _owner_id: Uuid,
        ) -> Result<Option<Dataset>, DatabaseError> {
            Ok(None)
        }

        async fn attach_data_to_dataset(
            &self,
            _dataset_id: Uuid,
            _data_id: Uuid,
        ) -> Result<(), DatabaseError> {
            Ok(())
        }

        async fn log_query(
            &self,
            _query_text: &str,
            _query_type: &str,
            _user_id: Option<Uuid>,
        ) -> Result<Uuid, DatabaseError> {
            Ok(Uuid::new_v4())
        }

        async fn log_result(
            &self,
            _query_id: Uuid,
            _serialized_result: &str,
            _user_id: Option<Uuid>,
        ) -> Result<Uuid, DatabaseError> {
            Ok(Uuid::new_v4())
        }

        async fn get_history(
            &self,
            _user_id: Option<Uuid>,
            _limit: Option<usize>,
        ) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
            Ok(vec![])
        }

        async fn initialize(&self) -> Result<(), DatabaseError> {
            Ok(())
        }
    }

    struct FakeRetriever;

    #[async_trait]
    impl SearchRetriever for FakeRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Chunks
        }

        async fn get_context(&self, _query: &str) -> Result<SearchContext, SearchError> {
            Ok(vec![])
        }

        async fn get_completion(
            &self,
            _query: &str,
            _context: Option<SearchContext>,
            _session_id: Option<&str>,
        ) -> Result<SearchOutput, SearchError> {
            Ok(SearchOutput::Text("builder-executed".to_string()))
        }
    }

    #[tokio::test]
    async fn executes_search_via_builder_entrypoint() {
        let orchestrator = SearchBuilder::new(
            Arc::new(TestVectorDb),
            Arc::new(TestEmbedding),
            Arc::new(TestGraphDb),
            Arc::new(TestLlm),
            Arc::new(TestDatabase),
        )
        .register_retriever(Arc::new(FakeRetriever))
        .build();

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::Chunks,
            top_k: Some(3),
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(false),
            use_combined_context: Some(false),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            verbose: None,
        };

        let response = orchestrator.search(&request).await.unwrap();

        match response.result {
            SearchOutput::Text(text) => assert_eq!(text, "builder-executed"),
            _ => panic!("expected text result"),
        }
    }

    #[tokio::test]
    async fn supports_context_only_execution_through_entrypoint() {
        struct ContextRetriever;

        #[async_trait]
        impl SearchRetriever for ContextRetriever {
            fn search_type(&self) -> SearchType {
                SearchType::Summaries
            }

            async fn get_context(&self, _query: &str) -> Result<SearchContext, SearchError> {
                Ok(vec![crate::types::SearchItem {
                    id: None,
                    score: Some(0.9),
                    payload: json!({ "text": "summary item" }),
                }])
            }

            async fn get_completion(
                &self,
                _query: &str,
                _context: Option<SearchContext>,
                _session_id: Option<&str>,
            ) -> Result<SearchOutput, SearchError> {
                Ok(SearchOutput::Text("unused".to_string()))
            }
        }

        let orchestrator = SearchBuilder::new(
            Arc::new(TestVectorDb),
            Arc::new(TestEmbedding),
            Arc::new(TestGraphDb),
            Arc::new(TestLlm),
            Arc::new(TestDatabase),
        )
        .register_retriever(Arc::new(ContextRetriever))
        .build();

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::Summaries,
            top_k: Some(3),
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: Some(true),
            use_combined_context: Some(false),
            session_id: None,
            node_type: None,
            node_name: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
            verbose: None,
        };

        let response = orchestrator.search(&request).await.unwrap();
        match response.result {
            SearchOutput::Items(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].payload["text"], "summary item");
            }
            _ => panic!("expected items result"),
        }
    }
}
