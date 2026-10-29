use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_vector::VectorDB;
use tracing::debug;

use cognee_session::SessionContext;

use crate::retrievers::SearchRetriever;
use crate::retrievers::context_items::search_node_filtered;
use crate::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};
use crate::user_preferences::{append_preference_guidance, load_for_search, personal_factor};
use crate::utils::{
    build_messages_with_history, render_user_prompt, resolve_system_prompt,
    skip_completion_on_empty_context,
};

const CHUNKS_DATA_TYPE: &str = "DocumentChunk";
const CHUNKS_FIELD_NAME: &str = "text";
const DEFAULT_TOP_K: usize = 10;
/// Candidates fetched before the personalized re-sort trims to `top_k`
/// (Python's `CompletionRetriever.wide_search_top_k` default).
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;

pub struct CompletionRetriever {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    llm: Arc<dyn Llm>,
    /// Where the requesting user's preference node lives; without one the
    /// retriever never personalizes.
    graph_db: Option<Arc<dyn GraphDBTrait>>,
    top_k: usize,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl CompletionRetriever {
    /// Read the requesting user's preferences from `graph_db` (their
    /// `prefers` weights re-rank the chunks, their preference text is the
    /// completion's guidance). Without it nothing is personalized.
    #[must_use]
    pub fn with_graph_db(mut self, graph_db: Arc<dyn GraphDBTrait>) -> Self {
        self.graph_db = Some(graph_db);
        self
    }

    async fn preferences(&self, params: &SearchParams) -> crate::ActivePreferences {
        match &self.graph_db {
            Some(graph_db) => load_for_search(graph_db.as_ref(), params).await,
            None => crate::ActivePreferences::default(),
        }
    }

    /// Keep the weights whose key is a chunk of `DocumentChunk_text`
    /// (Python's `_weights_matching_collection`): weights on graph entities
    /// can never match a chunk, so they must not widen the fetch. A failed
    /// lookup fails open to the unfiltered weights.
    async fn weights_matching_chunks(&self, weights: HashMap<String, f64>) -> HashMap<String, f64> {
        let ids: Vec<uuid::Uuid> = weights
            .keys()
            .filter_map(|id| uuid::Uuid::parse_str(id).ok())
            .collect();
        let rows = match self
            .vector_db
            .retrieve(CHUNKS_DATA_TYPE, CHUNKS_FIELD_NAME, &ids)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                debug!(%error, "preference weight collection check failed open");
                return weights;
            }
        };
        let mut present: HashSet<String> = HashSet::new();
        for row in rows {
            if let Some(id) = row.metadata.get("id").and_then(|id| id.as_str()) {
                present.insert(id.to_string());
            }
            present.insert(row.id.to_string());
        }
        weights
            .into_iter()
            .filter(|(id, _)| present.contains(id))
            .collect()
    }

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
        let context_text = context
            .iter()
            .filter_map(|item| item.payload.get("text").and_then(|value| value.as_str()))
            .collect::<Vec<_>>()
            .join("\n");

        if context_text.is_empty() {
            return Ok(None);
        }

        let system_prompt = resolve_system_prompt(
            params
                .system_prompt
                .as_deref()
                .or(self.system_prompt.as_deref()),
            params
                .system_prompt_path
                .as_deref()
                .or(self.system_prompt_path.as_deref()),
        )?;

        let user_prompt =
            render_user_prompt(self.user_prompt_template.as_deref(), query, &context_text);

        debug!(
            context_items = context.len(),
            "RAG context assembled:\n{context_text}"
        );
        debug!("LLM user prompt:\n{user_prompt}");

        Ok(Some(build_messages_with_history(
            system_prompt,
            user_prompt,
            session,
        )))
    }

    /// [`Self::build_completion_messages`] with the user's preference text as
    /// guidance after the question and context (Python's sessionless
    /// `SessionPrompt(guidance=load_preference_text())`).
    async fn personalized_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        let Some(mut messages) = self.build_completion_messages(query, context, session, params)?
        else {
            return Ok(None);
        };
        let preferences = self.preferences(params).await;
        append_preference_guidance(&mut messages, &preferences.rendered_text());
        Ok(Some(messages))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        llm: Arc<dyn Llm>,
        top_k: Option<usize>,
        system_prompt: Option<String>,
        system_prompt_path: Option<String>,
        user_prompt_template: Option<String>,
        generation_options: Option<GenerationOptions>,
    ) -> Self {
        Self {
            vector_db,
            embedding_engine,
            llm,
            graph_db: None,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }
}

