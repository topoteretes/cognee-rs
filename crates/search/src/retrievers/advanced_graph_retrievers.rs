use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_vector::VectorDB;

use cognee_session::SessionContext;

use crate::retrievers::SearchRetriever;
use crate::retrievers::graph_core::{GraphPrompts, GraphRetrieverCore, complete};
use crate::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};
use crate::utils::{
    DEFAULT_RAG_SYSTEM_PROMPT, build_messages_with_history, render_edges_context,
    render_graph_user_prompt, resolve_system_prompt, skip_completion_on_empty_context,
};

const DEFAULT_CONTEXT_EXTENSION_ROUNDS: usize = 4;
const DEFAULT_COT_MAX_ITER: usize = 4;

const DEFAULT_GRAPH_SUMMARY_SYSTEM_PROMPT: &str = "You are a top-tier summarization engine that is meant to eliminate redundancies.\nThe input contains relationships enclosed by \\\"--\\\" .\nSummarize the input into natural sentences, listing all relationships.";
const DEFAULT_GRAPH_SUMMARY_USER_PROMPT: &str = "{context}";

const DEFAULT_COT_VALIDATION_SYSTEM_PROMPT: &str = "You are a helpful agent who are allowed to use only the provided question answer and context.\nI want to you find reasoning what is missing from the context or why the answer is not answering the question or not correct strictly based on the context.";
const DEFAULT_COT_VALIDATION_USER_PROMPT: &str = "<QUESTION>\n`{question}`\n</QUESTION>\n\n<ANSWER>\n`{answer}`\n</ANSWER>\n\n<CONTEXT>\n`{context}`\n</CONTEXT>";

const DEFAULT_COT_FOLLOW_UP_SYSTEM_PROMPT: &str = "You are a helpful assistant whose job is to ask exactly one clarifying follow-up question,\nto collect the missing piece of information needed to fully answer the user's original query.\nRespond with the question only (no extra text, no punctuation beyond what's needed).";
const DEFAULT_COT_FOLLOW_UP_USER_PROMPT: &str = "Based on the following, ask exactly one question that would directly resolve the gap identified in the validation reasoning and allow a valid answer.\nThink in a way that with the followup question you are exploring a knowledge graph which contains entities, entity types and document chunks\n\n<QUERY>\n`{question}`\n</QUERY>\n\n<ANSWER>\n`{answer}`\n</ANSWER>\n\n<REASONING>\n`{validation}`\n</REASONING>";

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
    prompts: GraphPrompts,
    generation_options: Option<GenerationOptions>,
}

impl GraphSummaryCompletionRetriever {
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

    /// The answer messages over `context`: the rendered triplets are first
    /// summarized by the LLM (Python's `get_context_from_objects`), then sent
    /// through the graph prompt. `None` on an empty context, before any LLM
    /// call.
    async fn build_completion_messages(
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

        self.prompts
            .messages(query, &summarized_context, session, params)
            .map(Some)
    }
}

#[async_trait]
impl SearchRetriever for GraphSummaryCompletionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphSummaryCompletion
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

    /// Python's `GraphSummaryCompletionRetriever` inherits
    /// `GraphCompletionRetriever.get_retrieved_objects`, so an empty graph is
    /// `NotFound` here too.
    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        self.core.ensure_graph_not_empty().await?;
        // Scoped here, not only by the orchestrator: `get_completion` calls
        // this again for follow-up retrievals the orchestrator never sees.
        Ok(crate::orchestration::filter_context_to_params(
            self.core.search(query, params).await?,
            params,
        ))
    }

    async fn completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        self.build_completion_messages(query, context, session, params)
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
            .build_completion_messages(query, &completion_context, session, params)
            .await?
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

pub struct GraphCompletionContextExtensionRetriever {
    core: GraphRetrieverCore,
    llm: Arc<dyn Llm>,
    context_extension_rounds: usize,
    prompts: GraphPrompts,
    generation_options: Option<GenerationOptions>,
}

