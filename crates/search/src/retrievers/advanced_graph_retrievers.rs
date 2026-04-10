use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_vector::VectorDB;
use serde_json::json;

use cognee_session::SessionContext;

use crate::graph_retrieval::{
    DEFAULT_TRIPLET_DISTANCE_PENALTY, GraphRetrievalConfig, brute_force_triplet_search,
};
use crate::retrievers::SearchRetriever;
use crate::types::{SearchContext, SearchError, SearchItem, SearchOutput, SearchType};
use crate::utils::{
    DEFAULT_RAG_SYSTEM_PROMPT, build_messages_with_history, render_edges_context,
    render_graph_user_prompt, resolve_system_prompt,
};

const DEFAULT_TOP_K: usize = 5;
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;
const DEFAULT_CONTEXT_EXTENSION_ROUNDS: usize = 4;
const DEFAULT_COT_MAX_ITER: usize = 4;

const DEFAULT_GRAPH_SUMMARY_SYSTEM_PROMPT: &str = "You are a top-tier summarization engine that is meant to eliminate redundancies.\nThe input contains relationships enclosed by \\\"--\\\" .\nSummarize the input into natural sentences, listing all relationships.";
const DEFAULT_GRAPH_SUMMARY_USER_PROMPT: &str = "{context}";

const DEFAULT_COT_VALIDATION_SYSTEM_PROMPT: &str = "You are a helpful agent who are allowed to use only the provided question answer and context.\nI want to you find reasoning what is missing from the context or why the answer is not answering the question or not correct strictly based on the context.";
const DEFAULT_COT_VALIDATION_USER_PROMPT: &str = "<QUESTION>\n`{question}`\n</QUESTION>\n\n<ANSWER>\n`{answer}`\n</ANSWER>\n\n<CONTEXT>\n`{context}`\n</CONTEXT>";

const DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT: &str = "You are a helpful assistant whose job is to ask exactly one clarifying follow-up question,\nto collect the missing piece of information needed to fully answer the user's original query.\nRespond with the question only (no extra text, no punctuation beyond what's needed).";
const DEFAULT_COT_FOLLOW_UP_USER_PROMPT: &str = "Based on the following, ask exactly one question that would directly resolve the gap identified in the validation reasoning and allow a valid answer.\nThink in a way that with the followup question you are exploring a knowledge graph which contains entities, entity types and document chunks\n\n<QUERY>\n`{question}`\n</QUERY>\n\n<ANSWER>\n`{answer}`\n</ANSWER>\n\n<REASONING>\n`{validation}`\n</REASONING>";

struct GraphRetrieverCore {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    graph_db: Arc<dyn GraphDBTrait>,
    top_k: usize,
    wide_search_top_k: usize,
    triplet_distance_penalty: f32,
}

impl GraphRetrieverCore {
    fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        graph_db: Arc<dyn GraphDBTrait>,
        top_k: Option<usize>,
        wide_search_top_k: Option<usize>,
        triplet_distance_penalty: Option<f32>,
    ) -> Self {
        Self {
            vector_db,
            embedding_engine,
            graph_db,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            wide_search_top_k: wide_search_top_k.unwrap_or(DEFAULT_WIDE_SEARCH_TOP_K),
            triplet_distance_penalty: triplet_distance_penalty
                .unwrap_or(DEFAULT_TRIPLET_DISTANCE_PENALTY),
        }
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        if self.graph_db.is_empty().await? {
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
                }),
            })
            .collect())
    }
}

fn merge_dedup_context(left: &SearchContext, right: &SearchContext) -> SearchContext {
    let mut seen = HashSet::new();
    let mut merged = Vec::with_capacity(left.len() + right.len());

    for item in left.iter().chain(right.iter()) {
        let key = item
            .id
            .map(|id| id.to_string())
            .unwrap_or_else(|| item.payload.to_string());

        if seen.insert(key) {
            merged.push(item.clone());
        }
    }

    merged
}

