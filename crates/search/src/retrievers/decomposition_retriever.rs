//! `GRAPH_COMPLETION_DECOMPOSITION`: graph completion over a question split
//! into focused sub-questions.
//!
//! Port of Python's `GraphCompletionDecompositionRetriever`
//! (`graph_completion_decomposition_retriever.py`) and its helpers
//! (`utils/query_decomposition.py`). One LLM call splits the question into up
//! to five sub-questions, one batched triplet search fetches each
//! sub-question's triplets over one graph load, and the batches are merged
//! without duplicates. The answer is then built in one of two modes:
//!
//! * `answer_per_subquery` (the default): each sub-question is answered over
//!   its own triplets, concurrently, and the final answer is generated over
//!   those sub-answers.
//! * `combined_triplets_context`: the final answer is generated once over the
//!   merged triplets.
//!
//! Python keeps the sub-questions and their per-sub-question triplets on the
//! retriever instance between `get_retrieved_objects` and the completion. A
//! retriever here is shared by every search, so that state travels in the
//! context instead: each merged item carries a `decomposition` payload entry
//! with the sub-questions and the rank the triplet had in each sub-question's
//! results ([`DECOMPOSITION_PAYLOAD_KEY`]).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm, LlmExt, Message};
use cognee_session::SessionContext;
use cognee_vector::VectorDB;
use futures::future::try_join_all;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::warn;

use crate::graph_retrieval::RankedGraphEdge;
use crate::retrievers::SearchRetriever;
use crate::retrievers::graph_core::{
    GraphPrompts, GraphRetrieverCore, complete, ranked_edge_to_item,
};
use crate::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};
use crate::utils::{render_edges_context, skip_completion_on_empty_context};

/// System prompt for splitting a question into sub-questions.
///
/// Vendored byte-for-byte from Python's
/// `cognee/infrastructure/llm/prompts/graph_completion_decomposition_system_prompt.txt`.
const DECOMPOSITION_SYSTEM_PROMPT: &str =
    include_str!("../utils/prompts/graph_completion_decomposition_system_prompt.txt");

/// At most this many sub-questions are kept (`normalize_subqueries`).
const MAX_SUBQUERIES: usize = 5;

/// The payload entry that carries the decomposition on every context item:
/// `{"subqueries": [..], "positions": [[subquery_index, rank], ..]}`.
pub const DECOMPOSITION_PAYLOAD_KEY: &str = "decomposition";

/// Python's `DecompositionMode`: how the sub-questions feed the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecompositionMode {
    /// Answer each sub-question over its own triplets, then answer the
    /// question over those answers.
    #[default]
    AnswerPerSubquery,
    /// Answer the question once over the merged triplets.
    CombinedTripletsContext,
}

impl DecompositionMode {
    /// Parse Python's wire value (`retriever_specific_config.decomposition_mode`).
    /// `None` is the default mode; an unknown value is rejected, as Python's
    /// `DecompositionMode(value)` raises.
    ///
    /// # Errors
    /// [`SearchError::InvalidInput`] for a value that is not a mode.
    pub fn parse(value: Option<&str>) -> Result<Self, SearchError> {
        match value {
            None | Some("answer_per_subquery") => Ok(Self::AnswerPerSubquery),
            Some("combined_triplets_context") => Ok(Self::CombinedTripletsContext),
            Some(other) => Err(SearchError::InvalidInput(format!(
                "'{other}' is not a valid decomposition_mode; expected \
                 'answer_per_subquery' or 'combined_triplets_context'"
            ))),
        }
    }
}

/// Python's `QueryDecomposition`: the structured decomposition output.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct QueryDecomposition {
    subqueries: Vec<String>,
}

/// Python's `normalize_subqueries`: trim, drop blanks and duplicates, keep at
/// most [`MAX_SUBQUERIES`], and fall back to the original question when
/// nothing is left.
pub(crate) fn normalize_subqueries(original_query: &str, subqueries: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for subquery in subqueries {
        let cleaned = subquery.trim();
        if cleaned.is_empty() || normalized.iter().any(|seen| seen == cleaned) {
            continue;
        }
        normalized.push(cleaned.to_string());
        if normalized.len() >= MAX_SUBQUERIES {
            break;
        }
    }

    if normalized.is_empty() {
        let fallback = original_query.trim();
        normalized.push(if fallback.is_empty() {
            original_query.to_string()
        } else {
            fallback.to_string()
        });
    }
    normalized
}