#[async_trait]
impl SearchRetriever for CompletionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::RagCompletion
    }

    fn dataset_scoping(&self) -> super::DatasetScoping {
        super::DatasetScoping::Membership
    }

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        if !self
            .vector_db
            .has_collection(CHUNKS_DATA_TYPE, CHUNKS_FIELD_NAME)
            .await?
        {
            return Err(SearchError::NotFound(
                "missing vector collection: DocumentChunk_text".to_string(),
            ));
        }

        let embeddings = self.embedding_engine.embed_query(query).await?;
        let query_vector = embeddings.into_iter().next().ok_or_else(|| {
            SearchError::InvalidInput("embedding engine returned no vectors".to_string())
        })?;

        // Loaded before the search because it decides the limit: weights that
        // can match a chunk widen the fetch to `wide_search_top_k`, so
        // personalization can change which chunks make the cut, not just
        // their order. Otherwise the fetch and order are un-personalized.
        let top_k = params.top_k_or(self.top_k);
        let mut weights = self.preferences(params).await.weights;
        if !weights.is_empty() {
            weights = self.weights_matching_chunks(weights).await;
        }
        let limit = if weights.is_empty() {
            top_k
        } else {
            top_k.max(params.wide_search_top_k_or(DEFAULT_WIDE_SEARCH_TOP_K))
        };

        let mut context = search_node_filtered(
            self.vector_db.as_ref(),
            CHUNKS_DATA_TYPE,
            CHUNKS_FIELD_NAME,
            &query_vector,
            limit,
            params.node_name.as_deref(),
            params.node_name_filter_operator.as_deref().unwrap_or("OR"),
        )
        .await?;
        if !weights.is_empty() {
            let influence = params
                .personalization
                .map_or(0.0, |config| config.influence);
            sort_by_personal_score(&mut context, &weights, influence);
            context.truncate(top_k);
        }
        Ok(context)
    }

    async fn completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        self.personalized_messages(query, context, session, params)
            .await
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

        let Some(messages) = self
            .personalized_messages(query, &completion_context, session, params)
            .await?
        else {
            return Ok(skip_completion_on_empty_context());
        };

        if let Some(schema) = &params.response_schema {
            let structured_value = self
                .llm
                .create_structured_output_with_messages_raw(
                    messages,
                    schema,
                    self.generation_options.clone(),
                )
                .await
                .map_err(|e| SearchError::LlmError(e.to_string()))?;
            Ok(SearchOutput::Structured(structured_value))
        } else {
            let completion = self
                .llm
                .generate(messages, self.generation_options.clone())
                .await?;
            Ok(SearchOutput::Text(completion.content))
        }
    }
}

