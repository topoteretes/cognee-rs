use std::collections::HashMap;
use std::sync::Arc;

use cognee_database::{AclDb, IngestDb, SearchHistoryDb};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_session::SessionManager;
use cognee_vector::VectorDB;

use crate::orchestration::{SearchOrchestrator, SearchTypeRegistry};
use crate::retrievers::{
    ChunksRetriever, CodingRulesRetriever, CompletionRetriever, CypherSearchRetriever,
    FeedbackRetriever, FeelingLuckyRetriever, GraphCompletionContextExtensionRetriever,
    GraphCompletionCotRetriever, GraphCompletionDecompositionRetriever, GraphCompletionRetriever,
    GraphReportRetriever, GraphSummaryCompletionRetriever, HybridBudget, HybridRetriever,
    LexicalRetriever, NaturalLanguageRetriever, SearchRetrieverRef, SummariesRetriever,
    TemporalHybridRetriever, TripletRetriever,
};
use crate::types::SearchType;

/// Tuning the standard retrievers are built with (the `search` and
/// `search.hybrid` settings).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchTuning {
    /// Allow the CYPHER and NATURAL_LANGUAGE search types to run graph queries
    /// (Python's `ALLOW_CYPHER_QUERY`). Default `true`.
    pub allow_cypher_query: bool,
    /// Neighbours expanded per seed node in triplet search; `None` (the
    /// default) is uncapped. See
    /// [`GraphRetrievalConfig::neighbor_cap`](crate::graph_retrieval::GraphRetrievalConfig::neighbor_cap).
    pub triplet_neighbor_cap: Option<usize>,
    /// The HYBRID_COMPLETION context budget.
    pub hybrid: HybridBudget,
    /// Update `last_accessed` on the source data records of each search's
    /// context (Python's `ENABLE_LAST_ACCESSED`). Default `false`.
    pub access_tracking: bool,
    /// Detect feedback in session searches when the request leaves
    /// `auto_feedback_detection` unset (Python's `AUTO_FEEDBACK`). Default
    /// `false`.
    pub auto_feedback: bool,
    /// System prompt file used when a request names neither a system prompt
    /// nor a prompt path; `None` (the default) leaves each retriever's own
    /// default prompt.
    pub default_system_prompt_path: Option<String>,
}

impl Default for SearchTuning {
    fn default() -> Self {
        Self {
            allow_cypher_query: true,
            triplet_neighbor_cap: None,
            hybrid: HybridBudget::default(),
            access_tracking: false,
            auto_feedback: false,
            default_system_prompt_path: None,
        }
    }
}

/// The engines the standard retrievers are built over.
struct StandardEngines {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    graph_db: Arc<dyn GraphDBTrait>,
    llm: Arc<dyn Llm>,
}

pub struct SearchBuilder {
    engines: StandardEngines,
    tuning: SearchTuning,
    /// Retrievers registered with [`Self::register_retriever`]; they replace
    /// the standard retriever of the same search type.
    retrievers: HashMap<SearchType, SearchRetrieverRef>,
    database: Arc<dyn SearchHistoryDb>,
    dataset_resolver: Option<Arc<dyn IngestDb>>,
    acl_db: Option<Arc<dyn AclDb>>,
    session_manager: Option<Arc<SessionManager>>,
    log_search_history: bool,
    per_dataset_storage: bool,
}