/// Python's `merge_deduplicated_edges`, as context items: the batches merged
/// in order, each triplet once. Python deduplicates by object identity, which
/// is triplet identity because every sub-question ranks the same graph
/// projection; here the identity is the triplet itself (source, target,
/// relationship). Each item records the sub-questions and its rank in every
/// sub-question's results that returned it.
fn merge_deduplicated_edges(
    subqueries: &[String],
    batches: Vec<Vec<RankedGraphEdge>>,
) -> SearchContext {
    let mut merged: Vec<(RankedGraphEdge, Vec<[usize; 2]>)> = Vec::new();
    let mut index_of: HashMap<(String, String, String), usize> = HashMap::new();

    for (subquery_index, batch) in batches.into_iter().enumerate() {
        for (rank, edge) in batch.into_iter().enumerate() {
            let key = (
                edge.source_id.clone(),
                edge.target_id.clone(),
                edge.relationship_name.clone(),
            );
            match index_of.get(&key) {
                // A triplet repeated within one sub-question's results keeps
                // its best rank only, so it is rendered once for that answer.
                Some(&at) => {
                    let positions = &mut merged[at].1;
                    if !positions.iter().any(|[idx, _]| *idx == subquery_index) {
                        positions.push([subquery_index, rank]);
                    }
                }
                None => {
                    index_of.insert(key, merged.len());
                    merged.push((edge, vec![[subquery_index, rank]]));
                }
            }
        }
    }

    merged
        .into_iter()
        .map(|(edge, positions)| {
            let mut item = ranked_edge_to_item(edge);
            if let Value::Object(payload) = &mut item.payload {
                payload.insert(
                    DECOMPOSITION_PAYLOAD_KEY.to_string(),
                    json!({ "subqueries": subqueries, "positions": positions }),
                );
            }
            item
        })
        .collect()
}

/// The sub-questions and each one's triplets in its own rank order, read back
/// from a context built by [`merge_deduplicated_edges`].
///
/// A context without the decomposition entry (one a caller built) is treated
/// as a single sub-question, the question itself, over every item.
fn split_by_subquery(query: &str, context: &SearchContext) -> (Vec<String>, Vec<SearchContext>) {
    let subqueries: Option<Vec<String>> = context.iter().find_map(|item| {
        item.payload
            .get(DECOMPOSITION_PAYLOAD_KEY)?
            .get("subqueries")?
            .as_array()?
            .iter()
            .map(|value| value.as_str().map(str::to_string))
            .collect()
    });
    let Some(subqueries) = subqueries.filter(|subqueries| !subqueries.is_empty()) else {
        return (vec![query.to_string()], vec![context.clone()]);
    };

    let mut ranked: Vec<Vec<(u64, usize)>> = vec![Vec::new(); subqueries.len()];
    for (item_index, item) in context.iter().enumerate() {
        let positions = item
            .payload
            .get(DECOMPOSITION_PAYLOAD_KEY)
            .and_then(|decomposition| decomposition.get("positions"))
            .and_then(Value::as_array);
        for position in positions.into_iter().flatten() {
            let (Some(subquery_index), Some(rank)) = (
                position.get(0).and_then(Value::as_u64),
                position.get(1).and_then(Value::as_u64),
            ) else {
                continue;
            };
            if let Some(slot) = usize::try_from(subquery_index)
                .ok()
                .and_then(|index| ranked.get_mut(index))
            {
                slot.push((rank, item_index));
            }
        }
    }

    let per_subquery = ranked
        .into_iter()
        .map(|mut items| {
            items.sort_by_key(|(rank, _)| *rank);
            items
                .into_iter()
                .map(|(_, item_index)| context[item_index].clone())
                .collect()
        })
        .collect();
    (subqueries, per_subquery)
}