/// Stable re-sort of chunk hits by personalized distance (Python's
/// `_stable_sort_by_personal_distance`). Rust scores are similarities, so
/// each is taken back to Python's distance space (`1 - score`, never
/// negative for a cosine similarity) and a preferred chunk's distance shrinks
/// by `personal_factor`; scaling the similarity instead would demote a liked
/// chunk whose similarity is negative. A chunk with no weight keeps its
/// distance, and ties keep the engine's order. The chunk id is
/// `payload["id"]`, falling back to the hit id.
fn sort_by_personal_score(
    context: &mut SearchContext,
    weights: &HashMap<String, f64>,
    influence: f64,
) {
    let personalized_distance = |item: &crate::types::SearchItem| -> f64 {
        let distance = item
            .score
            .map_or(f64::INFINITY, |score| 1.0 - f64::from(score));
        let id = item
            .payload
            .get("id")
            .and_then(|id| id.as_str())
            .map(str::to_string)
            .or_else(|| item.id.map(|id| id.to_string()));
        match id.and_then(|id| weights.get(&id)) {
            Some(weight) => distance * personal_factor(*weight, influence, true),
            None => distance,
        }
    };
    context.sort_by(|a, b| personalized_distance(a).total_cmp(&personalized_distance(b)));
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};

    use serde_json::json;
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use crate::retrievers::{CompletionRetriever, SearchRetriever};
    use crate::types::{SearchContext, SearchError, SearchItem, SearchOutput, SearchParams};
    use crate::utils::DEFAULT_RAG_SYSTEM_PROMPT;

    struct TestEmbeddingEngine;

    #[async_trait]
    impl EmbeddingEngine for TestEmbeddingEngine {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![0.4, 0.6]])
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
        has_collection: bool,
        results: Vec<SearchResult>,
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

        async fn has_collection(
            &self,
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<bool> {
            Ok(self.has_collection)
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
            top_k: usize,
        ) -> VectorDBResult<Vec<SearchResult>> {
            Ok(self.results.iter().take(top_k).cloned().collect())
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
            _data_type: &str,
            _field_name: &str,
        ) -> VectorDBResult<usize> {
            Ok(self.results.len())
        }
    }

    #[derive(Default)]
    struct TestLlm {
        last_messages: Mutex<Vec<Message>>,
        response_text: String,
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

    fn sample_result(text: &str, score: f32) -> SearchResult {
        let mut metadata = HashMap::new();
        metadata.insert("text".to_string(), json!(text));

        SearchResult {
            id: Uuid::new_v4(),
            score,
            metadata,
        }
    }

    #[tokio::test]
    async fn returns_not_found_when_chunk_collection_missing() {
        let llm = Arc::new(TestLlm {
            response_text: "answer".to_string(),
            ..Default::default()
        });

        let retriever = CompletionRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: false,
                results: vec![],
            }),
            Arc::new(TestEmbeddingEngine),
            llm,
            Some(2),
            None,
            None,
            None,
            None,
        );

        let result = retriever
            .get_context("query", &SearchParams::default())
            .await;
        assert!(matches!(result, Err(SearchError::NotFound(_))));
    }

    #[tokio::test]
    async fn returns_deterministic_completion_and_renders_prompts() {
        let llm = Arc::new(TestLlm {
            response_text: "deterministic answer".to_string(),
            ..Default::default()
        });

        let retriever = CompletionRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: true,
                results: vec![
                    sample_result("chunk one", 0.93),
                    sample_result("chunk two", 0.88),
                ],
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(2),
            None,
            None,
            None,
            None,
        );

        let output = retriever
            .get_completion(
                "what happened?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "deterministic answer"),
            _ => panic!("expected text output"),
        }

        let messages = llm.last_messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, DEFAULT_RAG_SYSTEM_PROMPT);
        assert!(messages[1].content.contains("what happened?"));
        assert!(messages[1].content.contains("chunk one"));
        assert!(messages[1].content.contains("chunk two"));
    }

    #[tokio::test]
    async fn uses_provided_context_without_vector_lookup() {
        let llm = Arc::new(TestLlm {
            response_text: "context answer".to_string(),
            ..Default::default()
        });

        let retriever = CompletionRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: false,
                results: vec![],
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(2),
            Some("custom system prompt".to_string()),
            None,
            Some("Q={question}; C={context}".to_string()),
            None,
        );

        let provided_context: SearchContext = vec![SearchItem {
            id: None,
            score: Some(0.7),
            payload: json!({ "text": "provided chunk" }),
        }];

        let output = retriever
            .get_completion(
                "who?",
                Some(provided_context),
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "context answer"),
            _ => panic!("expected text output"),
        }

        let messages = llm.last_messages.lock().unwrap().clone();
        assert_eq!(messages[0].content, "custom system prompt");
        assert!(messages[1].content.contains("Q=who?; C=provided chunk"));
    }

    /// An existing but empty chunk collection is a normal miss: no LLM call
    /// (Python's `skip_completion_on_empty_context`, `completion_retriever.py:
    /// 265-270`).
    #[tokio::test]
    async fn empty_context_skips_the_llm() {
        let llm = Arc::new(TestLlm {
            response_text: "phantom answer".to_string(),
            ..Default::default()
        });

        let retriever = CompletionRetriever::new(
            Arc::new(TestVectorDb {
                has_collection: true,
                results: vec![],
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(2),
            None,
            None,
            None,
            None,
        );

        let output = retriever
            .get_completion(
                "what happened?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        assert!(matches!(output, SearchOutput::Texts(ref texts) if texts.is_empty()));
        assert!(llm.last_messages.lock().unwrap().is_empty(), "no LLM call");
    }

    /// RAG scopes its chunk search by `node_name` like Python's
    /// `CompletionRetriever` (`completion_retriever.py:136-144`): only in-set
    /// chunks reach the prompt.
    #[tokio::test]
    async fn node_name_scopes_the_rag_context() {
        use cognee_vector::VectorPoint;

        let db = cognee_vector::BruteForceVectorDB::new();
        db.create_collection("DocumentChunk", "text", 2)
            .await
            .unwrap();
        let point = |text: &str, sets: &[&str]| VectorPoint {
            id: Uuid::new_v4(),
            vector: vec![1.0, 0.0],
            metadata: HashMap::from([
                ("text".to_string(), json!(text)),
                ("belongs_to_set".to_string(), json!(sets)),
            ]),
        };
        db.index_points(
            "DocumentChunk",
            "text",
            &[
                point("in both sets", &["x", "y"]),
                point("only in x", &["x"]),
            ],
        )
        .await
        .unwrap();

        let llm = Arc::new(TestLlm {
            response_text: "answer".to_string(),
            ..Default::default()
        });
        let retriever = CompletionRetriever::new(
            Arc::new(db),
            Arc::new(TestEmbeddingEngine),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(10),
            None,
            None,
            None,
            None,
        );
        let params = SearchParams {
            node_name: Some(vec!["x".to_string(), "y".to_string()]),
            node_name_filter_operator: Some("AND".to_string()),
            ..SearchParams::default()
        };

        retriever
            .get_completion("q", None, &SessionContext::default(), &params)
            .await
            .unwrap();

        let prompt = llm.last_messages.lock().unwrap()[1].content.clone();
        assert!(prompt.contains("in both sets"), "got: {prompt}");
        assert!(!prompt.contains("only in x"), "got: {prompt}");
    }

    /// Python's `test_weights_wide_fetch_reorder_and_trim` and
    /// `test_preference_text_passed_as_guidance` (test_rag_personalization.py):
    /// a liked chunk the engine ranked second makes the `top_k` cut, and the
    /// user's preference text follows the question and context.
    #[tokio::test]
    async fn preferences_rerank_the_chunks_and_guide_the_answer() {
        let db = cognee_vector::BruteForceVectorDB::new();
        db.create_collection("DocumentChunk", "text", 2)
            .await
            .unwrap();
        let (near, liked) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let point = |id: Uuid, text: &str, vector: Vec<f32>| VectorPoint {
            id,
            vector,
            metadata: HashMap::from([
                ("id".to_string(), json!(id.to_string())),
                ("text".to_string(), json!(text)),
            ]),
        };
        db.index_points(
            "DocumentChunk",
            "text",
            &[
                // Distances from the query [0.4, 0.6]: ~0.077 and ~0.100. The
                // liked chunk's shrinks by the 0.3 influence to ~0.070, ahead
                // of the nearest (Python's distance-space re-rank).
                point(near, "the nearest chunk", vec![0.6, 0.4]),
                point(liked, "the liked chunk", vec![0.63, 0.37]),
            ],
        )
        .await
        .unwrap();

        let graph = Arc::new(cognee_graph::MockGraphDB::new());
        for id in [near, liked] {
            cognee_graph::GraphDBTrait::add_node_raw(
                graph.as_ref(),
                json!({"id": id.to_string(), "type": "DocumentChunk"}),
            )
            .await
            .unwrap();
        }
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        let (u, d) = (user.to_string(), dataset.to_string());
        crate::user_preferences::upsert_preference_node(
            graph.as_ref(),
            &u,
            &d,
            "Answer in one sentence",
            1,
            "",
        )
        .await
        .unwrap();
        crate::user_preferences::write_prefers_edges(
            graph.as_ref(),
            &u,
            &d,
            &std::collections::BTreeMap::from([(liked.to_string(), 1.0)]),
            1,
        )
        .await
        .unwrap();

        let llm = Arc::new(TestLlm {
            response_text: "answer".to_string(),
            ..Default::default()
        });
        let retriever = CompletionRetriever::new(
            Arc::new(db),
            Arc::new(TestEmbeddingEngine),
            Arc::clone(&llm) as Arc<dyn Llm>,
            Some(1),
            None,
            None,
            None,
            None,
        )
        .with_graph_db(graph);
        let text = |context: &SearchContext| context[0].payload["text"].clone();

        let plain = SearchParams {
            user_id: Some(user),
            dataset_id: Some(dataset),
            ..SearchParams::default()
        };
        let context = retriever.get_context("q", &plain).await.unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(text(&context), json!("the nearest chunk"));

        let personalized = SearchParams {
            personalization: Some(crate::user_preferences::PreferenceConfig {
                enabled: true,
                ..Default::default()
            }),
            ..plain
        };
        let context = retriever.get_context("q", &personalized).await.unwrap();
        assert_eq!(context.len(), 1, "trimmed back to top_k");
        assert_eq!(text(&context), json!("the liked chunk"));

        retriever
            .get_completion(
                "q",
                Some(context),
                &SessionContext::default(),
                &personalized,
            )
            .await
            .unwrap();
        let messages = llm.last_messages.lock().unwrap().clone();
        assert!(
            messages[1].content.ends_with(&format!(
                "\n\n{}\nAnswer in one sentence",
                crate::user_preferences::PREFERENCE_RENDER_HEADER
            )),
            "{}",
            messages[1].content
        );
    }

    // Python ranks in distance space: a liked chunk's distance shrinks. With
    // a negative similarity, scaling the similarity instead would push the
    // liked chunk down; in distance space it still moves up.
    #[test]
    fn a_liked_chunk_moves_up_even_with_a_negative_similarity() {
        let item = |id: &str, score: f32| crate::types::SearchItem {
            id: None,
            score: Some(score),
            payload: serde_json::json!({"id": id}),
        };
        let mut context = vec![item("neutral", -0.15), item("liked", -0.2)];
        let weights = HashMap::from([("liked".to_string(), 1.0)]);
        super::sort_by_personal_score(&mut context, &weights, 0.3);
        let order: Vec<_> = context
            .iter()
            .map(|i| i.payload["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(order, ["liked", "neutral"]);

        // No weights: the engine's order stands, ties included.
        let mut context = vec![item("a", 0.5), item("b", 0.5), item("c", 0.9)];
        super::sort_by_personal_score(&mut context, &HashMap::new(), 0.3);
        let order: Vec<_> = context
            .iter()
            .map(|i| i.payload["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(order, ["c", "a", "b"]);
    }
}
