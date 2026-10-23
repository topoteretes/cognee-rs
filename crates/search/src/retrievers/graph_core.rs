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
    brute_force_triplet_search, brute_force_triplet_search_batch, validate_query_batch,
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
            neighborhood_depth: params.neighborhood_depth,
            neighborhood_seed_top_k: params.neighborhood_seed_top_k,
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

    /// The ranked triplets for each of `queries`, best first, in query order —
    /// Python's `get_triplets_batch`. A single query runs the single-query
    /// search (and its seed-scoped graph load); several share one batch search
    /// over one full graph load ([`brute_force_triplet_search_batch`]), as in
    /// Python, where the two modes rank differently.
    pub(crate) async fn ranked_edges_batch(
        &self,
        queries: &[&str],
        params: &SearchParams,
    ) -> Result<Vec<Vec<RankedGraphEdge>>, SearchError> {
        if let [query] = queries {
            return Ok(vec![self.ranked_edges(query, params).await?]);
        }
        brute_force_triplet_search_batch(
            queries,
            self.vector_db.as_ref(),
            self.embedding_engine.as_ref(),
            self.graph_db.as_ref(),
            &self.retrieval_config(params),
        )
        .await
    }

    /// The ranked triplets for each of `queries` as context items, through
    /// [`ranked_edges_batch`](Self::ranked_edges_batch): one context per
    /// query, in query order. An empty graph yields empty contexts; callers
    /// that must raise call
    /// [`ensure_graph_not_empty`](Self::ensure_graph_not_empty) first.
    ///
    /// A batch of several queries is validated before anything else, so an
    /// empty or blank-query batch is rejected whether or not the graph holds
    /// data (Python's `validate_queries` runs before any engine call). A lone
    /// query is the single-query search and keeps its own handling.
    pub(crate) async fn search_batch(
        &self,
        queries: &[&str],
        params: &SearchParams,
    ) -> Result<Vec<SearchContext>, SearchError> {
        if queries.len() != 1 {
            validate_query_batch(queries)?;
        }
        if self.graph_db.is_empty().await? {
            return Ok(vec![Vec::new(); queries.len()]);
        }
        Ok(self
            .ranked_edges_batch(queries, params)
            .await?
            .into_iter()
            .map(|edges| edges.into_iter().map(ranked_edge_to_item).collect())
            .collect())
    }

    /// The ranked triplets for `query` as context items: a batch of one, so
    /// the empty-graph policy and the item mapping live only in
    /// [`search_batch`](Self::search_batch).
    pub(crate) async fn search(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        let mut contexts = self.search_batch(&[query], params).await?;
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let context = contexts
            .pop()
            .expect("search_batch returns one context per query, and this passes one query");
        Ok(context)
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_graph::MockGraphDB;
    use cognee_vector::{MockVectorDB, VectorPoint};

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

    async fn core() -> (GraphRetrieverCore, Arc<MockGraphDB>) {
        let seed = uuid::Uuid::from_u128(1);
        let other = uuid::Uuid::from_u128(2);
        let vector_db = MockVectorDB::new();
        vector_db
            .create_collection("Entity", "name", 2)
            .await
            .unwrap();
        vector_db
            .index_points("Entity", "name", &[VectorPoint::new(seed, vec![1.0, 0.0])])
            .await
            .unwrap();
        let graph = Arc::new(MockGraphDB::new());
        for id in [seed, other] {
            graph
                .add_node_raw(json!({ "id": id.to_string(), "name": id.to_string() }))
                .await
                .unwrap();
        }
        graph
            .add_edge(&seed.to_string(), &other.to_string(), "rel", None)
            .await
            .unwrap();
        let core = GraphRetrieverCore::new(
            Arc::new(vector_db),
            Arc::new(AlignedEmbedding),
            graph.clone(),
            None,
            None,
            None,
        );
        (core, graph)
    }

    /// An empty or blank-query batch is rejected before the empty-graph
    /// shortcut, so the answer does not depend on whether the graph has data.
    #[tokio::test]
    async fn an_invalid_batch_is_rejected_even_on_an_empty_graph() {
        let core = GraphRetrieverCore::new(
            Arc::new(MockVectorDB::new()),
            Arc::new(AlignedEmbedding),
            Arc::new(MockGraphDB::new()),
            None,
            None,
            None,
        );
        let params = SearchParams::default();
        for queries in [&[][..], &["q", " "][..]] {
            assert!(
                matches!(
                    core.search_batch(queries, &params).await,
                    Err(SearchError::InvalidInput(_))
                ),
                "{queries:?}"
            );
        }
        // A lone query on an empty graph is still an empty context.
        assert_eq!(core.search("q", &params).await.unwrap().len(), 0);
    }

    /// Python's `get_triplets_batch`: a lone query takes the single-query
    /// path, with its seed-scoped load; several share one full-graph load.
    #[tokio::test]
    async fn a_lone_query_runs_single_and_several_share_one_batch_load() {
        let (core, graph) = core().await;
        let params = SearchParams::default();

        let single = core.search_batch(&["q"], &params).await.unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].len(), 1);
        let calls = graph.get_call_log();
        assert!(
            calls.iter().any(|call| call == "get_neighborhood"),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call == "get_graph_data"),
            "{calls:?}"
        );

        let batch = core.search_batch(&["q", "r", "s"], &params).await.unwrap();
        assert_eq!(batch.len(), 3);
        assert!(batch.iter().all(|context| context.len() == 1), "{batch:?}");
        let calls = graph.get_call_log();
        assert_eq!(
            calls
                .iter()
                .filter(|call| *call == "get_graph_data")
                .count(),
            1,
            "{calls:?}"
        );
    }
}