/// Python's `build_subquery_answer_context`: the sub-answers, in sub-question
/// order, as the context of the final answer.
pub(crate) fn build_subquery_answer_context(subqueries: &[String], answers: &[String]) -> String {
    let sections: Vec<String> = subqueries
        .iter()
        .zip(answers)
        .enumerate()
        .map(|(index, (subquery, answer))| {
            let number = index + 1;
            format!("Subquery {number}: {subquery}\nSubquery {number} Answer:\n{answer}")
        })
        .collect();
    format!(
        "Question decomposition results:\n\n{}",
        sections.join("\n\n")
    )
}

pub struct GraphCompletionDecompositionRetriever {
    core: GraphRetrieverCore,
    llm: Arc<dyn Llm>,
    prompts: GraphPrompts,
    generation_options: Option<GenerationOptions>,
}

impl GraphCompletionDecompositionRetriever {
    /// Cap the neighbours expanded per seed node in triplet search
    /// (`search.triplet_neighbor_cap`); `None` is uncapped. Applies when the
    /// question decomposes into a single sub-question, which runs the
    /// single-query search; the batch search loads the whole graph.
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

    /// Python's `_decompose_query`: the normalized sub-questions, or the
    /// question alone when the LLM call fails.
    async fn decompose(&self, query: &str) -> Vec<String> {
        match self
            .llm
            .create_structured_output::<QueryDecomposition>(
                query,
                DECOMPOSITION_SYSTEM_PROMPT,
                None,
            )
            .await
        {
            Ok(decomposition) => normalize_subqueries(query, &decomposition.subqueries),
            Err(error) => {
                warn!(%error, "Query decomposition failed, falling back to original query");
                vec![query.to_string()]
            }
        }
    }

    /// Python's `_resolve_subquery_context_and_answer`: one sub-question
    /// answered over its own triplets, without the session history.
    async fn answer_subquery(
        &self,
        subquery: &str,
        context: &SearchContext,
        params: &SearchParams,
    ) -> Result<String, SearchError> {
        let messages = self.prompts.messages(
            subquery,
            &render_edges_context(context),
            &SessionContext::default(),
            params,
        )?;
        Ok(self
            .llm
            .generate(messages, self.generation_options.clone())
            .await?
            .content)
    }

    /// Python's `get_context_from_objects`: the context the final answer is
    /// generated over. In `answer_per_subquery` mode this answers every
    /// sub-question first.
    async fn final_context(
        &self,
        query: &str,
        context: &SearchContext,
        combined_context: String,
        params: &SearchParams,
    ) -> Result<String, SearchError> {
        match DecompositionMode::parse(params.decomposition_mode.as_deref())? {
            DecompositionMode::CombinedTripletsContext => Ok(combined_context),
            DecompositionMode::AnswerPerSubquery => {
                let (subqueries, per_subquery) = split_by_subquery(query, context);
                let answers = try_join_all(
                    subqueries
                        .iter()
                        .zip(&per_subquery)
                        .map(|(subquery, items)| self.answer_subquery(subquery, items, params)),
                )
                .await?;
                Ok(build_subquery_answer_context(&subqueries, &answers))
            }
        }
    }

    /// The final answer's messages, or `None` when no triplet was retrieved
    /// (the completion is skipped). Shared by `get_completion` and the
    /// `only_context` preview.
    async fn build_completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        let combined_context = render_edges_context(context);
        if combined_context.is_empty() {
            return Ok(None);
        }
        let final_context = self
            .final_context(query, context, combined_context, params)
            .await?;
        self.prompts
            .messages(query, &final_context, session, params)
            .map(Some)
    }

    fn reject_batch() -> SearchError {
        SearchError::InvalidInput(
            "GRAPH_COMPLETION_DECOMPOSITION accepts only a single query. \
             Decomposition batching is handled internally."
                .to_string(),
        )
    }
}

