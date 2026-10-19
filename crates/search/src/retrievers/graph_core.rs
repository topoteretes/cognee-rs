//! Graph-triplet retrieval shared by every graph-backed retriever.
//!
//! Python's graph retrievers (`GraphCompletionRetriever` and its subclasses:
//! summary, chain-of-thought, context extension, temporal) all inherit one
//! `get_triplets`. Here the same role is played by [`GraphRetrieverCore`]:
//! one retrieval configuration, one triplet search, and one edge → context
//! item mapping, so a payload field or a graph check is added in one place.

use std::sync::Arc;

use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, Llm, Message};
use cognee_session::SessionContext;
use cognee_vector::VectorDB;
use serde_json::json;
use tracing::debug;

use crate::graph_retrieval::{
    DEFAULT_TRIPLET_DISTANCE_PENALTY, GraphRetrievalConfig, RankedGraphEdge,
    brute_force_triplet_search,
};
use crate::types::{SearchContext, SearchError, SearchItem, SearchOutput, SearchParams};
use crate::utils::{
    EMPTY_GRAPH_MESSAGE, build_messages_with_history, render_graph_user_prompt,
    resolve_system_prompt,
};

const DEFAULT_TOP_K: usize = 10;
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;

pub(crate) struct GraphRetrieverCore {
    pub(crate) vector_db: Arc<dyn VectorDB>,
    pub(crate) embedding_engine: Arc<dyn EmbeddingEngine>,
    pub(crate) graph_db: Arc<dyn GraphDBTrait>,
    top_k: usize,
    wide_search_top_k: usize,
    triplet_distance_penalty: f32,
    feedback_influence: f32,
    /// Neighbours expanded per seed node in triplet search
    /// (`search.triplet_neighbor_cap`); `None` is uncapped.
    pub(crate) neighbor_cap: Option<usize>,
}

impl GraphRetrieverCore {
    pub(crate) fn new(
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
            feedback_influence: 0.0,
            neighbor_cap: None,
        }
    }

    /// Python's `NoDataError` on an empty graph
    /// (`graph_completion_retriever.py:139-148`, SDK-270): an empty graph is a
    /// state problem, not a query miss. Raised by the retrievers that inherit
    /// Python's `get_retrieved_objects` check — graph completion and graph
    /// summary; chain-of-thought and context extension override that method
    /// and do not raise.
    pub(crate) async fn ensure_graph_not_empty(&self) -> Result<(), SearchError> {
        if self.graph_db.is_empty().await? {
            return Err(SearchError::NotFound(EMPTY_GRAPH_MESSAGE.to_string()));
        }
        Ok(())
    }

    pub(crate) fn retrieval_config(&self, params: &SearchParams) -> GraphRetrievalConfig {
        GraphRetrievalConfig {
            top_k: params.top_k_or(self.top_k),
            wide_search_top_k: params.wide_search_top_k_or(self.wide_search_top_k),
            triplet_distance_penalty: params
                .triplet_distance_penalty_or(self.triplet_distance_penalty),
            feedback_influence: params.feedback_influence_or(self.feedback_influence),
            neighbor_cap: self.neighbor_cap,
            node_type: params.node_type.clone(),
            node_name: params.node_name.clone(),
            node_name_filter_operator: params
                .node_name_filter_operator
                .as_deref()
                .unwrap_or("OR")
                .to_string(),
        }
    }

    /// The ranked triplets for `query`, best first.
    pub(crate) async fn ranked_edges(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<Vec<RankedGraphEdge>, SearchError> {
        brute_force_triplet_search(
            query,
            self.vector_db.as_ref(),
            self.embedding_engine.as_ref(),
            self.graph_db.as_ref(),
            &self.retrieval_config(params),
        )
        .await
    }

    /// The ranked triplets for `query` as context items. An empty graph yields
    /// an empty context; callers that must raise call
    /// [`ensure_graph_not_empty`](Self::ensure_graph_not_empty) first.
    pub(crate) async fn search(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        if self.graph_db.is_empty().await? {
            return Ok(vec![]);
        }
        Ok(self
            .ranked_edges(query, params)
            .await?
            .into_iter()
            .map(ranked_edge_to_item)
            .collect())
    }
}

/// One ranked triplet as a context item — the payload every graph retriever
/// hands to `resolve_edges_to_text` and to the response.
pub(crate) fn ranked_edge_to_item(edge: RankedGraphEdge) -> SearchItem {
    SearchItem {
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
            "edge_text": edge.edge_text,
            "dataset_id": edge.dataset_id,
        }),
    }
}

/// The answer prompt every graph retriever sends: the resolved system prompt
/// and the graph user template over a rendered context, with the session
/// history (Python's `GraphCompletionRetriever._completion_kwargs` +
/// `build_completion_prompts`, which every subclass inherits).
pub(crate) struct GraphPrompts {
    pub(crate) system_prompt: Option<String>,
    pub(crate) system_prompt_path: Option<String>,
    pub(crate) user_prompt_template: Option<String>,
}

impl GraphPrompts {
    pub(crate) fn messages(
        &self,
        query: &str,
        context_text: &str,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<Vec<Message>, SearchError> {
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
            render_graph_user_prompt(self.user_prompt_template.as_deref(), query, context_text);
        debug!("LLM user prompt:\n{user_prompt}");
        Ok(build_messages_with_history(
            system_prompt,
            user_prompt,
            session,
        ))
    }
}

/// Send `messages` and wrap the answer: structured when the request carries a
/// response schema, text otherwise.
pub(crate) async fn complete(
    llm: &dyn Llm,
    messages: Vec<Message>,
    params: &SearchParams,
    generation_options: Option<GenerationOptions>,
) -> Result<SearchOutput, SearchError> {
    if let Some(schema) = &params.response_schema {
        let structured_value = llm
            .create_structured_output_with_messages_raw(messages, schema, generation_options)
            .await
            .map_err(|e| SearchError::LlmError(e.to_string()))?;
        Ok(SearchOutput::Structured(structured_value))
    } else {
        let completion = llm.generate(messages, generation_options).await?;
        Ok(SearchOutput::Text(completion.content))
    }
}
