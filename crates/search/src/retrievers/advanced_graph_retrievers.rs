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
    build_messages_with_history, render_edges_context, render_graph_user_prompt,
    resolve_system_prompt,
};

const DEFAULT_TOP_K: usize = 5;
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;
const DEFAULT_CONTEXT_EXTENSION_ROUNDS: usize = 4;
const DEFAULT_COT_MAX_ITER: usize = 4;

const DEFAULT_GRAPH_SUMMARY_SYSTEM_PROMPT: &str =
    "You summarize graph evidence into concise factual context.";
const DEFAULT_GRAPH_SUMMARY_USER_PROMPT: &str =
    "Summarize the following graph context:\n\n{context}";

const DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT: &str =
    "Generate a follow-up graph query that expands useful context for the question.";
const DEFAULT_CONTEXT_EXTENSION_USER_PROMPT: &str = "Original question:\n{question}\n\nCurrent graph context:\n{context}\n\nProvide one short follow-up graph query.";

const DEFAULT_COT_VALIDATION_SYSTEM_PROMPT: &str =
    "You validate whether an answer is sufficiently grounded in graph context.";
const DEFAULT_COT_VALIDATION_USER_PROMPT: &str = "Question:\n{question}\n\nAnswer:\n{answer}\n\nContext:\n{context}\n\nSay whether more context is needed and why.";

const DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT: &str =
    "Generate one concise follow-up graph query to improve the answer.";
const DEFAULT_COT_FOLLOW_UP_USER_PROMPT: &str = "Question:\n{question}\n\nAnswer:\n{answer}\n\nValidation:\n{validation}\n\nProvide one follow-up graph query.";

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
        let mut extended_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        for _ in 0..self.context_extension_rounds {
            let current_context_text = render_edges_context(&extended_context);
            let extension_prompt = DEFAULT_CONTEXT_EXTENSION_USER_PROMPT
                .replace("{question}", query)
                .replace("{context}", &current_context_text);

            let follow_up_query = self
                .llm
                .generate(
                    vec![
                        Message::system(DEFAULT_CONTEXT_EXTENSION_SYSTEM_PROMPT),
                        Message::user(extension_prompt),
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

            let new_context = self.get_context(&follow_up_query).await?;
            let merged_context = merge_dedup_context(&extended_context, &new_context);

            if merged_context.len() == extended_context.len() {
                break;
            }

            extended_context = merged_context;
        }

        let system_prompt = resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_path.as_deref(),
        )?;
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

        let mut final_answer = String::new();

        for iter_index in 0..self.max_iter {
            let answer_prompt = render_graph_user_prompt(
                self.user_prompt_template.as_deref(),
                query,
                &render_edges_context(&current_context),
            );

            final_answer = self
                .llm
                .generate(
                    build_messages_with_history(system_prompt.clone(), answer_prompt, session),
                    self.generation_options.clone(),
                )
                .await?
                .content;

            if iter_index + 1 >= self.max_iter {
                break;
            }

            let validation_prompt = DEFAULT_COT_VALIDATION_USER_PROMPT
                .replace("{question}", query)
                .replace("{answer}", &final_answer)
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

            let follow_up_prompt = DEFAULT_COT_FOLLOW_UP_USER_PROMPT
                .replace("{question}", query)
                .replace("{answer}", &final_answer)
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

            let additional_context = self.get_context(&follow_up_query).await?;
            current_context = merge_dedup_context(&current_context, &additional_context);
        }

        Ok(SearchOutput::Text(final_answer))
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
            Some(2),
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
}