#[async_trait]
impl SearchRetriever for GraphCompletionDecompositionRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletionDecomposition
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

    /// Python's `get_retrieved_objects`: decompose, fetch every sub-question's
    /// triplets in one batch, merge them. An empty graph is `NotFound`.
    #[tracing::instrument(
        name = "cognee.retrieval.get_context",
        skip(self, params),
        fields(cognee.retrieval.retriever = "GraphCompletionDecompositionRetriever")
    )]
    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        if query.trim().is_empty() {
            return Err(SearchError::InvalidInput(
                "A non-empty query is required.".to_string(),
            ));
        }
        // Python validates the mode when the retriever is built, before any
        // engine or LLM call.
        DecompositionMode::parse(params.decomposition_mode.as_deref())?;
        self.core.ensure_graph_not_empty().await?;

        let subqueries = self.decompose(query).await;
        let queries: Vec<&str> = subqueries.iter().map(String::as_str).collect();
        let batches = self.core.ranked_edges_batch(&queries, params).await?;
        let merged = merge_deduplicated_edges(&subqueries, batches);
        if merged.is_empty() {
            warn!("Empty context was provided to the completion");
        }
        Ok(merged)
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
        let context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query, params).await?,
        };
        let Some(messages) = self
            .build_completion_messages(query, &context, session, params)
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

    /// Python rejects `query_batch`: the retriever batches its own
    /// sub-questions.
    async fn get_context_batch(
        &self,
        _queries: &[String],
        _params: &SearchParams,
    ) -> Result<Vec<SearchContext>, SearchError> {
        Err(Self::reject_batch())
    }

    async fn get_completion_batch(
        &self,
        _queries: &[String],
        _contexts: Option<Vec<SearchContext>>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<Vec<SearchOutput>, SearchError> {
        Err(Self::reject_batch())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use cognee_embedding::EmbeddingResult;
    use cognee_graph::MockGraphDB;
    use cognee_llm::{GenerationResponse, LlmError, LlmResult};
    use cognee_vector::{MockVectorDB, VectorPoint};
    use uuid::Uuid;

    use super::*;

    struct AlignedEmbedding;

    #[async_trait]
    impl EmbeddingEngine for AlignedEmbedding {
        async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
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

    /// Answers the decomposition call with `subqueries` (an error when
    /// `None`) and every free-text call with `answer N`, recording both.
    struct ScriptedLlm {
        subqueries: Option<Vec<&'static str>>,
        structured_calls: Mutex<Vec<Vec<Message>>>,
        generate_calls: Mutex<Vec<Vec<Message>>>,
        answers: Mutex<VecDeque<String>>,
    }

    impl ScriptedLlm {
        fn new(subqueries: Option<Vec<&'static str>>) -> Arc<Self> {
            Arc::new(Self {
                subqueries,
                structured_calls: Mutex::new(Vec::new()),
                generate_calls: Mutex::new(Vec::new()),
                answers: Mutex::new((1..=10).map(|n| format!("answer {n}")).collect()),
            })
        }

        fn user_prompts(&self) -> Vec<String> {
            self.generate_calls
                .lock()
                .unwrap()
                .iter()
                .map(|messages| messages.last().unwrap().content.clone())
                .collect()
        }
    }

    #[async_trait]
    impl Llm for ScriptedLlm {
        async fn generate(
            &self,
            messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            self.generate_calls.lock().unwrap().push(messages);
            Ok(GenerationResponse {
                content: self.answers.lock().unwrap().pop_front().unwrap(),
                model: "test-model".to_string(),
                usage: None,
                finish_reason: Some("stop".to_string()),
            })
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            self.structured_calls.lock().unwrap().push(messages);
            match &self.subqueries {
                Some(subqueries) => Ok(json!({ "subqueries": subqueries })),
                None => Err(LlmError::ConfigError("offline".to_string())),
            }
        }

        fn model(&self) -> &str {
            "test-model"
        }
    }

    /// Alice -knows-> Bob -likes-> Carol, every entity a vector hit.
    async fn graph() -> (Arc<MockVectorDB>, Arc<MockGraphDB>) {
        let ids: Vec<Uuid> = (1..=3).map(Uuid::from_u128).collect();
        let vector_db = MockVectorDB::new();
        vector_db
            .create_collection("Entity", "name", 2)
            .await
            .unwrap();
        let points: Vec<VectorPoint> = ids
            .iter()
            .map(|id| VectorPoint::new(*id, vec![1.0, 0.0]))
            .collect();
        vector_db
            .index_points("Entity", "name", &points)
            .await
            .unwrap();

        let graph = Arc::new(MockGraphDB::new());
        for (id, name) in ids.iter().zip(["Alice", "Bob", "Carol"]) {
            graph
                .add_node_raw(json!({"id": id.to_string(), "name": name, "type": "Entity"}))
                .await
                .unwrap();
        }
        graph
            .add_edge(&ids[0].to_string(), &ids[1].to_string(), "knows", None)
            .await
            .unwrap();
        graph
            .add_edge(&ids[1].to_string(), &ids[2].to_string(), "likes", None)
            .await
            .unwrap();
        (Arc::new(vector_db), graph)
    }

    fn retriever(
        vector_db: Arc<MockVectorDB>,
        graph: Arc<MockGraphDB>,
        llm: Arc<ScriptedLlm>,
    ) -> GraphCompletionDecompositionRetriever {
        GraphCompletionDecompositionRetriever::new(
            vector_db,
            Arc::new(AlignedEmbedding),
            graph,
            llm,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    fn mode(value: &str) -> SearchParams {
        SearchParams {
            decomposition_mode: Some(value.to_string()),
            ..SearchParams::default()
        }
    }

    #[test]
    fn subqueries_are_trimmed_deduplicated_and_capped() {
        let raw: Vec<String> = [" a ", "a", "", "  ", "b", "c", "d", "e", "f"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(normalize_subqueries("q", &raw), ["a", "b", "c", "d", "e"]);
        assert_eq!(normalize_subqueries(" q ", &[" ".to_string()]), ["q"]);
        assert_eq!(normalize_subqueries("  ", &[]), ["  "]);
    }

    #[test]
    fn subquery_answers_render_as_python_does() {
        let subqueries = ["Who?".to_string(), "Where?".to_string()];
        let answers = ["Alice".to_string(), "Home".to_string()];
        assert_eq!(
            build_subquery_answer_context(&subqueries, &answers),
            "Question decomposition results:\n\nSubquery 1: Who?\nSubquery 1 Answer:\nAlice\
             \n\nSubquery 2: Where?\nSubquery 2 Answer:\nHome"
        );
    }

    #[test]
    fn modes_parse_from_python_values() {
        assert_eq!(
            DecompositionMode::parse(None).unwrap(),
            DecompositionMode::AnswerPerSubquery
        );
        assert_eq!(
            DecompositionMode::parse(Some("combined_triplets_context")).unwrap(),
            DecompositionMode::CombinedTripletsContext
        );
        assert!(matches!(
            DecompositionMode::parse(Some("other")),
            Err(SearchError::InvalidInput(_))
        ));
    }

    fn ranked(source: &str, target: &str, relationship: &str) -> RankedGraphEdge {
        RankedGraphEdge {
            source_id: source.to_string(),
            target_id: target.to_string(),
            relationship_name: relationship.to_string(),
            score: 0.0,
            source_name: source.to_string(),
            target_name: target.to_string(),
            dataset_id: None,
            source_text: None,
            target_text: None,
            source_description: None,
            target_description: None,
            edge_text: None,
        }
    }

    /// Merged once in first-seen order; each sub-question gets its own
    /// triplets back in its own rank order, an empty one none.
    #[test]
    fn merge_deduplicates_and_split_restores_each_subquery() {
        let subqueries: Vec<String> = ["one", "two", "three"].map(String::from).to_vec();
        let merged = merge_deduplicated_edges(
            &subqueries,
            vec![
                vec![ranked("a", "b", "r"), ranked("b", "c", "r")],
                vec![
                    ranked("b", "c", "r"),
                    ranked("a", "b", "r"),
                    ranked("a", "b", "s"),
                ],
                vec![],
            ],
        );
        let key = |item: &crate::types::SearchItem| {
            format!(
                "{}-{}-{}",
                item.payload["source_id"].as_str().unwrap(),
                item.payload["relationship"].as_str().unwrap(),
                item.payload["target_id"].as_str().unwrap()
            )
        };
        assert_eq!(
            merged.iter().map(key).collect::<Vec<_>>(),
            ["a-r-b", "b-r-c", "a-s-b"]
        );
        assert_eq!(
            merged[0].payload[DECOMPOSITION_PAYLOAD_KEY],
            json!({"subqueries": ["one", "two", "three"], "positions": [[0, 0], [1, 1]]})
        );

        let (restored, per_subquery) = split_by_subquery("q", &merged);
        assert_eq!(restored, subqueries);
        let keys: Vec<Vec<String>> = per_subquery
            .iter()
            .map(|items| items.iter().map(key).collect())
            .collect();
        assert_eq!(
            keys,
            vec![
                vec!["a-r-b", "b-r-c"],
                vec!["b-r-c", "a-r-b", "a-s-b"],
                vec![],
            ]
        );
    }

    /// A triplet repeated within one sub-question's results keeps its best
    /// rank and is rendered once for that sub-question.
    #[test]
    fn a_triplet_repeated_in_one_subquery_is_kept_once() {
        let subqueries: Vec<String> = ["one"].map(String::from).to_vec();
        let merged = merge_deduplicated_edges(
            &subqueries,
            vec![vec![
                ranked("a", "b", "r"),
                ranked("b", "c", "r"),
                ranked("a", "b", "r"),
            ]],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged[0].payload[DECOMPOSITION_PAYLOAD_KEY]["positions"],
            json!([[0, 0]])
        );
        let (_, per_subquery) = split_by_subquery("q", &merged);
        assert_eq!(per_subquery[0].len(), 2);
    }

    /// A context the retriever did not build is one sub-question: the query.
    #[test]
    fn a_foreign_context_is_the_query_over_every_item() {
        let context = vec![ranked_edge_to_item(ranked("a", "b", "r"))];
        let (subqueries, per_subquery) = split_by_subquery("q", &context);
        assert_eq!(subqueries, ["q"]);
        assert_eq!(per_subquery.len(), 1);
        assert_eq!(per_subquery[0][0].payload, context[0].payload);
    }

    #[tokio::test]
    async fn answers_each_subquery_then_the_question() {
        let (vector_db, graph) = graph().await;
        let llm = ScriptedLlm::new(Some(vec![
            " Who is Alice? ",
            "Who is Alice?",
            "Whom does Bob like?",
        ]));
        let retriever = retriever(vector_db, Arc::clone(&graph), Arc::clone(&llm));
        assert_eq!(
            retriever.search_type(),
            SearchType::GraphCompletionDecomposition
        );
        let params = SearchParams::default();

        let context = retriever
            .get_context("Tell me about Alice and Bob", &params)
            .await
            .unwrap();
        // Both sub-questions return both triplets from one graph load.
        assert_eq!(context.len(), 2, "{context:?}");
        let calls = graph.get_call_log();
        assert_eq!(
            calls
                .iter()
                .filter(|call| *call == "get_graph_data")
                .count(),
            1,
            "{calls:?}"
        );
        {
            let structured = llm.structured_calls.lock().unwrap();
            assert_eq!(structured.len(), 1);
            assert_eq!(structured[0][0].content, DECOMPOSITION_SYSTEM_PROMPT);
            assert_eq!(structured[0][1].content, "Tell me about Alice and Bob");
        }

        let output = retriever
            .get_completion(
                "Tell me about Alice and Bob",
                Some(context),
                &SessionContext::default(),
                &params,
            )
            .await
            .unwrap();
        assert!(
            matches!(output, SearchOutput::Text(ref text) if text == "answer 3"),
            "{output:?}"
        );

        let prompts = llm.user_prompts();
        assert_eq!(prompts.len(), 3, "{prompts:?}");
        assert!(
            prompts[0].starts_with("The question is: `Who is Alice?`"),
            "{}",
            prompts[0]
        );
        assert!(prompts[0].contains("Alice"), "{}", prompts[0]);
        assert!(
            prompts[1].starts_with("The question is: `Whom does Bob like?`"),
            "{}",
            prompts[1]
        );
        assert!(
            prompts[2].contains(
                "Question decomposition results:\n\nSubquery 1: Who is Alice?\nSubquery 1 \
                 Answer:\nanswer 1\n\nSubquery 2: Whom does Bob like?\nSubquery 2 Answer:\n\
                 answer 2"
            ),
            "{}",
            prompts[2]
        );
        assert!(
            prompts[2].starts_with("The question is: `Tell me about Alice and Bob`"),
            "{}",
            prompts[2]
        );
    }

    #[tokio::test]
    async fn combined_mode_answers_once_over_the_merged_triplets() {
        let (vector_db, graph) = graph().await;
        let llm = ScriptedLlm::new(Some(vec!["Who is Alice?", "Whom does Bob like?"]));
        let retriever = retriever(vector_db, graph, Arc::clone(&llm));
        let params = mode("combined_triplets_context");

        let output = retriever
            .get_completion("q", None, &SessionContext::default(), &params)
            .await
            .unwrap();
        assert!(matches!(output, SearchOutput::Text(ref text) if text == "answer 1"));
        let prompts = llm.user_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(
            prompts[0].contains("knows") && prompts[0].contains("likes"),
            "{}",
            prompts[0]
        );
        assert!(!prompts[0].contains("Question decomposition results"));
    }

    /// The `only_context` preview runs the sub-answers, as Python's
    /// `get_context_from_objects` does, but not the final answer.
    #[tokio::test]
    async fn the_preview_answers_subqueries_but_not_the_question() {
        let (vector_db, graph) = graph().await;
        let llm = ScriptedLlm::new(Some(vec!["Who is Alice?", "Whom does Bob like?"]));
        let retriever = retriever(vector_db, graph, Arc::clone(&llm));
        let params = SearchParams::default();
        let context = retriever.get_context("q", &params).await.unwrap();

        let messages = retriever
            .completion_messages("q", &context, &SessionContext::default(), &params)
            .await
            .unwrap()
            .unwrap();
        assert!(
            messages
                .last()
                .unwrap()
                .content
                .contains("Subquery 2 Answer:\nanswer 2")
        );
        assert_eq!(llm.user_prompts().len(), 2);
    }

    /// A failed decomposition is the question alone, which takes the
    /// single-query search.
    #[tokio::test]
    async fn a_failed_decomposition_falls_back_to_the_query() {
        let (vector_db, graph) = graph().await;
        let llm = ScriptedLlm::new(None);
        let retriever = retriever(vector_db, Arc::clone(&graph), llm);

        let context = retriever
            .get_context("Who is Alice?", &SearchParams::default())
            .await
            .unwrap();
        assert!(!context.is_empty());
        assert_eq!(
            context[0].payload[DECOMPOSITION_PAYLOAD_KEY]["subqueries"],
            json!(["Who is Alice?"])
        );
        assert!(
            !graph
                .get_call_log()
                .iter()
                .any(|call| call == "get_graph_data")
        );
    }

    #[tokio::test]
    async fn an_empty_graph_is_not_found_before_any_llm_call() {
        let llm = ScriptedLlm::new(Some(vec!["a"]));
        let retriever = retriever(
            Arc::new(MockVectorDB::new()),
            Arc::new(MockGraphDB::new()),
            Arc::clone(&llm),
        );
        let result = retriever.get_context("q", &SearchParams::default()).await;
        assert!(
            matches!(result, Err(SearchError::NotFound(_))),
            "{result:?}"
        );
        assert!(llm.structured_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_input_is_rejected_before_any_llm_call() {
        let (vector_db, graph) = graph().await;
        let llm = ScriptedLlm::new(Some(vec!["a"]));
        let retriever = retriever(vector_db, graph, Arc::clone(&llm));

        for (query, params) in [("q", mode("bogus")), ("  ", SearchParams::default())] {
            let result = retriever.get_context(query, &params).await;
            assert!(
                matches!(result, Err(SearchError::InvalidInput(_))),
                "{result:?}"
            );
        }
        let batch = retriever
            .get_context_batch(
                &["a".to_string(), "b".to_string()],
                &SearchParams::default(),
            )
            .await;
        assert!(matches!(batch, Err(SearchError::InvalidInput(_))));
        assert!(llm.structured_calls.lock().unwrap().is_empty());
    }

    /// No triplet retrieved: the completion is skipped, as Python's
    /// `skip_completion_on_empty_context`.
    #[tokio::test]
    async fn no_triplets_skip_the_completion() {
        let (_, graph) = graph().await;
        let llm = ScriptedLlm::new(Some(vec!["a", "b"]));
        let retriever = retriever(Arc::new(MockVectorDB::new()), graph, Arc::clone(&llm));
        let output = retriever
            .get_completion(
                "q",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();
        assert!(matches!(output, SearchOutput::Texts(ref texts) if texts.is_empty()));
        assert!(llm.user_prompts().is_empty());
    }
}