impl SearchBuilder {
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        database: Arc<dyn SearchHistoryDb>,
    ) -> Self {
        Self {
            engines: StandardEngines {
                vector_db,
                embedding_engine,
                graph_db,
                llm,
            },
            tuning: SearchTuning::default(),
            retrievers: HashMap::new(),
            database,
            dataset_resolver: None,
            acl_db: None,
            session_manager: None,
            log_search_history: true,
            per_dataset_storage: false,
        }
    }

    /// Tune the standard retrievers (`search` / `search.hybrid` settings).
    /// They are built in [`Self::build`], so the order of builder calls does
    /// not matter.
    #[must_use]
    pub fn with_tuning(mut self, tuning: SearchTuning) -> Self {
        self.tuning = tuning;
        self
    }

    pub fn with_session_manager(mut self, session_manager: Arc<SessionManager>) -> Self {
        self.session_manager = Some(session_manager);
        self
    }

    /// Wire in an `IngestDb`-backed resolver so dataset name strings can be
    /// translated to UUIDs when `SearchRequest.datasets` is set. Without one,
    /// requests carrying `datasets` fail with `SearchError::InvalidInput`.
    pub fn with_dataset_resolver(mut self, resolver: Arc<dyn IngestDb>) -> Self {
        self.dataset_resolver = Some(resolver);
        self
    }

    /// Wire in an ACL backend so explicit `SearchRequest.dataset_ids` are
    /// authorized against the caller's `read` grants rather than ownership.
    /// See [`SearchOrchestrator::with_acl_db`].
    pub fn with_acl_db(mut self, acl_db: Arc<dyn AclDb>) -> Self {
        self.acl_db = Some(acl_db);
        self
    }

    /// See [`SearchOrchestrator::with_search_history_logging`].
    pub fn with_search_history_logging(mut self, enabled: bool) -> Self {
        self.log_search_history = enabled;
        self
    }

    /// See [`SearchOrchestrator::with_per_dataset_storage`]: on when the
    /// engines handed to [`Self::new`] are per-dataset storage's routing
    /// engines (`ComponentManager::dataset_storage` is `Some`).
    pub fn with_per_dataset_storage(mut self, enabled: bool) -> Self {
        self.per_dataset_storage = enabled;
        self
    }

    pub fn register_retriever(mut self, retriever: SearchRetrieverRef) -> Self {
        self.retrievers.insert(retriever.search_type(), retriever);
        self
    }

    fn standard_retrievers(
        engines: StandardEngines,
        tuning: &SearchTuning,
    ) -> HashMap<SearchType, SearchRetrieverRef> {
        let StandardEngines {
            vector_db,
            embedding_engine,
            graph_db,
            llm,
        } = engines;
        let mut retrievers: HashMap<SearchType, SearchRetrieverRef> = HashMap::new();
        retrievers.insert(
            SearchType::Chunks,
            Arc::new(ChunksRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                None,
            )),
        );

        retrievers.insert(
            SearchType::Summaries,
            Arc::new(SummariesRetriever::new(
                Arc::clone(&vector_db),
                Arc::clone(&embedding_engine),
                None,
            )),
        );

        retrievers.insert(
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

        retrievers.insert(
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

        retrievers.insert(
            SearchType::GraphCompletion,
            Arc::new(
                GraphCompletionRetriever::new(
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
                )
                .with_neighbor_cap(tuning.triplet_neighbor_cap),
            ),
        );

        retrievers.insert(
            SearchType::GraphCompletionDecomposition,
            Arc::new(
                GraphCompletionDecompositionRetriever::new(
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
                )
                .with_neighbor_cap(tuning.triplet_neighbor_cap),
            ),
        );

        retrievers.insert(
            SearchType::GraphSummaryCompletion,
            Arc::new(
                GraphSummaryCompletionRetriever::new(
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
                )
                .with_neighbor_cap(tuning.triplet_neighbor_cap),
            ),
        );

        retrievers.insert(
            SearchType::GraphCompletionContextExtension,
            Arc::new(
                GraphCompletionContextExtensionRetriever::new(
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
                )
                .with_neighbor_cap(tuning.triplet_neighbor_cap),
            ),
        );

        retrievers.insert(
            SearchType::GraphCompletionCot,
            Arc::new(
                GraphCompletionCotRetriever::new(
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
                )
                .with_neighbor_cap(tuning.triplet_neighbor_cap),
            ),
        );

        retrievers.insert(
            SearchType::Cypher,
            Arc::new(
                CypherSearchRetriever::new(Arc::clone(&graph_db))
                    .with_allow_cypher_query(tuning.allow_cypher_query),
            ),
        );

        retrievers.insert(
            SearchType::NaturalLanguage,
            Arc::new(
                NaturalLanguageRetriever::new(Arc::clone(&graph_db), Arc::clone(&llm), None, None)
                    .with_allow_cypher_query(tuning.allow_cypher_query),
            ),
        );

        retrievers.insert(
            SearchType::Temporal,
            // Python maps TEMPORAL to `TemporalHybridRetriever`
            // (`get_search_type_retriever_instance.py`): hybrid retrieval
            // reranked by the question's time window, with the same answering
            // budget as HYBRID_COMPLETION.
            Arc::new(
                TemporalHybridRetriever::new(
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
                )
                .with_budget(tuning.hybrid.clone()),
            ),
        );

        retrievers.insert(
            SearchType::ChunksLexical,
            // Python wires CHUNKS_LEXICAL to `BM25ChunksRetriever`
            // (`get_search_type_retriever_instance.py:353`).
            Arc::new(LexicalRetriever::bm25(
                Arc::clone(&graph_db),
                None,
                false,
                None,
            )),
        );

        retrievers.insert(
            SearchType::GraphReport,
            // Python wires GRAPH_REPORT to `GraphReportRetriever(top_n=top_k)`
            // (`get_search_type_retriever_instance.py:394`); the request's
            // `top_k` is read per search.
            Arc::new(GraphReportRetriever::new(
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
            )),
        );

        retrievers.insert(
            SearchType::HybridCompletion,
            Arc::new(
                HybridRetriever::new(
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
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .with_budget(tuning.hybrid.clone()),
            ),
        );

        retrievers.insert(
            SearchType::Feedback,
            Arc::new(FeedbackRetriever::new(
                Arc::clone(&graph_db),
                Arc::clone(&llm),
                None,
                None,
            )),
        );

        retrievers.insert(
            SearchType::CodingRules,
            Arc::new(CodingRulesRetriever::new(Arc::clone(&graph_db), None)),
        );

        let feeling_lucky_retrievers = retrievers.clone();
        retrievers.insert(
            SearchType::FeelingLucky,
            Arc::new(FeelingLuckyRetriever::new(
                llm,
                feeling_lucky_retrievers,
                Some(SearchType::RagCompletion),
                None,
            )),
        );

        retrievers
    }

    pub fn build(self) -> SearchOrchestrator {
        // The orchestrator's own LLM runs session feedback detection.
        let llm = Arc::clone(&self.engines.llm);
        let mut retrievers = Self::standard_retrievers(self.engines, &self.tuning);
        retrievers.extend(self.retrievers);
        let mut registry = SearchTypeRegistry::new();
        for retriever in retrievers.values() {
            registry.register(Arc::clone(retriever));
        }

        let mut orchestrator = SearchOrchestrator::new(registry)
            .with_database(self.database)
            .with_llm(llm)
            .with_auto_feedback_default(self.tuning.auto_feedback)
            .with_default_system_prompt_path(self.tuning.default_system_prompt_path)
            .with_search_history_logging(self.log_search_history)
            .with_per_dataset_storage(self.per_dataset_storage);
        if self.tuning.access_tracking {
            orchestrator = orchestrator.with_access_tracking();
        }
        if let Some(resolver) = self.dataset_resolver {
            orchestrator = orchestrator.with_dataset_resolver(resolver);
        }
        if let Some(acl_db) = self.acl_db {
            orchestrator = orchestrator.with_acl_db(acl_db);
        }
        if let Some(session_manager) = self.session_manager {
            orchestrator = orchestrator.with_session_manager(session_manager);
        }
        orchestrator
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use async_trait::async_trait;
    use cognee_database::{DatabaseError, SearchHistoryDb, SearchHistoryEntry};
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_graph::{EdgeData, GraphDBResult, GraphDBTrait, GraphNode, NodeData};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};

    use serde_json::json;
    use std::borrow::Cow;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use super::SearchBuilder;
    use crate::retrievers::SearchRetriever;
    use crate::types::{
        SearchContext, SearchError, SearchOutput, SearchParams, SearchRequest, SearchType,
    };

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
            _node_name_filter_operator: &str,
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

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<serde_json::Value> {
            Err(LlmError::ConfigError("not used in this test".to_string()))
        }

        fn model(&self) -> &str {
            "test"
        }
    }

    struct TestDatabase;

    #[async_trait]
    impl SearchHistoryDb for TestDatabase {
        async fn log_query(
            &self,
            _query_text: &str,
            _query_type: &str,
            _user_id: Option<Uuid>,
            _dataset_id: Option<Uuid>,
        ) -> Result<Uuid, DatabaseError> {
            Ok(Uuid::new_v4())
        }

        async fn log_result(
            &self,
            _query_id: Uuid,
            _serialized_result: &str,
            _user_id: Option<Uuid>,
            _dataset_id: Option<Uuid>,
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
    }

    struct FakeRetriever;

    #[async_trait]
    impl SearchRetriever for FakeRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Chunks
        }

        async fn get_context(
            &self,
            _query: &str,
            _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            Ok(vec![])
        }

        async fn get_completion(
            &self,
            _query: &str,
            _context: Option<SearchContext>,
            _session: &SessionContext,
            _params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            Ok(SearchOutput::Text("builder-executed".to_string()))
        }
    }

    fn request(search_type: SearchType) -> SearchRequest {
        SearchRequest {
            query_text: "hello".to_string(),
            search_type,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            user_id: None,
            tenant_id: None,
            verbose: None,
            feedback_influence: None,
            retriever_specific_config: None,
            response_schema: None,
            custom_search_type: None,
            auto_feedback_detection: None,
            neighborhood_depth: None,
            neighborhood_seed_top_k: None,
            summarize_context: None,
        }
    }

    fn builder() -> SearchBuilder {
        SearchBuilder::new(
            Arc::new(TestVectorDb),
            Arc::new(TestEmbedding),
            Arc::new(TestGraphDb),
            Arc::new(TestLlm),
            Arc::new(TestDatabase),
        )
    }

    /// Answers with the system prompt path the retriever was handed.
    struct PromptPathRetriever;

    #[async_trait]
    impl SearchRetriever for PromptPathRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::Chunks
        }

        async fn get_context(
            &self,
            _query: &str,
            _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            Ok(vec![])
        }

        async fn get_completion(
            &self,
            _query: &str,
            _context: Option<SearchContext>,
            _session: &SessionContext,
            params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            Ok(SearchOutput::Text(
                params.system_prompt_path.clone().unwrap_or_default(),
            ))
        }
    }

    async fn prompt_path_seen(
        orchestrator: &super::SearchOrchestrator,
        req: SearchRequest,
    ) -> String {
        match orchestrator.search(&req).await.unwrap().result {
            SearchOutput::Text(text) => text,
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// `default_system_prompt_path` reaches a request that names no prompt,
    /// and never overrides one that does.
    #[tokio::test]
    async fn default_system_prompt_path_fills_only_an_unset_prompt() {
        let orchestrator = builder()
            .with_tuning(super::SearchTuning {
                default_system_prompt_path: Some("/prompts/custom.txt".to_string()),
                ..super::SearchTuning::default()
            })
            .register_retriever(Arc::new(PromptPathRetriever))
            .build();

        let plain = request(SearchType::Chunks);
        assert_eq!(
            prompt_path_seen(&orchestrator, plain).await,
            "/prompts/custom.txt"
        );

        let mut explicit = request(SearchType::Chunks);
        explicit.system_prompt_path = Some("/prompts/explicit.txt".to_string());
        assert_eq!(
            prompt_path_seen(&orchestrator, explicit).await,
            "/prompts/explicit.txt"
        );

        let mut inline = request(SearchType::Chunks);
        inline.system_prompt = Some("inline".to_string());
        assert_eq!(prompt_path_seen(&orchestrator, inline).await, "");

        // Unset: the retriever keeps its own default.
        let untuned = builder()
            .register_retriever(Arc::new(PromptPathRetriever))
            .build();
        assert_eq!(
            prompt_path_seen(&untuned, request(SearchType::Chunks)).await,
            ""
        );
    }

    /// `access_tracking` and `auto_feedback` reach the orchestrator, and
    /// are off unless set.
    #[test]
    fn orchestrator_knobs_reach_the_orchestrator() {
        assert_eq!(builder().build().tuning_flags(), (false, false));
        let tuned = builder()
            .with_tuning(super::SearchTuning {
                access_tracking: true,
                auto_feedback: true,
                ..super::SearchTuning::default()
            })
            .build();
        assert_eq!(tuned.tuning_flags(), (true, true));
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            user_id: None,
            tenant_id: None,
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

            async fn get_context(
                &self,
                _query: &str,
                _params: &SearchParams,
            ) -> Result<SearchContext, SearchError> {
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
                _session: &SessionContext,
                _params: &SearchParams,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            user_id: None,
            tenant_id: None,
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

        let response = orchestrator.search(&request).await.unwrap();
        match response.result {
            SearchOutput::Items(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].payload["text"], "summary item");
            }
            _ => panic!("expected items result"),
        }
    }

    #[tokio::test]
    async fn hybrid_completion_resolves_to_a_registered_retriever() {
        // With P1-09, HYBRID_COMPLETION is registered by the standard builder.
        // The empty `TestVectorDb` has no DocumentChunk_text collection, so the
        // registered `HybridRetriever` runs and fails with `NotFound` — proving
        // the type is no longer `UnsupportedSearchType`.
        let orchestrator = SearchBuilder::new(
            Arc::new(TestVectorDb),
            Arc::new(TestEmbedding),
            Arc::new(TestGraphDb),
            Arc::new(TestLlm),
            Arc::new(TestDatabase),
        )
        .build();

        let request = SearchRequest {
            query_text: "hello".to_string(),
            search_type: SearchType::HybridCompletion,
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
            node_name_filter_operator: None,
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            user_id: None,
            tenant_id: None,
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

        let result = orchestrator.search(&request).await;
        match result {
            Err(SearchError::UnsupportedSearchType(_)) => {
                panic!("HYBRID_COMPLETION should be registered after P1-09")
            }
            Err(SearchError::NotFound(_)) => {}
            other => {
                panic!("expected NotFound from the registered hybrid retriever, got {other:?}")
            }
        }
    }

    /// GRAPH_COMPLETION_DECOMPOSITION and GRAPH_REPORT are standard search
    /// types (SDK-1062), served by their own retrievers.
    #[test]
    fn decomposition_and_report_are_standard_retrievers() {
        use super::{SearchTuning, StandardEngines};

        let retrievers = SearchBuilder::standard_retrievers(
            StandardEngines {
                vector_db: Arc::new(TestVectorDb),
                embedding_engine: Arc::new(TestEmbedding),
                graph_db: Arc::new(TestGraphDb),
                llm: Arc::new(TestLlm),
            },
            &SearchTuning::default(),
        );
        for search_type in [
            SearchType::GraphCompletionDecomposition,
            SearchType::GraphReport,
        ] {
            let retriever = retrievers
                .get(&search_type)
                .unwrap_or_else(|| panic!("{search_type:?} is a standard retriever"));
            assert_eq!(retriever.search_type(), search_type);
        }
    }

    /// `with_tuning` must reach every standard retriever that consumes a
    /// knob: each is wired by a hand-written builder call, so a dropped one
    /// would silently fall back to the default for that search type.
    #[test]
    fn tuning_reaches_every_retriever_that_consumes_it() {
        use super::{SearchTuning, StandardEngines};
        use crate::retrievers::AppliedTuning;
        use crate::retrievers::HybridBudget;

        let tuning = SearchTuning {
            allow_cypher_query: false,
            triplet_neighbor_cap: Some(7),
            hybrid: HybridBudget {
                max_context_tokens: Some(321),
                summarize_overflow: true,
                summarize_batch: 3,
                overflow_summary_file: Some("summaries.json".into()),
            },
            // Orchestrator-level knobs, not retriever ones; covered by
            // `orchestrator_knobs_reach_the_orchestrator`.
            ..SearchTuning::default()
        };
        assert_ne!(tuning, SearchTuning::default(), "every knob is non-default");
        let retrievers = SearchBuilder::standard_retrievers(
            StandardEngines {
                vector_db: Arc::new(TestVectorDb),
                embedding_engine: Arc::new(TestEmbedding),
                graph_db: Arc::new(TestGraphDb),
                llm: Arc::new(TestLlm),
            },
            &tuning,
        );

        let capped = AppliedTuning {
            neighbor_cap: Some(tuning.triplet_neighbor_cap),
            ..Default::default()
        };
        let cypher = AppliedTuning {
            allow_cypher_query: Some(tuning.allow_cypher_query),
            ..Default::default()
        };
        let hybrid = AppliedTuning {
            hybrid: Some(tuning.hybrid.clone()),
            ..Default::default()
        };
        let expected = [
            (SearchType::GraphCompletion, &capped),
            (SearchType::GraphCompletionCot, &capped),
            (SearchType::GraphCompletionContextExtension, &capped),
            (SearchType::GraphCompletionDecomposition, &capped),
            (SearchType::GraphSummaryCompletion, &capped),
            (SearchType::Temporal, &hybrid),
            (SearchType::Cypher, &cypher),
            (SearchType::NaturalLanguage, &cypher),
            (SearchType::HybridCompletion, &hybrid),
        ];
        for (search_type, want) in expected {
            let retriever = retrievers
                .get(&search_type)
                .unwrap_or_else(|| panic!("{search_type:?} is a standard retriever"));
            assert_eq!(&retriever.applied_tuning(), want, "{search_type:?}");
        }
        // Every other standard retriever consumes no tuning knob.
        for (search_type, retriever) in &retrievers {
            if !expected.iter().any(|(t, _)| t == search_type) {
                assert_eq!(
                    retriever.applied_tuning(),
                    AppliedTuning::default(),
                    "{search_type:?}"
                );
            }
        }
    }
}