impl GraphCompletionContextExtensionRetriever {
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
            prompts: GraphPrompts {
                system_prompt,
                system_prompt_path,
                user_prompt_template,
            },
            generation_options,
        }
    }

    /// Grow `context` round by round: answer over the current context, search
    /// with the answer, merge, and stop when a round adds nothing (Python's
    /// `_run_extension_round`).
    async fn extend_context(
        &self,
        query: &str,
        mut extended_context: SearchContext,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        let rounds = params
            .context_extension_rounds
            .unwrap_or(self.context_extension_rounds);

        for _ in 0..rounds {
            let current_context_text = render_edges_context(&extended_context);
            let extension_prompt = render_graph_user_prompt(
                self.prompts.user_prompt_template.as_deref(),
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

            let new_context = self.get_context(&completion, params).await?;
            let merged_context = merge_dedup_context(&extended_context, &new_context);

            if merged_context.len() == extended_context.len() {
                break;
            }

            extended_context = merged_context;
        }
        Ok(extended_context)
    }

    /// The answer messages over the extended `context`, or `None` when the
    /// initial context is empty — no extension round runs over nothing.
    async fn build_completion_messages(
        &self,
        query: &str,
        context: SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        if render_edges_context(&context).is_empty() {
            return Ok(None);
        }
        let extended_context = self.extend_context(query, context, params).await?;
        self.prompts
            .messages(
                query,
                &render_edges_context(&extended_context),
                session,
                params,
            )
            .map(Some)
    }
}

#[async_trait]
impl SearchRetriever for GraphCompletionContextExtensionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletionContextExtension
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

    /// Python's context-extension retriever overrides
    /// `get_retrieved_objects` without the empty-graph check, so an empty
    /// graph is an empty context here, not `NotFound`.
    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        // Scoped here, not only by the orchestrator: `get_completion` calls
        // this again for follow-up retrievals the orchestrator never sees.
        Ok(crate::orchestration::filter_context_to_params(
            self.core.search(query, params).await?,
            params,
        ))
    }

    /// The final prompt over the extended context. The extension rounds call
    /// the LLM, as they do under Python's `only_context` — they are retrieval,
    /// not the answer.
    async fn completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        self.build_completion_messages(query, context.clone(), session, params)
            .await
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let initial_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query, params).await?,
        };

        let Some(messages) = self
            .build_completion_messages(query, initial_context, session, params)
            .await?
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

pub struct GraphCompletionCotRetriever {
    core: GraphRetrieverCore,
    llm: Arc<dyn Llm>,
    max_iter: usize,
    prompts: GraphPrompts,
    generation_options: Option<GenerationOptions>,
}

/// Where a chain-of-thought run ended: the last answer and the context it
/// was reasoned over.
struct CotOutcome {
    answer: Option<String>,
    context: SearchContext,
}

impl GraphCompletionCotRetriever {
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
            prompts: GraphPrompts {
                system_prompt,
                system_prompt_path,
                user_prompt_template,
            },
            generation_options,
        }
    }

    /// Answer, validate, ask a follow-up, widen the context, answer again —
    /// `max_iter` times or until no follow-up comes back.
    ///
    /// With `final_answer = false` (the `only_context` preview) the answer
    /// after the last widening is not generated: nothing reads it, and
    /// Python's preview stops there too (`skip_final_completion=True`).
    async fn reason(
        &self,
        query: &str,
        mut current_context: SearchContext,
        session: &SessionContext,
        params: &SearchParams,
        final_answer: bool,
    ) -> Result<CotOutcome, SearchError> {
        let system_prompt = resolve_system_prompt(
            params
                .system_prompt
                .as_deref()
                .or(self.prompts.system_prompt.as_deref()),
            params
                .system_prompt_path
                .as_deref()
                .or(self.prompts.system_prompt_path.as_deref()),
        )?;
        let max_iter = params.max_iter.unwrap_or(self.max_iter);
        let answer_over = |context: &SearchContext| {
            render_graph_user_prompt(
                self.prompts.user_prompt_template.as_deref(),
                query,
                &render_edges_context(context),
            )
        };

        // Step 1: Generate INITIAL completion (before any reasoning rounds)
        let mut current_answer = self
            .llm
            .generate(
                build_messages_with_history(
                    system_prompt.clone(),
                    answer_over(&current_context),
                    session,
                ),
                self.generation_options.clone(),
            )
            .await?
            .content;

        // Step 2: Run max_iter REASONING rounds
        for iteration in 0..max_iter {
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
            let additional_context = self.get_context(&follow_up_query, params).await?;
            current_context = merge_dedup_context(&current_context, &additional_context);

            if !final_answer && iteration + 1 == max_iter {
                return Ok(CotOutcome {
                    answer: None,
                    context: current_context,
                });
            }

            // 2d. Regenerate completion with the enriched context
            current_answer = self
                .llm
                .generate(
                    build_messages_with_history(
                        system_prompt.clone(),
                        answer_over(&current_context),
                        session,
                    ),
                    self.generation_options.clone(),
                )
                .await?
                .content;
        }

        Ok(CotOutcome {
            answer: Some(current_answer),
            context: current_context,
        })
    }
}