pub struct GraphSummaryCompletionRetriever {
    core: GraphRetrieverCore,
    llm: Arc<dyn Llm>,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl GraphSummaryCompletionRetriever {
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
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }
}

#[async_trait]
impl SearchRetriever for GraphSummaryCompletionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphSummaryCompletion
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        self.core.get_context(query).await
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
        let summary_prompt =
            DEFAULT_GRAPH_SUMMARY_USER_PROMPT.replace("{context}", &graph_context_text);

        let summarized_context = self
            .llm
            .generate(
                vec![
                    Message::system(DEFAULT_GRAPH_SUMMARY_SYSTEM_PROMPT),
                    Message::user(summary_prompt),
                ],
                self.generation_options.clone(),
            )
            .await?
            .content;

        let system_prompt = resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_path.as_deref(),
        )?;

        let user_prompt = render_graph_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &summarized_context,
        );

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

pub struct GraphCompletionContextExtensionRetriever {
    core: GraphRetrieverCore,
    llm: Arc<dyn Llm>,
    context_extension_rounds: usize,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl GraphCompletionContextExtensionRetriever {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        top_k: Option<usize>,
        wide_search_top_k: Option<usize>,
        triplet_distance_penalty: Option<f32>,
        context_extension_rounds: Option<usize>,
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
            context_extension_rounds: context_extension_rounds
                .unwrap_or(DEFAULT_CONTEXT_EXTENSION_ROUNDS),
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }
}

#[async_trait]
impl SearchRetriever for GraphCompletionContextExtensionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletionContextExtension
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        self.core.get_context(query).await
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
    ) -> Result<SearchOutput, SearchError> {
        let system_prompt = resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_path.as_deref(),
        )?;

        let mut extended_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        for _ in 0..self.context_extension_rounds {
            let current_context_text = render_edges_context(&extended_context);
            let extension_prompt = render_graph_user_prompt(
                self.user_prompt_template.as_deref(),
                query,
                &current_context_text,
            );

            let completion = self
                .llm
                .generate(
                    vec![
                        Message::system(DEFAULT_RAG_SYSTEM_PROMPT),
                        Message::user(extension_prompt),
                    ],
                    self.generation_options.clone(),
                )
                .await?
                .content
                .trim()
                .to_string();

            if completion.is_empty() {
                break;
            }

            let new_context = self.get_context(&completion).await?;
            let merged_context = merge_dedup_context(&extended_context, &new_context);

            if merged_context.len() == extended_context.len() {
                break;
            }

            extended_context = merged_context;
        }

        let user_prompt = render_graph_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &render_edges_context(&extended_context),
        );

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

pub struct GraphCompletionCotRetriever {
    core: GraphRetrieverCore,
    llm: Arc<dyn Llm>,
    max_iter: usize,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl GraphCompletionCotRetriever {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        top_k: Option<usize>,
        wide_search_top_k: Option<usize>,
        triplet_distance_penalty: Option<f32>,
        max_iter: Option<usize>,
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
            max_iter: max_iter.unwrap_or(DEFAULT_COT_MAX_ITER),
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }
}