#[async_trait]
impl SearchRetriever for GraphCompletionCotRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletionCot
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

    /// Python's chain-of-thought retriever overrides `get_retrieved_objects`
    /// without the empty-graph check, so an empty graph is an empty context
    /// here, not `NotFound`.
    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        // Scoped here, not only by the orchestrator: `get_completion` calls
        // this again for follow-up retrievals the orchestrator never sees.
        Ok(crate::orchestration::filter_context_to_params(
            self.core.search(query, params).await?,
            params,
        ))
    }

    /// The final prompt over the context the reasoning ended with. The
    /// reasoning rounds call the LLM, as they do under Python's
    /// `only_context`; the final answer is not generated.
    async fn completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        if render_edges_context(context).is_empty() {
            return Ok(None);
        }
        let outcome = self
            .reason(query, context.clone(), session, params, false)
            .await?;
        self.prompts
            .messages(
                query,
                &render_edges_context(&outcome.context),
                session,
                params,
            )
            .map(Some)
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        let initial_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query, params).await?,
        };
        if render_edges_context(&initial_context).is_empty() {
            return Ok(skip_completion_on_empty_context());
        }

        let outcome = self
            .reason(query, initial_context, session, params, true)
            .await?;

        if params.response_schema.is_some() {
            // CoT builds the answer as plain text; structured output is the
            // final prompt re-run as a structured call.
            let messages = self.prompts.messages(
                query,
                &render_edges_context(&outcome.context),
                session,
                params,
            )?;
            complete(
                self.llm.as_ref(),
                messages,
                params,
                self.generation_options.clone(),
            )
            .await
        } else {
            Ok(SearchOutput::Text(outcome.answer.unwrap_or_default()))
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
    use crate::types::{SearchError, SearchOutput, SearchParams, SearchType};

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
            .get_completion(
                "Who knows Bob?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
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
            .get_completion(
                "Who knows Bob?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
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
            .get_completion(
                "Who knows Bob?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
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
            .get_completion(
                "Who knows Bob?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
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
            .get_completion(
                "Who knows Bob?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "the answer"),
            _ => panic!("expected text output"),
        }

        // Exactly one LLM call: the initial completion (no reasoning rounds).
        assert_eq!(llm.captured_messages.lock().unwrap().len(), 1);
    }

    fn summary(graph_db: Arc<MockGraphDB>, llm: &Arc<TestLlm>) -> GraphSummaryCompletionRetriever {
        GraphSummaryCompletionRetriever::new(
            build_vector_db(),
            Arc::new(TestEmbeddingEngine),
            graph_db,
            Arc::clone(llm) as Arc<dyn Llm>,
            Some(5),
            Some(5),
            Some(0.0),
            None,
            None,
            None,
            None,
        )
    }

    fn extension(
        graph_db: Arc<MockGraphDB>,
        llm: &Arc<TestLlm>,
    ) -> GraphCompletionContextExtensionRetriever {
        GraphCompletionContextExtensionRetriever::new(
            build_vector_db(),
            Arc::new(TestEmbeddingEngine),
            graph_db,
            Arc::clone(llm) as Arc<dyn Llm>,
            Some(5),
            Some(5),
            Some(0.0),
            Some(1),
            None,
            None,
            None,
            None,
        )
    }

    fn cot(graph_db: Arc<MockGraphDB>, llm: &Arc<TestLlm>) -> GraphCompletionCotRetriever {
        GraphCompletionCotRetriever::new(
            build_vector_db(),
            Arc::new(TestEmbeddingEngine),
            graph_db,
            Arc::clone(llm) as Arc<dyn Llm>,
            Some(5),
            Some(5),
            Some(0.0),
            Some(1),
            None,
            None,
            None,
            None,
        )
    }

    /// GRAPH_SUMMARY_COMPLETION inherits Python's empty-graph `NoDataError`.
    #[tokio::test]
    async fn graph_summary_on_an_empty_graph_is_not_found() {
        let llm = Arc::new(TestLlm::new(vec![]));
        let retriever = summary(Arc::new(MockGraphDB::new()), &llm);

        let err = retriever
            .get_completion(
                "q",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::NotFound(_)), "got {err:?}");
        assert!(llm.captured_messages.lock().unwrap().is_empty());
    }

    /// CoT and context extension skip Python's empty-graph check, but an empty
    /// context still never reaches the LLM: no reasoning or extension round
    /// runs over nothing, and the answer is skipped.
    #[tokio::test]
    async fn cot_and_extension_skip_the_llm_on_an_empty_graph() {
        let llm = Arc::new(TestLlm::new(vec![]));
        let params = SearchParams::default();
        let session = SessionContext::default();

        let extension = extension(Arc::new(MockGraphDB::new()), &llm);
        assert!(
            extension
                .get_context("q", &params)
                .await
                .unwrap()
                .is_empty()
        );
        let output = extension
            .get_completion("q", None, &session, &params)
            .await
            .unwrap();
        assert!(matches!(output, SearchOutput::Texts(ref texts) if texts.is_empty()));

        let cot = cot(Arc::new(MockGraphDB::new()), &llm);
        assert!(cot.get_context("q", &params).await.unwrap().is_empty());
        let output = cot
            .get_completion("q", None, &session, &params)
            .await
            .unwrap();
        assert!(matches!(output, SearchOutput::Texts(ref texts) if texts.is_empty()));

        assert!(
            llm.captured_messages.lock().unwrap().is_empty(),
            "no LLM call"
        );
    }

    /// The summary preview runs the summarization (a context-stage LLM call,
    /// as under Python's `only_context`) and returns exactly the messages the
    /// completion then sends.
    #[tokio::test]
    async fn graph_summary_preview_matches_the_completion_prompt() {
        let params = SearchParams::default();
        let session = SessionContext::default();

        let preview_llm = Arc::new(TestLlm::new(vec!["the summary"]));
        let retriever = summary(build_graph_db().await, &preview_llm);
        let context = retriever
            .get_context("Who knows Bob?", &params)
            .await
            .unwrap();
        let preview = retriever
            .completion_messages("Who knows Bob?", &context, &session, &params)
            .await
            .unwrap()
            .expect("a non-empty context builds a prompt");
        assert_eq!(
            preview_llm.captured_messages.lock().unwrap().len(),
            1,
            "summary only"
        );

        let llm = Arc::new(TestLlm::new(vec!["the summary", "answer"]));
        summary(build_graph_db().await, &llm)
            .get_completion("Who knows Bob?", None, &session, &params)
            .await
            .unwrap();
        let sent = llm.captured_messages.lock().unwrap()[1].clone();
        assert_eq!(sent[0].content, preview[0].content);
        assert_eq!(sent[1].content, preview[1].content);
        assert!(preview[1].content.contains("the summary"));
    }

    /// The CoT preview reasons as the completion does but stops before the
    /// final answer, and its prompt is the one the final answer is built from.
    #[tokio::test]
    async fn graph_cot_preview_skips_the_final_answer() {
        let llm = Arc::new(TestLlm::new(vec![
            "first answer",
            "needs more evidence",
            "find graph neighbors",
        ]));
        let retriever = cot(build_graph_db().await, &llm);
        let params = SearchParams::default();
        let context = retriever
            .get_context("Who knows Bob?", &params)
            .await
            .unwrap();

        let preview = retriever
            .completion_messages(
                "Who knows Bob?",
                &context,
                &SessionContext::default(),
                &params,
            )
            .await
            .unwrap()
            .expect("a non-empty context builds a prompt");

        // Initial answer, validation, follow-up — no regeneration.
        assert_eq!(llm.captured_messages.lock().unwrap().len(), 3);
        assert!(preview[1].content.contains("Who knows Bob?"));
        assert!(preview[1].content.contains("--[KNOWS]-->"));
    }

    /// The context-extension preview runs the extension rounds and returns
    /// the prompt over the extended context.
    #[tokio::test]
    async fn graph_extension_preview_runs_the_rounds() {
        let llm = Arc::new(TestLlm::new(vec!["Find Bob relations"]));
        let retriever = extension(build_graph_db().await, &llm);
        let params = SearchParams::default();
        let context = retriever
            .get_context("Who knows Bob?", &params)
            .await
            .unwrap();

        let preview = retriever
            .completion_messages(
                "Who knows Bob?",
                &context,
                &SessionContext::default(),
                &params,
            )
            .await
            .unwrap()
            .expect("a non-empty context builds a prompt");

        assert_eq!(
            llm.captured_messages.lock().unwrap().len(),
            1,
            "one extension round"
        );
        assert!(preview[1].content.contains("--[KNOWS]-->"));
    }
}