#[async_trait]
impl SearchRetriever for GraphCompletionCotRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletionCot
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        self.core.get_context(query).await
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
    ) -> Result<SearchOutput, SearchError> {
        let mut current_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        let system_prompt = resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_path.as_deref(),
        )?;

        // Step 1: Generate INITIAL completion (before any reasoning rounds)
        let context_text = render_edges_context(&current_context);
        let answer_prompt =
            render_graph_user_prompt(self.user_prompt_template.as_deref(), query, &context_text);

        let mut current_answer = self
            .llm
            .generate(
                build_messages_with_history(system_prompt.clone(), answer_prompt, session),
                self.generation_options.clone(),
            )
            .await?
            .content;

        // Step 2: Run max_iter REASONING rounds
        for _ in 0..self.max_iter {
            // 2a. Validate the current answer against the context
            let validation_prompt = DEFAULT_COT_VALIDATION_USER_PROMPT
                .replace("{question}", query)
                .replace("{answer}", &current_answer)
                .replace("{context}", &render_edges_context(&current_context));

            let validation = self
                .llm
                .generate(
                    vec![
                        Message::system(DEFAULT_COT_VALIDATION_SYSTEM_PROMPT),
                        Message::user(validation_prompt),
                    ],
                    self.generation_options.clone(),
                )
                .await?
                .content;

            // 2b. Generate follow-up question based on validation reasoning
            let follow_up_prompt = DEFAULT_COT_FOLLOW_UP_USER_PROMPT
                .replace("{question}", query)
                .replace("{answer}", &current_answer)
                .replace("{validation}", &validation);

            let follow_up_query = self
                .llm
                .generate(
                    vec![
                        Message::system(DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT),
                        Message::user(follow_up_prompt),
                    ],
                    self.generation_options.clone(),
                )
                .await?
                .content
                .trim()
                .to_string();

            if follow_up_query.is_empty() {
                break;
            }

            // 2c. Fetch new context using the follow-up question
            let additional_context = self.get_context(&follow_up_query).await?;
            current_context = merge_dedup_context(&current_context, &additional_context);

            // 2d. Regenerate completion with the enriched context
            let enriched_context_text = render_edges_context(&current_context);
            let regeneration_prompt = render_graph_user_prompt(
                self.user_prompt_template.as_deref(),
                query,
                &enriched_context_text,
            );

            current_answer = self
                .llm
                .generate(
                    build_messages_with_history(
                        system_prompt.clone(),
                        regeneration_prompt,
                        session,
                    ),
                    self.generation_options.clone(),
                )
                .await?
                .content;
        }

        Ok(SearchOutput::Text(current_answer))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_graph::MockGraphDB;
    use cognee_graph::{GraphDBTrait, GraphDBTraitExt};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};

    use serde::Serialize;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use crate::retrievers::{
        GraphCompletionContextExtensionRetriever, GraphCompletionCotRetriever,
        GraphSummaryCompletionRetriever, SearchRetriever,
    };
    use crate::types::{SearchOutput, SearchType};

    struct TestEmbeddingEngine;

    #[async_trait]
    impl EmbeddingEngine for TestEmbeddingEngine {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![0.1, 0.2]])
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

    struct TestLlm {
        queued_responses: Mutex<VecDeque<String>>,
        captured_messages: Mutex<Vec<Vec<Message>>>,
    }

    impl TestLlm {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                queued_responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(ToString::to_string)
                        .collect::<VecDeque<_>>(),
                ),
                captured_messages: Mutex::new(vec![]),
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
            self.captured_messages.lock().unwrap().push(messages);
            let content = self
                .queued_responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| "default response".to_string());

            Ok(GenerationResponse {
                content,
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

    #[derive(Serialize)]
    struct EntityNode {
        id: String,
        #[serde(rename = "type")]
        kind: String,
        name: String,
    }

    async fn build_graph_db() -> Arc<MockGraphDB> {
        let graph_db = Arc::new(MockGraphDB::new());

        let a = EntityNode {
            id: "00000000-0000-0000-0000-000000000001".to_string(),
            kind: "Entity".to_string(),
            name: "Alice".to_string(),
        };
        let b = EntityNode {
            id: "00000000-0000-0000-0000-000000000002".to_string(),
            kind: "Entity".to_string(),
            name: "Bob".to_string(),
        };

        graph_db.add_node(&a).await.unwrap();
        graph_db.add_node(&b).await.unwrap();
        graph_db
            .add_edge(&a.id, &b.id, "KNOWS", Some(HashMap::new()))
            .await
            .unwrap();

        graph_db
    }

    fn build_vector_db() -> Arc<TestVectorDb> {
        let mut collections = HashMap::new();
        collections.insert(
            TestVectorDb::key("Entity", "name"),
            vec![
                SearchResult {
                    id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
                    score: 0.9,
                    metadata: HashMap::new(),
                },
                SearchResult {
                    id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
                    score: 0.8,
                    metadata: HashMap::new(),
                },
            ],
        );

        Arc::new(TestVectorDb { collections })
    }

    #[tokio::test]
    async fn graph_summary_completion_uses_two_generation_steps() {
        let llm = Arc::new(TestLlm::new(vec!["short summary", "final summary answer"]));

        let retriever = GraphSummaryCompletionRetriever::new(
            build_vector_db(),
            Arc::new(TestEmbeddingEngine),
            build_graph_db().await,
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(5),
            Some(5),
            Some(0.0),
            None,
            None,
            None,
            None,
        );

        assert_eq!(retriever.search_type(), SearchType::GraphSummaryCompletion);
        let output = retriever
            .get_completion("Who knows Bob?", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "final summary answer"),
            _ => panic!("expected text output"),
        }

        assert_eq!(llm.captured_messages.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn graph_context_extension_returns_final_answer() {
        let llm = Arc::new(TestLlm::new(vec!["Find Bob relations", "extended answer"]));

        let retriever = GraphCompletionContextExtensionRetriever::new(
            build_vector_db(),
            Arc::new(TestEmbeddingEngine),
            build_graph_db().await,
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(5),
            Some(5),
            Some(0.0),
            Some(1),
            None,
            None,
            None,
            None,
        );

        assert_eq!(
            retriever.search_type(),
            SearchType::GraphCompletionContextExtension
        );
        let output = retriever
            .get_completion("Who knows Bob?", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "extended answer"),
            _ => panic!("expected text output"),
        }
    }

    #[tokio::test]
    async fn graph_context_extension_with_zero_rounds_returns_single_completion() {
        // With context_extension_rounds = 0, the loop body is never entered.
        // Only the final completion LLM call should be made.
        let llm = Arc::new(TestLlm::new(vec!["direct answer"]));

        let retriever = GraphCompletionContextExtensionRetriever::new(
            build_vector_db(),
            Arc::new(TestEmbeddingEngine),
            build_graph_db().await,
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(5),
            Some(5),
            Some(0.0),
            Some(0), // zero extension rounds
            None,
            None,
            None,
            None,
        );

        let output = retriever
            .get_completion("Who knows Bob?", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "direct answer"),
            _ => panic!("expected text output"),
        }

        // Exactly one LLM call: the final completion (no extension iterations).
        assert_eq!(llm.captured_messages.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn graph_cot_returns_answer_from_last_iteration() {
        let llm = Arc::new(TestLlm::new(vec![
            "first answer",
            "needs more evidence",
            "find graph neighbors",
            "second answer",
        ]));

        let retriever = GraphCompletionCotRetriever::new(
            build_vector_db(),
            Arc::new(TestEmbeddingEngine),
            build_graph_db().await,
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(5),
            Some(5),
            Some(0.0),
            Some(1),
            None,
            None,
            None,
            None,
        );

        assert_eq!(retriever.search_type(), SearchType::GraphCompletionCot);
        let output = retriever
            .get_completion("Who knows Bob?", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "second answer"),
            _ => panic!("expected text output"),
        }
    }

    #[tokio::test]
    async fn graph_cot_with_zero_rounds_returns_initial_completion_only() {
        // With max_iter = 0, the reasoning loop is never entered.
        // Only the initial completion LLM call should be made.
        let llm = Arc::new(TestLlm::new(vec!["the answer"]));

        let retriever = GraphCompletionCotRetriever::new(
            build_vector_db(),
            Arc::new(TestEmbeddingEngine),
            build_graph_db().await,
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(5),
            Some(5),
            Some(0.0),
            Some(0), // zero reasoning rounds
            None,
            None,
            None,
            None,
        );

        let output = retriever
            .get_completion("Who knows Bob?", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "the answer"),
            _ => panic!("expected text output"),
        }

        // Exactly one LLM call: the initial completion (no reasoning rounds).
        assert_eq!(llm.captured_messages.lock().unwrap().len(), 1);
    }
}
