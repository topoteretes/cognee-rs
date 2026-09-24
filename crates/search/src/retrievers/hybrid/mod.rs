//! Hybrid-retrieval retriever ([`HybridRetriever`], `SearchType::HybridCompletion`).
//!
//! Port of Python cognee's `cognee/modules/retrieval/hybrid_retriever.py` and
//! `cognee/modules/retrieval/hybrid/*.py`. The retriever fuses three lanes into
//! a single completion context: a chunk lane that merges the vector
//! `DocumentChunk_text` / `TextSummary_text` channels via Reciprocal Rank
//! Fusion (RRF, with optional importance-weight boosting), an entity lane, and a
//! standalone-facts lane. (Python dropped its per-query BM25 channel from the
//! chunk lane in SDK-322; so does this port.)
//!
//! [`HybridRetriever`] implements the crate's `SearchRetriever` trait and is
//! wired to `SearchType::HybridCompletion`; the chunk-ranking spine lives in the
//! private submodules below and is orchestrated via [`retrieve_hybrid_chunks`].

mod budget;
mod chunks;
mod context;
mod entities;
mod facts;
mod overflow;
mod pairs;
mod ranking;
mod results;

pub(crate) use chunks::{HybridChunksResult, retrieve_hybrid_chunks, search_collection};
pub(crate) use context::extract_used_ids;
pub(crate) use entities::{build_entities, format_entities};
pub(crate) use facts::{edge_rank_by_id, format_facts};

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{debug, warn};
use uuid::Uuid;

use cognee_embedding::EmbeddingEngine;
use cognee_graph::{GraphDBTrait, NodeTruthState};
use cognee_llm::{GenerationOptions, Llm};
use cognee_session::SessionContext;
use cognee_truth_subspace::align::query_coords;
use cognee_truth_subspace::{DEFAULT_K, load_centroids, pad_coords};
use cognee_vector::VectorDB;

use self::budget::{context_budget_chars, graph_budget_chars, section_cost, take_blocks_within};
use self::context::{format_hybrid_context, format_passages, format_passages_within_budget};
use self::entities::{EdgeBullet, EntityResult, format_entity};
use self::facts::{FactResult, fact_bullets, resolve_facts_top_k, select_facts_for_entities};
use self::results::result_id;
use crate::retrievers::SearchRetriever;
use crate::types::{
    SearchContext, SearchError, SearchItem, SearchOutput, SearchParams, SearchType,
};
use crate::utils::{
    DEFAULT_HYBRID_USER_PROMPT_TEMPLATE, HYBRID_SMALL_WINDOW_SYSTEM_PROMPT,
    HYBRID_SMALL_WINDOW_USER_PROMPT_TEMPLATE, build_messages_with_history, render_user_prompt,
    resolve_system_prompt,
};

/// Top-k for the chunks/entities/facts lanes when neither a per-lane knob nor
/// `top_k` is supplied — Python's `HybridRetriever.__init__` default.
const DEFAULT_TOP_K: usize = 5;
/// Ceiling a request-level `top_k` is clamped to before it feeds the lanes.
///
/// Port of `_hybrid_lane_top_k` / `DEFAULT_HYBRID_LANE_TOP_K`
/// (`get_search_type_retriever_instance.py:41-51`): Python caps it "so default
/// context stays small" — `cognee.search()` defaults `top_k` to 15, so every
/// default Python search runs 10 per lane. An explicit per-lane knob is not
/// capped.
const DEFAULT_HYBRID_LANE_TOP_K: usize = 10;

/// Resolve one lane's top-k: per-lane knob, else `top_k` capped at
/// [`DEFAULT_HYBRID_LANE_TOP_K`], else the constructor default.
fn lane_top_k(lane_knob: Option<usize>, top_k: Option<usize>, default: usize) -> usize {
    lane_knob
        .or(top_k.map(|top_k| top_k.min(DEFAULT_HYBRID_LANE_TOP_K)))
        .unwrap_or(default)
}
/// Default max edges expanded per entity (`HybridRetriever.__init__`).
const DEFAULT_MAX_EDGES_PER_ENTITY: usize = 10;
/// Default global-context-index top-k (inert in Phase 1).
const DEFAULT_GLOBAL_CONTEXT_INDEX_TOP_K: usize = 3;
/// Default node-name filter operator.
const DEFAULT_NODE_NAME_FILTER_OPERATOR: &str = "OR";

const ENTITY_DATA_TYPE: &str = "Entity";
const ENTITY_FIELD: &str = "name";
const EDGE_TYPE_DATA_TYPE: &str = "EdgeType";
const EDGE_TYPE_FIELD: &str = "relationship_name";

/// Completion retriever fusing the chunk lane (P1-07) and the entity/facts lane
/// (P1-08) into a single `SearchType::HybridCompletion` retriever.
///
/// Port of Python's `HybridRetriever` (`hybrid_retriever.py`). Embeds the query
/// once, runs the chunk lane and the entity/facts lane concurrently, tags each
/// lane's output with a `"kind"` payload discriminator, and renders the
/// sectioned prompt context Python feeds the LLM.
///
/// **Truth-subspace (`use_truth_weight`):** live as of Phase 2 (P2-07) — when
/// set and exactly one dataset is in scope, the chunk lane is re-ranked by
/// truth-subspace alignment; it fails open to baseline ranking on any error and
/// stays default-off. **Global-context (`include_global_context_index` /
/// `global_context_index_top_k`):** still accepted on the wire but inert — no
/// global-context section is produced (locked deferral).
pub struct HybridRetriever {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    graph_db: Arc<dyn GraphDBTrait>,
    llm: Arc<dyn Llm>,
    chunks_top_k: usize,
    entities_top_k: usize,
    facts_top_k: usize,
    max_edges_per_entity: usize,
    text_summaries_top_k: Option<usize>,
    use_importance_weight: bool,
    // Live as of Phase 2 (P2-07); default-off, fails open to baseline ranking.
    use_truth_weight: bool,
    // Accepted on the wire but inert (locked deferral).
    include_global_context_index: bool,
    #[allow(
        dead_code,
        reason = "global-context-index knob is accepted on the wire but inert in Phase 1"
    )]
    global_context_index_top_k: usize,
    node_name: Option<Vec<String>>,
    node_name_filter_operator: String,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl HybridRetriever {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        chunks_top_k: Option<usize>,
        entities_top_k: Option<usize>,
        facts_top_k: Option<usize>,
        max_edges_per_entity: Option<usize>,
        text_summaries_top_k: Option<usize>,
        use_importance_weight: Option<bool>,
        use_truth_weight: Option<bool>,
        include_global_context_index: Option<bool>,
        global_context_index_top_k: Option<usize>,
        node_name: Option<Vec<String>>,
        node_name_filter_operator: Option<String>,
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
            chunks_top_k: chunks_top_k.unwrap_or(DEFAULT_TOP_K),
            entities_top_k: entities_top_k.unwrap_or(DEFAULT_TOP_K),
            facts_top_k: facts_top_k.unwrap_or(DEFAULT_TOP_K),
            max_edges_per_entity: max_edges_per_entity.unwrap_or(DEFAULT_MAX_EDGES_PER_ENTITY),
            text_summaries_top_k,
            use_importance_weight: use_importance_weight.unwrap_or(true),
            use_truth_weight: use_truth_weight.unwrap_or(false),
            include_global_context_index: include_global_context_index.unwrap_or(false),
            global_context_index_top_k: global_context_index_top_k
                .unwrap_or(DEFAULT_GLOBAL_CONTEXT_INDEX_TOP_K),
            node_name,
            node_name_filter_operator: node_name_filter_operator
                .unwrap_or_else(|| DEFAULT_NODE_NAME_FILTER_OPERATOR.to_string()),
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }

    /// Entity/facts lane: the Rust equivalent of Python's
    /// `_retrieve_entities_and_facts` (`hybrid_retriever.py:157-201`). Runs the
    /// `Entity_name` search (node filter applied) and the
    /// `EdgeType_relationship_name` search (node filter NOT applied)
    /// concurrently, builds ranked entity edge bullets scoped to the requested
    /// node set, then selects facts ([`resolve_facts_top_k`],
    /// [`select_facts_for_entities`]).
    ///
    /// An `Entity_name` search failure is not fatal: Python's `search_entities`
    /// logs it and continues without entities (`entities.py:17-40`), which in
    /// turn hands the entity lane's edge budget to the facts lane.
    #[allow(clippy::too_many_arguments)]
    async fn retrieve_entities_and_facts(
        &self,
        query_vector: &[f32],
        entities_top_k: usize,
        facts_top_k: usize,
        max_edges_per_entity: usize,
        node_name: Option<&[String]>,
        node_name_filter_operator: &str,
    ) -> Result<(Vec<EntityResult>, Vec<FactResult>), SearchError> {
        let max_ranked_bullets = entities_top_k.saturating_mul(max_edges_per_entity);
        let edge_limit = max_ranked_bullets.saturating_add(facts_top_k);

        let entity_future = search_collection(
            &self.vector_db,
            ENTITY_DATA_TYPE,
            ENTITY_FIELD,
            query_vector,
            entities_top_k,
            node_name,
            node_name_filter_operator,
        );
        // Python passes `apply_node_filter=False` for the edge lane
        // (`hybrid_retriever.py:173-182`); the Rust equivalent is `node_name=None`.
        let edge_future = search_collection(
            &self.vector_db,
            EDGE_TYPE_DATA_TYPE,
            EDGE_TYPE_FIELD,
            query_vector,
            edge_limit,
            None,
            DEFAULT_NODE_NAME_FILTER_OPERATOR,
        );

        let (entity_result, edge_result) = tokio::join!(entity_future, edge_future);
        let entity_hits = entity_result.unwrap_or_else(|error| {
            warn!(%error, "Entity_name search failed; continuing without entities");
            Vec::new()
        });
        let edge_hits = edge_result?;

        let (entities, reachable_ids) = build_entities(
            self.graph_db.as_ref(),
            &entity_hits,
            max_edges_per_entity,
            &edge_rank_by_id(&edge_hits),
            node_name,
            node_name_filter_operator,
        )
        .await;

        let node_scoped = node_name.is_some_and(|names| !names.is_empty());
        let facts = select_facts_for_entities(
            &edge_hits,
            &entities,
            &reachable_ids,
            resolve_facts_top_k(&entities, node_scoped, facts_top_k, max_ranked_bullets),
            node_scoped,
        );

        Ok((entities, facts))
    }

    /// Truth-subspace alignment context for the chunk lane (Python
    /// `_build_truth_context`, `hybrid_retriever.py:110-143`).
    ///
    /// Returns `(q_coords, truth_state_by_id, current_truth_epoch)`. All three
    /// are `None` when the truth weight is off, no single dataset is in scope,
    /// or the dataset has no centroid slots — so ranking stays at exact
    /// baseline. This is the SINGLE fail-open boundary: any error inside
    /// [`Self::try_build_truth_context`] (a partial failure such as centroids
    /// loading fine but the graph batch call erroring) discards the
    /// already-computed `q_coords` and returns `(None, None, None)`, mirroring
    /// Python's one `try/except` around the whole computation.
    async fn build_truth_context(
        &self,
        use_truth_weight: bool,
        chunks_top_k: usize,
        query_vector: &[f32],
        dataset_id: Option<Uuid>,
        node_name: Option<&[String]>,
        node_name_filter_operator: &str,
    ) -> (
        Option<Vec<f64>>,
        Option<HashMap<String, NodeTruthState>>,
        Option<i64>,
    ) {
        if !use_truth_weight {
            return (None, None, None);
        }
        let Some(dataset_id) = dataset_id else {
            return (None, None, None);
        };

        match self
            .try_build_truth_context(
                chunks_top_k,
                query_vector,
                dataset_id,
                node_name,
                node_name_filter_operator,
            )
            .await
        {
            Ok(Some((q_coords, truth_state_by_id, current_truth_epoch))) => (
                Some(q_coords),
                Some(truth_state_by_id),
                Some(current_truth_epoch),
            ),
            Ok(None) => (None, None, None),
            Err(error) => {
                debug!(%error, "truth-subspace lookup failed; using baseline ranking");
                (None, None, None)
            }
        }
    }

    /// Fallible inner body of [`Self::build_truth_context`]. The `Option` layer
    /// distinguishes "no centroids → no truth context" (`Ok(None)`) from a hard
    /// error (`Err`); both collapse to `(None, None, None)` at the caller. Every
    /// `.await?` propagates via `SearchError`'s `From<VectorDBError>` /
    /// `From<GraphDBError>` impls.
    async fn try_build_truth_context(
        &self,
        chunks_top_k: usize,
        query_vector: &[f32],
        dataset_id: Uuid,
        node_name: Option<&[String]>,
        node_name_filter_operator: &str,
    ) -> Result<Option<(Vec<f64>, HashMap<String, NodeTruthState>, i64)>, SearchError> {
        let centroids =
            load_centroids(self.vector_db.as_ref(), &dataset_id.to_string(), DEFAULT_K).await?;
        if centroids.is_empty() {
            return Ok(None);
        }

        let centroid_vectors: Vec<Vec<f64>> =
            centroids.iter().map(|c| c.centroid.clone()).collect();
        let query_vector_f64: Vec<f64> = query_vector.iter().map(|&v| v as f64).collect();
        let q_coords = pad_coords(
            &query_coords(&query_vector_f64, &centroid_vectors),
            DEFAULT_K,
        );
        // `centroids` is non-empty (checked above), so `max()` is always `Some`.
        #[allow(
            clippy::expect_used,
            reason = "centroids is non-empty (early-returned above), so max() cannot be None"
        )]
        let current_truth_epoch = centroids
            .iter()
            .map(|c| c.truth_epoch)
            .max()
            .expect("centroids is non-empty, checked above");

        let candidate_ids = self
            .candidate_chunk_ids(
                chunks_top_k,
                query_vector,
                node_name,
                node_name_filter_operator,
            )
            .await?;
        if candidate_ids.is_empty() {
            return Ok(Some((q_coords, HashMap::new(), current_truth_epoch)));
        }

        let truth_state_by_id = self.graph_db.get_node_truth_state(&candidate_ids).await?;
        Ok(Some((q_coords, truth_state_by_id, current_truth_epoch)))
    }

    /// Candidate `DocumentChunk` ids whose truth alignments the truth lane
    /// batch-fetches (Python `_candidate_chunk_ids`, `hybrid_retriever.py:145-165`).
    ///
    /// Reuses the same [`search_collection`] entry point and candidate window
    /// (`chunks_top_k * 2`) as the chunk lane so the truth
    /// coords map covers exactly the chunks ranking can surface. `chunks_top_k`
    /// is the request-resolved value threaded down from `get_context`, not the
    /// constructor default, so a per-request `chunks_top_k`/`top_k` override
    /// widens or narrows the truth window in lockstep with the chunk lane
    /// (Python constructs the retriever per request, so its `self.chunks_top_k`
    /// is already request-resolved). Ids are derived via [`result_id`] and the
    /// `None`s dropped (Python's `if chunk_id:`).
    async fn candidate_chunk_ids(
        &self,
        chunks_top_k: usize,
        query_vector: &[f32],
        node_name: Option<&[String]>,
        node_name_filter_operator: &str,
    ) -> Result<Vec<String>, SearchError> {
        let candidate_limit = chunks_top_k.saturating_mul(2);
        if candidate_limit == 0 {
            return Ok(vec![]);
        }
        let hits = search_collection(
            &self.vector_db,
            "DocumentChunk",
            "text",
            query_vector,
            candidate_limit,
            node_name,
            node_name_filter_operator,
        )
        .await?;
        Ok(hits.iter().filter_map(result_id).collect())
    }

    /// Render the three lanes into the sectioned context the model is shown,
    /// spending the model's input window in the order the sections earn it.
    ///
    /// When the whole context fits the window it is rendered exactly as
    /// Python's `format_hybrid_context` renders it (`context.py:8-27`) — a
    /// hosted model with a 128k-token window pays nothing for this. When it
    /// does not fit, the budget decides: the graph sections first, because they
    /// say more per token than the prose they were built from, then whole raw
    /// passages best-ranked first for whatever is left. See [`budget`] for the
    /// measurements behind that order.
    fn assemble_context(
        &self,
        chunks: &[SearchItem],
        entities: &[EntityResult],
        facts: &[FactResult],
        overhead_chars: usize,
        overflow_summaries: &HashMap<String, String>,
    ) -> (String, bool, Vec<String>) {
        let unbudgeted = format_hybrid_context(
            None,
            &format_passages(chunks),
            &format_entities(entities),
            &format_facts(facts),
        );

        let budget = context_budget_chars(
            self.llm.max_context_length(),
            self.completion_reserve_tokens(),
            overhead_chars,
        );
        if unbudgeted.len() <= budget {
            return (unbudgeted, false, Vec::new());
        }

        let graph_budget = graph_budget_chars(budget);
        let entities_section = take_blocks_within(
            "## Relevant entities",
            entities.iter().map(format_entity),
            "\n\n",
            graph_budget,
        );
        let facts_section = take_blocks_within(
            "## Related facts",
            fact_bullets(facts),
            "\n",
            graph_budget.saturating_sub(section_cost(&entities_section, SECTION_SEPARATOR)),
        );

        let graph_spend = section_cost(&entities_section, SECTION_SEPARATOR)
            + section_cost(&facts_section, SECTION_SEPARATOR);
        let (passages, unsummarized) = format_passages_within_budget(
            chunks,
            budget.saturating_sub(graph_spend),
            overflow_summaries,
        );

        let entities_section = entities_section.unwrap_or_default();
        let facts_section = facts_section.unwrap_or_default();
        let context = format_hybrid_context(None, &passages, &entities_section, &facts_section);
        debug!(
            was = unbudgeted.len(),
            now = context.len(),
            budget,
            "Hybrid context budgeted to the model's input window"
        );
        (context, true, unsummarized)
    }

    /// Tokens held back from the window for the answer itself.
    ///
    /// **This number and the adapter's own output cap are one decision, and
    /// they must agree.** The reserve is how much of the window the prompt
    /// leaves unspent; the output cap is how much of it the decoder is allowed
    /// to use. Reserve less than the decoder may write and a long answer runs
    /// off the end of the window mid-generation, which is not a truncated
    /// answer but a hard rejection. `cognee-llm-litert`'s
    /// `DEFAULT_MAX_OUTPUT_TOKENS` is the other half of this pair; change one
    /// and change the other.
    ///
    /// 512 tokens is roughly 380 words — ample for a RAG answer — and on a
    /// 4096-token window buying it back from a quarter-window reserve is worth
    /// about one more whole passage of context.
    fn completion_reserve_tokens(&self) -> u32 {
        COMPLETION_RESERVE_TOKENS
    }
}

/// What [`format_hybrid_context`] costs to put one more section in: the
/// `"\n\n"` it joins them with.
const SECTION_SEPARATOR: usize = 2;

/// Window tokens kept for the answer. Paired with the LLM adapter's own
/// output cap — see [`HybridRetriever::completion_reserve_tokens`].
const COMPLETION_RESERVE_TOKENS: u32 = 512;

/// Tag a chunk lane [`SearchItem`] with `"kind": "chunk"` and carry its paired
/// summary onto the item's own payload — Python's `chunk_summaries` map, which
/// it keeps on the retrieved objects but no longer renders into the prompt.
fn tag_chunk_item(mut item: SearchItem, chunk_summaries: &HashMap<String, String>) -> SearchItem {
    let summary = result_id(&item).and_then(|id| chunk_summaries.get(&id).cloned());
    if let Value::Object(map) = &mut item.payload {
        map.insert("kind".to_string(), Value::String("chunk".to_string()));
        if let Some(summary) = summary {
            map.insert("chunk_summary".to_string(), Value::String(summary));
        }
    }
    item
}

/// Stamp the resolved dataset scope onto a graph-derived item's payload so
/// dataset-scope bucketing retains it.
///
/// Entities and facts come from the knowledge graph and — unlike vector chunks,
/// which carry their real `dataset_ids` membership from the store — have no
/// per-item dataset membership of their own. Without a dataset key,
/// [`scope_context_by_datasets`](crate::orchestration::scope_context_by_datasets)
/// buckets them nowhere and silently discards them under any dataset scope,
/// leaving a scoped hybrid completion to answer from chunks alone. Python's
/// hybrid retriever flows entity/fact context regardless of scope, so we mirror
/// that by stamping the full resolved scope (`scope`) onto every such item:
/// the bucketer's authoritative `dataset_ids` array then assigns the item to
/// every requested dataset. A no-op when `scope` is empty (unscoped request).
fn stamp_scope(mut item: SearchItem, scope: &[String]) -> SearchItem {
    if scope.is_empty() {
        return item;
    }
    if let Value::Object(map) = &mut item.payload {
        map.insert(
            "dataset_ids".to_string(),
            Value::Array(scope.iter().cloned().map(Value::String).collect()),
        );
    }
    item
}

/// Convert an [`EntityResult`] into a `"kind": "entity"` tagged [`SearchItem`],
/// nesting the edge bullets as a JSON array so downstream consumers can walk
/// them without a second schema. `scope` is the resolved dataset scope stamped
/// on so the item survives dataset bucketing (see [`stamp_scope`]).
fn entity_to_item(entity: &EntityResult, scope: &[String]) -> SearchItem {
    let edges: Vec<Value> = entity
        .edges
        .iter()
        .map(|edge| {
            json!({
                "text": edge.text,
                "source": edge.source,
                "target": edge.target,
                "source_id": edge.source_id,
                "relationship": edge.relationship,
                "target_id": edge.target_id,
                "edge_type_id": edge.edge_type_id,
            })
        })
        .collect();

    stamp_scope(
        SearchItem {
            id: Uuid::parse_str(&entity.id).ok(),
            score: None,
            payload: json!({
                "kind": "entity",
                "id": entity.id,
                "name": entity.name,
                "description": entity.description,
                "type": entity.entity_type,
                "edges": edges,
            }),
        },
        scope,
    )
}

/// Convert a [`FactResult`] into a `"kind": "fact"` tagged [`SearchItem`]. No
/// `source_id`/`target_id`/`edges` — the field-level contract that lets a
/// kind-aware id extractor skip facts. `scope` is the resolved dataset scope
/// stamped on so the item survives dataset bucketing (see [`stamp_scope`]).
fn fact_to_item(fact: &FactResult, scope: &[String]) -> SearchItem {
    stamp_scope(
        SearchItem {
            id: Uuid::parse_str(&fact.id).ok(),
            score: None,
            payload: json!({
                "kind": "fact",
                "id": fact.id,
                "text": fact.text,
            }),
        },
        scope,
    )
}

fn payload_str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn payload_str_opt(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Reconstruct an [`EntityResult`] from a `"kind": "entity"` tagged item.
fn item_to_entity(item: &SearchItem) -> EntityResult {
    let payload = &item.payload;
    let edges = payload
        .get("edges")
        .and_then(Value::as_array)
        .map(|edges| {
            edges
                .iter()
                .map(|edge| EdgeBullet {
                    text: payload_str(edge, "text"),
                    source: payload_str_opt(edge, "source"),
                    target: payload_str_opt(edge, "target"),
                    source_id: payload_str_opt(edge, "source_id"),
                    relationship: payload_str_opt(edge, "relationship"),
                    target_id: payload_str_opt(edge, "target_id"),
                    edge_type_id: payload_str_opt(edge, "edge_type_id"),
                })
                .collect()
        })
        .unwrap_or_default();

    EntityResult {
        id: payload_str(payload, "id"),
        name: payload_str(payload, "name"),
        description: payload_str_opt(payload, "description"),
        entity_type: payload_str_opt(payload, "type"),
        edges,
        // Only fact selection reads it, and that runs before serialization.
        covered_edge_type_ids: vec![],
    }
}

/// Reconstruct a [`FactResult`] from a `"kind": "fact"` tagged item.
fn item_to_fact(item: &SearchItem) -> FactResult {
    FactResult {
        id: payload_str(&item.payload, "id"),
        text: payload_str(&item.payload, "text"),
    }
}

#[async_trait]
impl SearchRetriever for HybridRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::HybridCompletion
    }

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        // Three-layer resolution for the top-k family: request knob -> capped
        // top_k -> constructor default (see [`lane_top_k`]).
        let chunks_top_k = lane_top_k(params.chunks_top_k, params.top_k, self.chunks_top_k);
        let entities_top_k = lane_top_k(params.entities_top_k, params.top_k, self.entities_top_k);
        let facts_top_k = lane_top_k(params.facts_top_k, params.top_k, self.facts_top_k);
        let max_edges_per_entity = params
            .max_edges_per_entity
            .unwrap_or(self.max_edges_per_entity);
        let text_summaries_top_k = params.text_summaries_top_k.or(self.text_summaries_top_k);
        let use_importance_weight = params
            .use_importance_weight
            .unwrap_or(self.use_importance_weight);
        let node_name = params.node_name.as_deref().or(self.node_name.as_deref());
        let node_name_filter_operator = params
            .node_name_filter_operator
            .as_deref()
            .unwrap_or(self.node_name_filter_operator.as_str());

        if params
            .include_global_context_index
            .unwrap_or(self.include_global_context_index)
        {
            debug!(
                "HYBRID_COMPLETION: include_global_context_index is set but unsupported in \
                 Phase 1; no global-context section will be produced"
            );
        }
        // Python raises `NoDataError` here (`hybrid_retriever.py:102-109`,
        // SDK-270): an empty graph is a state problem, not a query miss, and
        // this is the default search type — it must not answer a fresh install
        // with an empty context while the other completion types report it.
        if self.graph_db.is_empty().await? {
            return Err(SearchError::NotFound(
                "The knowledge graph is empty. Ingest data through Cognee before searching."
                    .to_string(),
            ));
        }

        // Embed the query once and share the vector across both lanes.
        let embeddings = self.embedding_engine.embed(&[query]).await?;
        let query_vector = embeddings.into_iter().next().ok_or_else(|| {
            SearchError::InvalidInput("embedding engine returned no vectors".to_string())
        })?;

        // Build the truth-subspace context (default-off; fails open to baseline).
        let use_truth_weight = params.use_truth_weight.unwrap_or(self.use_truth_weight);
        let (q_coords, truth_state_by_id, current_truth_epoch) = self
            .build_truth_context(
                use_truth_weight,
                chunks_top_k,
                &query_vector,
                params.dataset_id,
                node_name,
                node_name_filter_operator,
            )
            .await;

        let (chunk_result, (entities, facts)) = tokio::try_join!(
            retrieve_hybrid_chunks(
                &self.vector_db,
                chunks_top_k,
                text_summaries_top_k,
                node_name,
                node_name_filter_operator,
                use_importance_weight,
                &query_vector,
                use_truth_weight,
                q_coords.as_deref(),
                truth_state_by_id.as_ref(),
                current_truth_epoch,
            ),
            self.retrieve_entities_and_facts(
                &query_vector,
                entities_top_k,
                facts_top_k,
                max_edges_per_entity,
                node_name,
                node_name_filter_operator,
            ),
        )?;

        let HybridChunksResult {
            chunks,
            chunk_summaries,
        } = chunk_result;

        // Resolved dataset scope stamped onto graph-derived entity/fact items so
        // they survive `scope_context_by_datasets` bucketing (chunks already
        // carry their real `dataset_ids` membership from the vector store).
        let scope_dataset_ids: Vec<String> = params
            .dataset_ids
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(Uuid::to_string)
            .collect();

        let mut context: SearchContext =
            Vec::with_capacity(chunks.len() + entities.len() + facts.len());
        for chunk in chunks {
            context.push(tag_chunk_item(chunk, &chunk_summaries));
        }
        for entity in &entities {
            context.push(entity_to_item(entity, &scope_dataset_ids));
        }
        for fact in &facts {
            context.push(fact_to_item(fact, &scope_dataset_ids));
        }
        Ok(context)
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

        // Partition the flat context back into the three lanes by "kind".
        let mut chunks: Vec<SearchItem> = Vec::new();
        let mut entities: Vec<EntityResult> = Vec::new();
        let mut facts: Vec<FactResult> = Vec::new();
        for item in &completion_context {
            match item.payload.get("kind").and_then(Value::as_str) {
                Some("entity") => entities.push(item_to_entity(item)),
                Some("fact") => facts.push(item_to_fact(item)),
                Some("chunk") => chunks.push(item.clone()),
                _ => {}
            }
        }

        let configured_system_prompt = params
            .system_prompt
            .as_deref()
            .or(self.system_prompt.as_deref());
        let configured_system_prompt_path = params
            .system_prompt_path
            .as_deref()
            .or(self.system_prompt_path.as_deref());
        let configured_template = self.user_prompt_template.as_deref();
        let default_system_prompt =
            resolve_system_prompt(configured_system_prompt, configured_system_prompt_path)?;

        // Everything the prompt costs before a single retrieved item is added:
        // the system prompt, the template actually in use with the question
        // filled in, and the session history. Which wording is used is not
        // settled yet -- budgeting is what selects it -- so with nothing
        // configured both candidates are costed and the longer one wins, and
        // the budget is never optimistic.
        let overhead_system =
            if configured_system_prompt.is_none() && configured_system_prompt_path.is_none() {
                default_system_prompt
                    .len()
                    .max(HYBRID_SMALL_WINDOW_SYSTEM_PROMPT.len())
            } else {
                default_system_prompt.len()
            };
        let overhead_template = match configured_template {
            Some(template) => render_user_prompt(Some(template), query, "").len(),
            None => render_user_prompt(Some(DEFAULT_HYBRID_USER_PROMPT_TEMPLATE), query, "")
                .len()
                .max(
                    render_user_prompt(Some(HYBRID_SMALL_WINDOW_USER_PROMPT_TEMPLATE), query, "")
                        .len(),
                ),
        };
        let overhead = overhead_system + overhead_template + session.formatted_history.len();

        let mut overflow_summaries = overflow::known(&chunks);
        let (mut context_text, budgeted, unsummarized) =
            self.assemble_context(&chunks, &entities, &facts, overhead, &overflow_summaries);

        // Python's `skip_completion_on_empty_context` (`hybrid_retriever.py:52,
        // 246-253`, SDK-270): search is not an LLM gateway, and with every
        // section empty the only possible answer is a "no context provided"
        // deflection. No completion, no session turn — `Texts([])` is not the
        // `Text` the orchestrator records a QA turn for.
        if context_text.is_empty() {
            warn!("Empty context: skipping LLM completion, returning no results");
            return Ok(SearchOutput::Texts(Vec::new()));
        }

        // Passages that did not fit and have no summary yet: ask the answering
        // model for one, then rebuild the context with them in. Only the
        // passages this question actually pushed out are summarized, and only
        // once each — `overflow::known()` above already carries everything
        // earlier questions paid for.
        if budgeted && !unsummarized.is_empty() && overflow::summarization_enabled() {
            let to_summarize: Vec<(String, String)> = unsummarized
                .iter()
                .filter_map(|id| {
                    let chunk = chunks
                        .iter()
                        .find(|chunk| result_id(chunk).as_deref() == Some(id))?;
                    let text = payload_str_opt(&chunk.payload, "text")?;
                    Some((id.clone(), text))
                })
                .collect();
            if !to_summarize.is_empty() {
                let produced =
                    overflow::summarize(self.llm.as_ref(), &to_summarize, overflow::batch_size())
                        .await;
                if !produced.is_empty() {
                    overflow_summaries.extend(produced);
                    let (rebuilt, _, _) = self.assemble_context(
                        &chunks,
                        &entities,
                        &facts,
                        overhead,
                        &overflow_summaries,
                    );
                    context_text = rebuilt;
                }
            }
        }
        let context_text = context_text;

        // A context that had to be budgeted is a context long enough to lose
        // the question in, and a model with a window that small is the one
        // that reads two brevity instructions as an instruction to say
        // nothing. That is the case the second pair of prompts exists for.
        let use_small_window = budgeted
            && configured_system_prompt.is_none()
            && configured_system_prompt_path.is_none();
        let system_prompt = if use_small_window {
            HYBRID_SMALL_WINDOW_SYSTEM_PROMPT.to_string()
        } else {
            default_system_prompt
        };
        let template = configured_template.unwrap_or(if budgeted {
            HYBRID_SMALL_WINDOW_USER_PROMPT_TEMPLATE
        } else {
            DEFAULT_HYBRID_USER_PROMPT_TEMPLATE
        });

        let user_prompt = render_user_prompt(Some(template), query, &context_text);

        debug!(
            context_items = completion_context.len(),
            "Hybrid context assembled:\n{context_text}"
        );
        debug!("LLM user prompt:\n{user_prompt}");

        let messages = build_messages_with_history(system_prompt, user_prompt, session);

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

    /// HYBRID_COMPLETION rejects query batches at every entry point (Python
    /// `_reject_query_batch`, `hybrid_retriever.py:300-302`). No default
    /// per-query-loop fallback.
    async fn get_context_batch(
        &self,
        _queries: &[String],
        _params: &SearchParams,
    ) -> Result<Vec<SearchContext>, SearchError> {
        Err(SearchError::InvalidInput(
            "HYBRID_COMPLETION does not support query_batch.".to_string(),
        ))
    }

    async fn get_completion_batch(
        &self,
        _queries: &[String],
        _contexts: Option<Vec<SearchContext>>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<Vec<SearchOutput>, SearchError> {
        Err(SearchError::InvalidInput(
            "HYBRID_COMPLETION does not support query_batch.".to_string(),
        ))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod retriever_tests {
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_graph::{GraphDBTrait, MockGraphDB};
    use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmResult, Message, TokenUsage};
    use cognee_models::EdgeType;
    use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
    use serde_json::{Value, json};
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use super::HybridRetriever;
    use crate::retrievers::SearchRetriever;
    use crate::types::{SearchContext, SearchError, SearchItem, SearchOutput, SearchParams};
    use crate::utils::DEFAULT_RAG_SYSTEM_PROMPT;

    const CHUNK_TEXT: &str = "the rust ownership model chunk";
    const ENTITY_NAME: &str = "Alice";
    const BULLET_TEXT: &str = "Alice works at Acme.";
    const FACT_TEXT: &str = "Acme acquired Initech.";

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

    #[derive(Default)]
    struct CapturingLlm {
        last_messages: Mutex<Vec<Message>>,
        response_text: String,
        structured_response: Option<Value>,
        /// Reported window; `None` keeps the trait default.
        max_context: Option<u32>,
    }

    #[async_trait]
    impl Llm for CapturingLlm {
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
            messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            self.last_messages.lock().unwrap().clone_from(&messages);
            Ok(self
                .structured_response
                .clone()
                .unwrap_or_else(|| json!({ "ok": true })))
        }

        fn model(&self) -> &str {
            "test-model"
        }

        fn max_context_length(&self) -> u32 {
            self.max_context.unwrap_or(4096)
        }
    }

    async fn edge_props(edge_text: &str) -> HashMap<Cow<'static, str>, Value> {
        let mut props: HashMap<Cow<'static, str>, Value> = HashMap::new();
        props.insert(Cow::from("edge_text"), json!(edge_text));
        props
    }

    /// Build vector + graph DBs holding one chunk, one entity (Alice with a
    /// works_at edge to Acme), and two EdgeType rows (one that becomes an entity
    /// bullet, one that survives as a standalone fact).
    async fn populated_dbs() -> (Arc<dyn VectorDB>, Arc<dyn GraphDBTrait>, Uuid) {
        let db = MockVectorDB::new();

        // DocumentChunk_text lane.
        db.create_collection("DocumentChunk", "text", 2)
            .await
            .unwrap();
        let chunk_id = Uuid::new_v4();
        db.index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint::new(chunk_id, vec![1.0, 0.0])
                .with_metadata("id", json!(chunk_id.to_string()))
                .with_metadata("text", json!(CHUNK_TEXT))],
        )
        .await
        .unwrap();

        // Entity_name lane.
        db.create_collection("Entity", "name", 2).await.unwrap();
        let alice_id = Uuid::new_v4();
        db.index_points(
            "Entity",
            "name",
            &[VectorPoint::new(alice_id, vec![1.0, 0.0])
                .with_metadata("id", json!(alice_id.to_string()))
                .with_metadata("name", json!(ENTITY_NAME))],
        )
        .await
        .unwrap();

        // EdgeType_relationship_name lane: two rows.
        db.create_collection("EdgeType", "relationship_name", 2)
            .await
            .unwrap();
        let bullet_edge_id = EdgeType::deterministic_id(BULLET_TEXT);
        let fact_edge_id = EdgeType::deterministic_id(FACT_TEXT);
        db.index_points(
            "EdgeType",
            "relationship_name",
            &[
                VectorPoint::new(bullet_edge_id, vec![1.0, 0.0])
                    .with_metadata("id", json!(bullet_edge_id.to_string()))
                    .with_metadata("text", json!(BULLET_TEXT)),
                VectorPoint::new(fact_edge_id, vec![1.0, 0.0])
                    .with_metadata("id", json!(fact_edge_id.to_string()))
                    .with_metadata("text", json!(FACT_TEXT)),
            ],
        )
        .await
        .unwrap();

        // Graph: Alice --works_at--> Acme, with the bullet edge_text.
        let graph = MockGraphDB::new();
        graph
            .add_node_raw(json!({ "id": alice_id.to_string(), "name": ENTITY_NAME }))
            .await
            .unwrap();
        graph
            .add_node_raw(json!({ "id": "acme-id", "name": "Acme" }))
            .await
            .unwrap();
        graph
            .add_edge(
                &alice_id.to_string(),
                "acme-id",
                "works_at",
                Some(edge_props(BULLET_TEXT).await),
            )
            .await
            .unwrap();

        (Arc::new(db), Arc::new(graph), alice_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn retriever(
        vector_db: Arc<dyn VectorDB>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
    ) -> HybridRetriever {
        HybridRetriever::new(
            vector_db,
            Arc::new(AlignedEmbedding),
            graph_db,
            llm,
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
    }

    fn kinds(context: &SearchContext) -> Vec<String> {
        context
            .iter()
            .filter_map(|item| {
                item.payload
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    /// The `text` bodies of the chunk items, in context order.
    fn chunk_texts(context: &SearchContext) -> Vec<String> {
        context
            .iter()
            .filter(|item| item.payload.get("kind").and_then(Value::as_str) == Some("chunk"))
            .filter_map(|item| {
                item.payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    #[tokio::test]
    async fn get_context_returns_tagged_chunk_entity_and_fact_items() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, llm);

        let context = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(kinds(&context), vec!["chunk", "entity", "fact"]);

        let chunk = &context[0];
        assert_eq!(chunk.payload["text"], json!(CHUNK_TEXT));

        let entity = &context[1];
        assert_eq!(entity.payload["name"], json!(ENTITY_NAME));
        assert!(entity.payload["edges"].is_array());

        let fact = &context[2];
        assert_eq!(fact.payload["text"], json!(FACT_TEXT));
    }

    #[tokio::test]
    async fn dataset_scoped_context_retains_entity_and_fact_sections() {
        // Graph-derived entity/fact items carry no per-item dataset membership,
        // so before the fix `scope_context_by_datasets` bucketed them nowhere
        // and discarded them under scope. With the resolved scope stamped onto
        // their payloads they must survive bucketing alongside chunks — matching
        // Python, which flows entity/fact context regardless of scope.
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, llm);

        let dataset_a = Uuid::new_v4();
        let params = SearchParams {
            dataset_ids: Some(vec![dataset_a]),
            ..SearchParams::default()
        };

        let context = retriever.get_context("query", &params).await.unwrap();

        // The retriever stamped the resolved scope onto entity/fact items.
        for item in &context {
            match item.payload.get("kind").and_then(Value::as_str) {
                Some("entity") | Some("fact") => {
                    assert_eq!(
                        item.payload["dataset_ids"],
                        json!([dataset_a.to_string()]),
                        "entity/fact items must carry the resolved dataset scope"
                    );
                }
                _ => {}
            }
        }

        // The bucketer must retain entity + fact under the scoped dataset.
        let scoped = crate::orchestration::scope_context_by_datasets(&context, &[dataset_a]);
        let bucket = scoped.get(&dataset_a.to_string()).expect("scoped bucket");
        let bucket_kinds = kinds(bucket);
        assert!(
            bucket_kinds.iter().any(|k| k == "entity"),
            "entity section must survive dataset scoping, got {bucket_kinds:?}"
        );
        assert!(
            bucket_kinds.iter().any(|k| k == "fact"),
            "fact section must survive dataset scoping, got {bucket_kinds:?}"
        );
    }

    #[tokio::test]
    async fn facts_are_gated_off_when_node_name_is_set() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, llm);

        let params = SearchParams {
            node_name: Some(vec!["some-set".to_string()]),
            ..SearchParams::default()
        };
        let context = retriever.get_context("query", &params).await.unwrap();

        // No item is tagged as a fact when the search is node-scoped.
        assert!(!kinds(&context).iter().any(|kind| kind == "fact"));
    }

    #[tokio::test]
    async fn get_completion_renders_all_section_headers() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let llm = Arc::new(CapturingLlm {
            response_text: "hybrid answer".to_string(),
            ..Default::default()
        });
        let retriever = retriever(vector_db, graph_db, Arc::clone(&llm) as Arc<dyn Llm>);

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
            SearchOutput::Text(text) => assert_eq!(text, "hybrid answer"),
            _ => panic!("expected text output"),
        }

        let messages = llm.last_messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, DEFAULT_RAG_SYSTEM_PROMPT);
        let user = &messages[1].content;
        assert!(user.contains("what happened?"));
        assert!(user.contains("## Relevant passages"));
        assert!(user.contains("## Relevant entities"));
        assert!(user.contains("## Related facts"));
        assert!(user.contains(CHUNK_TEXT));
        assert!(user.contains(FACT_TEXT));
    }

    #[tokio::test]
    async fn get_completion_reuses_provided_context_without_running_lanes() {
        // Empty vector DB: if the lanes ran, the chunk lane would raise NotFound
        // on the missing DocumentChunk_text collection.
        let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
        let llm = Arc::new(CapturingLlm {
            response_text: "reused".to_string(),
            ..Default::default()
        });
        let retriever = retriever(vector_db, graph_db, Arc::clone(&llm) as Arc<dyn Llm>);

        let provided: SearchContext = vec![
            SearchItem {
                id: None,
                score: None,
                payload: json!({ "kind": "chunk", "id": "c1", "text": "provided chunk" }),
            },
            SearchItem {
                id: None,
                score: None,
                payload: json!({
                    "kind": "entity",
                    "id": "e1",
                    "name": "Bob",
                    "description": Value::Null,
                    "type": Value::Null,
                    "edges": []
                }),
            },
        ];

        let output = retriever
            .get_completion(
                "who?",
                Some(provided),
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => assert_eq!(text, "reused"),
            _ => panic!("expected text output"),
        }

        let messages = llm.last_messages.lock().unwrap().clone();
        let user = &messages[1].content;
        assert!(user.contains("provided chunk"));
        assert!(user.contains("### Bob"));
    }

    #[tokio::test]
    async fn response_schema_selects_structured_output() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let llm = Arc::new(CapturingLlm {
            structured_response: Some(json!({ "answer": "structured" })),
            ..Default::default()
        });
        let retriever = retriever(vector_db, graph_db, llm);

        let params = SearchParams {
            response_schema: Some(json!({ "type": "object" })),
            ..SearchParams::default()
        };
        let output = retriever
            .get_completion("q", None, &SessionContext::default(), &params)
            .await
            .unwrap();

        match output {
            SearchOutput::Structured(value) => {
                assert_eq!(value, json!({ "answer": "structured" }));
            }
            _ => panic!("expected structured output"),
        }
    }

    #[tokio::test]
    async fn respects_request_level_top_k_override() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;

        // Index extra DocumentChunk_text rows (aligned with the query vector) so
        // several chunks match. With the default limit (15) every one survives
        // ranking; a request-level chunks_top_k of 1 must truncate the effective
        // limit down to a single chunk — that difference is what proves the
        // override is honored rather than silently ignored.
        for i in 0..3 {
            let extra = Uuid::new_v4();
            vector_db
                .index_points(
                    "DocumentChunk",
                    "text",
                    &[VectorPoint::new(extra, vec![1.0, 0.0])
                        .with_metadata("id", json!(extra.to_string()))
                        .with_metadata("text", json!(format!("{CHUNK_TEXT} {i}")))],
                )
                .await
                .unwrap();
        }

        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, llm);

        // Baseline: without an override the default limit keeps all matching
        // chunks, so more than one chunk comes back.
        let default_context = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap();
        let default_chunks = kinds(&default_context)
            .iter()
            .filter(|k| *k == "chunk")
            .count();
        assert!(
            default_chunks > 1,
            "default limit should return every matching chunk, got {default_chunks}"
        );

        // A per-request chunks_top_k of 1 truncates the effective limit to a
        // single chunk.
        let params = SearchParams {
            chunks_top_k: Some(1),
            ..SearchParams::default()
        };
        let context = retriever.get_context("query", &params).await.unwrap();
        let chunk_count = kinds(&context).iter().filter(|k| *k == "chunk").count();
        assert_eq!(chunk_count, 1);
    }

    #[tokio::test]
    async fn batch_methods_reject_with_invalid_input() {
        let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, llm);

        let queries = vec!["a".to_string(), "b".to_string()];
        let params = SearchParams::default();

        match retriever.get_context_batch(&queries, &params).await {
            Err(SearchError::InvalidInput(message)) => {
                assert_eq!(message, "HYBRID_COMPLETION does not support query_batch.");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }

        match retriever
            .get_completion_batch(&queries, None, &SessionContext::default(), &params)
            .await
        {
            Err(SearchError::InvalidInput(message)) => {
                assert_eq!(message, "HYBRID_COMPLETION does not support query_batch.");
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    /// A lone `top_k` (all per-channel knobs `None`) must drive EVERY lane's
    /// limit through the `.or(top_k)` middle layer, not just the chunk lane. Seed
    /// 3 aligned chunks and 3 aligned entities; with the default limit (15) more
    /// than one of each survives, but `top_k = 1` must cap BOTH lanes to exactly
    /// one item. A regression that fed `top_k` only to chunks would leave the
    /// entity lane at its default 15 and return all 3 entities.
    #[tokio::test]
    async fn top_k_drives_all_channel_limits_when_no_per_channel_knob() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;

        // Grow each vector lane to 3 aligned rows (populated_dbs seeds one each).
        for i in 0..2 {
            let extra_chunk = Uuid::new_v4();
            vector_db
                .index_points(
                    "DocumentChunk",
                    "text",
                    &[VectorPoint::new(extra_chunk, vec![1.0, 0.0])
                        .with_metadata("id", json!(extra_chunk.to_string()))
                        .with_metadata("text", json!(format!("{CHUNK_TEXT} {i}")))],
                )
                .await
                .unwrap();
            let extra_entity = Uuid::new_v4();
            vector_db
                .index_points(
                    "Entity",
                    "name",
                    &[VectorPoint::new(extra_entity, vec![1.0, 0.0])
                        .with_metadata("id", json!(extra_entity.to_string()))
                        .with_metadata("name", json!(format!("{ENTITY_NAME} {i}")))],
                )
                .await
                .unwrap();
        }

        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, llm);

        // Baseline: the default limit keeps more than one of BOTH kinds, so the
        // top_k=1 assertions below are non-vacuous.
        let default_context = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap();
        let default_kinds = kinds(&default_context);
        assert!(
            default_kinds.iter().filter(|k| *k == "chunk").count() > 1,
            "default limit should keep >1 chunk, got {default_kinds:?}"
        );
        assert!(
            default_kinds.iter().filter(|k| *k == "entity").count() > 1,
            "default limit should keep >1 entity, got {default_kinds:?}"
        );

        // Only top_k set; every per-channel knob left None.
        let params = SearchParams {
            top_k: Some(1),
            ..SearchParams::default()
        };
        let context = retriever.get_context("query", &params).await.unwrap();
        let ctx_kinds = kinds(&context);
        assert_eq!(
            ctx_kinds.iter().filter(|k| *k == "chunk").count(),
            1,
            "top_k must cap the chunk lane to one item, got {ctx_kinds:?}"
        );
        assert_eq!(
            ctx_kinds.iter().filter(|k| *k == "entity").count(),
            1,
            "top_k must feed the entity lane too, got {ctx_kinds:?}"
        );
    }

    /// The importance-weight boost is applied by default and can be disabled.
    /// Two chunks with IDENTICAL vectors differ only in `importance_weight`
    /// (0.0 vs 1.0). With importance ON (default) the `high` chunk's 1.25 factor
    /// overtakes `low`'s 0.75 despite the one-slot RRF gap; with importance OFF
    /// ranking collapses to pure RRF by vector rank, so `low` (indexed first ->
    /// rank 0) leads and the order flips.
    #[tokio::test]
    async fn get_context_applies_importance_weight_by_default_and_honors_disable() {
        let db = MockVectorDB::new();
        db.create_collection("DocumentChunk", "text", 2)
            .await
            .unwrap();
        // `low` indexed first -> vector_rank 0 (stable tie order in MockVectorDB).
        let low_id = Uuid::new_v4();
        db.index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint::new(low_id, vec![1.0, 0.0])
                .with_metadata("id", json!(low_id.to_string()))
                .with_metadata("text", json!("low importance chunk"))
                .with_metadata("importance_weight", json!(0.0))],
        )
        .await
        .unwrap();
        let high_id = Uuid::new_v4();
        db.index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint::new(high_id, vec![1.0, 0.0])
                .with_metadata("id", json!(high_id.to_string()))
                .with_metadata("text", json!("high importance chunk"))
                .with_metadata("importance_weight", json!(1.0))],
        )
        .await
        .unwrap();

        let vector_db: Arc<dyn VectorDB> = Arc::new(db);
        // Any node at all: an empty graph is rejected before the lanes run.
        let graph = MockGraphDB::new();
        graph
            .add_node_raw(json!({"id": Uuid::new_v4().to_string(), "name": "seed"}))
            .await
            .unwrap();
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(graph);
        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, llm);

        // (1) Default: importance weighting ON -> high outranks low.
        let default_ctx = retriever
            .get_context("query", &SearchParams::default())
            .await
            .unwrap();
        let default_order = chunk_texts(&default_ctx);
        assert_eq!(
            default_order,
            vec!["high importance chunk", "low importance chunk"],
            "importance-on default must rank the higher-weight chunk first"
        );

        // (2) Disabled: pure RRF -> low (rank 0) leads; order distinct from (1).
        let params = SearchParams {
            use_importance_weight: Some(false),
            ..SearchParams::default()
        };
        let disabled_ctx = retriever.get_context("query", &params).await.unwrap();
        let disabled_order = chunk_texts(&disabled_ctx);
        assert_eq!(
            disabled_order,
            vec!["low importance chunk", "high importance chunk"],
            "importance-off ranking must fall back to pure RRF by vector rank"
        );
        assert_ne!(
            disabled_order, default_order,
            "disabling importance weighting must change the order"
        );
    }

    /// End-to-end truth-subspace re-ranking through the real `get_context`
    /// wiring (`build_truth_context` -> `MockVectorDB` centroid load ->
    /// `get_node_truth_state` -> `rank_chunk_summary_pairs`). Two identical-vector
    /// chunks: `b` is indexed first (RRF rank 0, the baseline winner), `a` second
    /// (rank 1) but carries an aligned truth state at the current epoch. With the
    /// truth weight ON `a`'s 1.25 alignment factor lifts it past `b`; with it OFF
    /// the baseline RRF order (`b` first) stands.
    #[tokio::test]
    async fn truth_subspace_lane_reranks_through_get_context() {
        use cognee_truth_subspace::{TruthCentroidPayload, upsert_centroids};

        let dataset_id = Uuid::new_v4();
        let db = MockVectorDB::new();
        db.create_collection("DocumentChunk", "text", 2)
            .await
            .unwrap();

        // `b` first -> vector_rank 0; `a` second -> vector_rank 1.
        let chunk_b = Uuid::new_v4();
        db.index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint::new(chunk_b, vec![1.0, 0.0])
                .with_metadata("id", json!(chunk_b.to_string()))
                .with_metadata("text", json!("baseline chunk b"))],
        )
        .await
        .unwrap();
        let chunk_a = Uuid::new_v4();
        db.index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint::new(chunk_a, vec![1.0, 0.0])
                .with_metadata("id", json!(chunk_a.to_string()))
                .with_metadata("text", json!("aligned chunk a"))],
        )
        .await
        .unwrap();

        // One centroid slot for the dataset at epoch 3, aligned with the query.
        upsert_centroids(
            &db,
            &[TruthCentroidPayload {
                dataset_id: dataset_id.to_string(),
                slot: 0,
                count: 1,
                truth_epoch: 3,
                updated_at: 0,
                centroid: vec![1.0, 0.0],
                learning_ids: vec![],
            }],
        )
        .await
        .unwrap();

        let vector_db: Arc<dyn VectorDB> = Arc::new(db);

        // Only `a` has an aligned truth state at the current epoch (3); `b` is
        // absent from the graph, so it receives no truth multiplier.
        let graph = MockGraphDB::new();
        graph
            .add_node_raw(json!({
                "id": chunk_a.to_string(),
                "truth_alignment": [1.0],
                "truth_epoch": 3,
            }))
            .await
            .unwrap();
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(graph);

        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, llm);

        // Truth ON (single dataset in scope): `a` overtakes `b`.
        let truth_on = SearchParams {
            use_truth_weight: Some(true),
            dataset_id: Some(dataset_id),
            ..SearchParams::default()
        };
        let on_ctx = retriever.get_context("query", &truth_on).await.unwrap();
        assert_eq!(
            chunk_texts(&on_ctx),
            vec!["aligned chunk a", "baseline chunk b"],
            "truth weighting must lift the aligned chunk to first"
        );

        // Truth OFF: baseline RRF order (`b` first) stands.
        let truth_off = SearchParams {
            use_truth_weight: Some(false),
            dataset_id: Some(dataset_id),
            ..SearchParams::default()
        };
        let off_ctx = retriever.get_context("query", &truth_off).await.unwrap();
        assert_eq!(
            chunk_texts(&off_ctx),
            vec!["baseline chunk b", "aligned chunk a"],
            "with truth off the baseline RRF order must stand"
        );
    }

    /// The rendered user prompt must carry the three context sections in the
    /// exact order passages -> entities -> facts, each joined by a blank line and
    /// built from the real seeded bodies (not order-blind `.contains`). This pins
    /// the `format_hybrid_context` section ordering + join through the full
    /// `get_completion` render path.
    #[tokio::test]
    async fn get_context_context_sections_in_order() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let llm = Arc::new(CapturingLlm {
            response_text: "answer".to_string(),
            ..Default::default()
        });
        let retriever = retriever(vector_db, graph_db, Arc::clone(&llm) as Arc<dyn Llm>);

        retriever
            .get_completion(
                "what happened?",
                None,
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        let messages = llm.last_messages.lock().unwrap().clone();
        let user = &messages[1].content;

        // A single contiguous block: passages, then entities, then facts, each
        // section joined to the next by exactly one blank line.
        let expected = format!(
            "## Relevant passages\n{CHUNK_TEXT}\n\n\
             ## Relevant entities\n### {ENTITY_NAME}\n- {BULLET_TEXT}\n\n\
             ## Related facts\n- {FACT_TEXT}"
        );
        assert!(
            user.contains(&expected),
            "user prompt must contain the ordered section block:\n{expected}\n---\nGOT:\n{user}"
        );
    }

    fn retriever_with_template(
        vector_db: Arc<dyn VectorDB>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        user_prompt_template: &str,
    ) -> HybridRetriever {
        HybridRetriever::new(
            vector_db,
            Arc::new(AlignedEmbedding),
            graph_db,
            llm,
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
            Some(user_prompt_template.to_string()),
            None,
        )
    }

    #[test]
    fn lane_top_k_caps_top_k_but_not_an_explicit_knob() {
        // Python `_hybrid_lane_top_k`: explicit knob verbatim, else
        // min(top_k, 10), else the constructor default.
        assert_eq!(super::lane_top_k(Some(25), Some(15), 5), 25);
        assert_eq!(super::lane_top_k(None, Some(15), 5), 10);
        assert_eq!(super::lane_top_k(None, Some(3), 5), 3);
        assert_eq!(super::lane_top_k(None, None, 5), 5);
    }

    #[tokio::test]
    async fn an_empty_graph_is_an_error_not_an_empty_context() {
        let (vector_db, _graph_db, _alice) = populated_dbs().await;
        let empty_graph: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
        let retriever = retriever(vector_db, empty_graph, Arc::new(CapturingLlm::default()));

        let result = retriever
            .get_context("query", &SearchParams::default())
            .await;
        assert!(
            matches!(result, Err(SearchError::NotFound(ref message)) if message.contains("knowledge graph is empty")),
            "got {result:?}"
        );
    }

    #[tokio::test]
    async fn an_empty_context_never_reaches_the_llm() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let llm = Arc::new(CapturingLlm::default());
        let retriever = retriever(vector_db, graph_db, Arc::clone(&llm) as Arc<dyn Llm>);

        let output = retriever
            .get_completion(
                "what happened?",
                Some(Vec::new()),
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();

        assert!(matches!(output, SearchOutput::Texts(ref texts) if texts.is_empty()));
        assert!(llm.last_messages.lock().unwrap().is_empty(), "no LLM call");
    }

    #[tokio::test]
    async fn a_paired_summary_is_carried_but_not_rendered() {
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let chunk_id = Uuid::parse_str(
            vector_db
                .search_similar("DocumentChunk", "text", &[1.0, 0.0], 1)
                .await
                .unwrap()[0]
                .id
                .to_string()
                .as_str(),
        )
        .unwrap();
        const SUMMARY: &str = "a digest the model would copy verbatim";
        vector_db
            .create_collection("TextSummary", "text", 2)
            .await
            .unwrap();
        let summary_id = Uuid::new_v5(&chunk_id, b"TextSummary");
        vector_db
            .index_points(
                "TextSummary",
                "text",
                &[VectorPoint::new(summary_id, vec![1.0, 0.0])
                    .with_metadata("id", json!(summary_id.to_string()))
                    .with_metadata("text", json!(SUMMARY))
                    .with_metadata("source_chunk_id", json!(chunk_id.to_string()))],
            )
            .await
            .unwrap();

        let llm = Arc::new(CapturingLlm {
            response_text: "answer".to_string(),
            ..Default::default()
        });
        let retriever = retriever(vector_db, graph_db, Arc::clone(&llm) as Arc<dyn Llm>);
        let context = retriever
            .get_context("what happened?", &SearchParams::default())
            .await
            .unwrap();
        assert!(
            context
                .iter()
                .any(|item| item.payload.get("chunk_summary") == Some(&json!(SUMMARY))),
            "the summary is paired onto the chunk item"
        );

        retriever
            .get_completion(
                "what happened?",
                Some(context),
                &SessionContext::default(),
                &SearchParams::default(),
            )
            .await
            .unwrap();
        let user = llm.last_messages.lock().unwrap()[1].content.clone();
        // Python SDK-322: raw passages only.
        assert!(!user.contains(SUMMARY), "{user}");
        assert!(!user.contains("[Passage Summary]"), "{user}");
        assert!(
            user.contains(&format!("## Relevant passages\n{CHUNK_TEXT}")),
            "{user}"
        );
    }

    #[tokio::test]
    async fn the_budget_counts_the_template_actually_in_use() {
        // (1000 - 512 reserved) * 3 chars = 1464 chars for the whole prompt.
        // The default template leaves room for this small context; a template
        // that alone costs more than that leaves none, so nothing fits and the
        // empty context is not sent.
        let (vector_db, graph_db, _alice) = populated_dbs().await;
        let small_window = || {
            Arc::new(CapturingLlm {
                response_text: "answer".to_string(),
                max_context: Some(1000),
                ..Default::default()
            })
        };

        let llm = small_window();
        let output = retriever(
            Arc::clone(&vector_db),
            Arc::clone(&graph_db),
            Arc::clone(&llm) as Arc<dyn Llm>,
        )
        .get_completion(
            "what happened?",
            None,
            &SessionContext::default(),
            &SearchParams::default(),
        )
        .await
        .unwrap();
        assert!(matches!(output, SearchOutput::Text(_)));
        assert!(
            llm.last_messages.lock().unwrap()[1]
                .content
                .contains(CHUNK_TEXT)
        );

        let llm = small_window();
        let long_template = format!("{}\n{{question}}\n{{context}}", "x".repeat(2000));
        let output = retriever_with_template(
            vector_db,
            graph_db,
            Arc::clone(&llm) as Arc<dyn Llm>,
            &long_template,
        )
        .get_completion(
            "what happened?",
            None,
            &SessionContext::default(),
            &SearchParams::default(),
        )
        .await
        .unwrap();
        assert!(matches!(output, SearchOutput::Texts(ref texts) if texts.is_empty()));
        assert!(llm.last_messages.lock().unwrap().is_empty());
    }
}

/// Truth-subspace context tests (P2-07).
///
/// Curated port of Python's `test_hybrid_truth_context.py`. These exercise the
/// private `build_truth_context` glue directly, so they live inline rather than
/// in `crates/search/tests/` — Rust integration tests are a separate crate and
/// cannot reach private methods. All mock-based (seeded `MockVectorDB` via
/// `upsert_centroids`, `MockGraphDB::get_node_truth_state`); no credentials.
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod truth_context_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_graph::{GraphDBTrait, MockGraphDB};
    use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmResult, Message, TokenUsage};
    use cognee_truth_subspace::{TruthCentroidPayload, upsert_centroids};
    use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
    use serde_json::json;
    use uuid::Uuid;

    use super::HybridRetriever;

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

    struct NoopLlm;

    #[async_trait]
    impl Llm for NoopLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Ok(GenerationResponse {
                content: String::new(),
                model: "test-model".to_string(),
                usage: Some(TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
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
            Ok(json!({}))
        }

        fn model(&self) -> &str {
            "test-model"
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn truth_retriever(
        vector_db: Arc<dyn VectorDB>,
        graph_db: Arc<dyn GraphDBTrait>,
        use_truth_weight: bool,
    ) -> HybridRetriever {
        HybridRetriever::new(
            vector_db,
            Arc::new(AlignedEmbedding),
            graph_db,
            Arc::new(NoopLlm),
            None,                   // chunks_top_k
            None,                   // entities_top_k
            None,                   // facts_top_k
            None,                   // max_edges_per_entity
            None,                   // text_summaries_top_k
            None,                   // use_importance_weight
            Some(use_truth_weight), // use_truth_weight
            None,                   // include_global_context_index
            None,                   // global_context_index_top_k
            None,                   // node_name
            None,                   // node_name_filter_operator
            None,                   // system_prompt
            None,                   // system_prompt_path
            None,                   // user_prompt_template
            None,                   // generation_options
        )
    }

    /// Seed a single centroid slot for `dataset_id` (dim-2 centroid).
    async fn seed_centroid(db: &MockVectorDB, dataset_id: Uuid, centroid: Vec<f64>, epoch: i64) {
        let payload = TruthCentroidPayload {
            dataset_id: dataset_id.to_string(),
            slot: 0,
            count: 1,
            truth_epoch: epoch,
            updated_at: 0,
            centroid,
            learning_ids: vec![],
        };
        upsert_centroids(db, &[payload]).await.unwrap();
    }

    /// Seed a DocumentChunk_text vector row aligned with the query direction.
    async fn seed_chunk(db: &MockVectorDB, chunk_id: Uuid) {
        db.create_collection("DocumentChunk", "text", 2)
            .await
            .unwrap();
        db.index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint::new(chunk_id, vec![1.0, 0.0])
                .with_metadata("id", json!(chunk_id.to_string()))
                .with_metadata("text", json!("chunk text"))],
        )
        .await
        .unwrap();
    }

    /// Seed `n` DocumentChunk_text rows aligned with the query direction. The
    /// collection is created once; every row shares the query vector so all are
    /// equally rankable and only the candidate-window limit truncates them.
    async fn seed_chunks(db: &MockVectorDB, n: usize) {
        db.create_collection("DocumentChunk", "text", 2)
            .await
            .unwrap();
        for i in 0..n {
            let chunk_id = Uuid::new_v4();
            db.index_points(
                "DocumentChunk",
                "text",
                &[VectorPoint::new(chunk_id, vec![1.0, 0.0])
                    .with_metadata("id", json!(chunk_id.to_string()))
                    .with_metadata("text", json!(format!("chunk text {i}")))],
            )
            .await
            .unwrap();
        }
    }

    const QUERY_VECTOR: [f32; 2] = [1.0, 0.0];

    #[tokio::test]
    async fn happy_path_loads_centroid_slot_and_truth_state() {
        let dataset_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();

        let vdb = MockVectorDB::new();
        seed_centroid(&vdb, dataset_id, vec![1.0, 0.0], 3).await;
        seed_chunk(&vdb, chunk_id).await;
        let vector_db: Arc<dyn VectorDB> = Arc::new(vdb);

        let graph = MockGraphDB::new();
        graph
            .add_node_raw(json!({
                "id": chunk_id.to_string(),
                "truth_alignment": [1.0],
                "truth_epoch": 3,
            }))
            .await
            .unwrap();
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(graph);

        let retriever = truth_retriever(vector_db, graph_db, true);
        let (q_coords, truth_state_by_id, current_truth_epoch) = retriever
            .build_truth_context(
                true,
                super::DEFAULT_TOP_K,
                &QUERY_VECTOR,
                Some(dataset_id),
                None,
                "OR",
            )
            .await;

        // 1-centroid basis, DEFAULT_K = 8: cosine([1,0],[1,0]) = 1.0, padded.
        assert_eq!(q_coords, Some(vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]));
        assert_eq!(current_truth_epoch, Some(3));
        let states = truth_state_by_id.expect("truth state present");
        let state = states.get(&chunk_id.to_string()).expect("chunk state");
        assert_eq!(state.truth_epoch, 3);
        assert_eq!(state.truth_alignment, vec![1.0]);
    }

    #[tokio::test]
    async fn fail_open_when_use_truth_weight_off() {
        let dataset_id = Uuid::new_v4();
        let vdb = MockVectorDB::new();
        seed_centroid(&vdb, dataset_id, vec![1.0, 0.0], 3).await;
        let vector_db: Arc<dyn VectorDB> = Arc::new(vdb);

        // Concrete clone shares the call log (Arc-backed) with the dyn handle.
        let graph = MockGraphDB::new();
        let graph_probe = graph.clone();
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(graph);

        let retriever = truth_retriever(vector_db, graph_db, false);
        let result = retriever
            .build_truth_context(
                false,
                super::DEFAULT_TOP_K,
                &QUERY_VECTOR,
                Some(dataset_id),
                None,
                "OR",
            )
            .await;
        assert_eq!(result, (None, None, None));
        // Short-circuits before touching the graph at all.
        assert!(
            !graph_probe
                .get_call_log()
                .contains(&"get_node_truth_state".to_string()),
            "graph must not be queried when the knob is off"
        );
    }

    #[tokio::test]
    async fn fail_open_when_no_single_dataset() {
        let vdb = MockVectorDB::new();
        let vector_db: Arc<dyn VectorDB> = Arc::new(vdb);
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());

        let retriever = truth_retriever(vector_db, graph_db, true);
        let result = retriever
            .build_truth_context(true, super::DEFAULT_TOP_K, &QUERY_VECTOR, None, None, "OR")
            .await;
        assert_eq!(result, (None, None, None));
    }

    #[tokio::test]
    async fn fail_open_when_no_centroids() {
        let dataset_id = Uuid::new_v4();
        // No centroids seeded -> load_centroids returns empty -> Ok(None).
        let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());

        let retriever = truth_retriever(vector_db, graph_db, true);
        let result = retriever
            .build_truth_context(
                true,
                super::DEFAULT_TOP_K,
                &QUERY_VECTOR,
                Some(dataset_id),
                None,
                "OR",
            )
            .await;
        assert_eq!(result, (None, None, None));
    }

    #[tokio::test]
    async fn fail_open_on_vector_store_error() {
        let dataset_id = Uuid::new_v4();
        let vdb = MockVectorDB::new();
        // Centroid load goes through `retrieve`, which we make error.
        vdb.set_retrieve_error("boom");
        let vector_db: Arc<dyn VectorDB> = Arc::new(vdb);
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());

        let retriever = truth_retriever(vector_db, graph_db, true);
        let result = retriever
            .build_truth_context(
                true,
                super::DEFAULT_TOP_K,
                &QUERY_VECTOR,
                Some(dataset_id),
                None,
                "OR",
            )
            .await;
        assert_eq!(result, (None, None, None));
    }

    #[tokio::test]
    async fn fail_open_on_graph_error_discards_computed_q_coords() {
        // Centroids + candidate chunk load fine, so q_coords/epoch are already
        // computed; the graph batch call errors. The whole chain must still
        // fail open to (None, None, None) — the single-fail-open-boundary.
        let dataset_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();

        let vdb = MockVectorDB::new();
        seed_centroid(&vdb, dataset_id, vec![1.0, 0.0], 3).await;
        seed_chunk(&vdb, chunk_id).await;
        let vector_db: Arc<dyn VectorDB> = Arc::new(vdb);

        let graph = MockGraphDB::new();
        graph
            .add_node_raw(json!({ "id": chunk_id.to_string(), "truth_epoch": 3 }))
            .await
            .unwrap();
        graph.set_truth_state_error("graph down");
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(graph);

        let retriever = truth_retriever(vector_db, graph_db, true);
        let result = retriever
            .build_truth_context(
                true,
                super::DEFAULT_TOP_K,
                &QUERY_VECTOR,
                Some(dataset_id),
                None,
                "OR",
            )
            .await;
        assert_eq!(result, (None, None, None));
    }

    #[tokio::test]
    async fn candidate_empty_yields_populated_coords_and_empty_states() {
        // Centroids present, but no DocumentChunk collection -> no candidates.
        // q_coords/epoch stay Some; the state map is Some but empty.
        let dataset_id = Uuid::new_v4();
        let vdb = MockVectorDB::new();
        seed_centroid(&vdb, dataset_id, vec![1.0, 0.0], 3).await;
        let vector_db: Arc<dyn VectorDB> = Arc::new(vdb);
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());

        let retriever = truth_retriever(vector_db, graph_db, true);
        let (q_coords, truth_state_by_id, current_truth_epoch) = retriever
            .build_truth_context(
                true,
                super::DEFAULT_TOP_K,
                &QUERY_VECTOR,
                Some(dataset_id),
                None,
                "OR",
            )
            .await;

        assert_eq!(q_coords, Some(vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]));
        assert_eq!(current_truth_epoch, Some(3));
        let states = truth_state_by_id.expect("empty-but-present state map");
        assert!(states.is_empty());
    }

    /// The truth candidate window must be sized from the request-resolved
    /// `chunks_top_k` threaded down from `get_context`, not the constructor
    /// default. With 5 equally-ranked chunks seeded, a resolved `chunks_top_k`
    /// of 1 caps the window at `1 * 2 = 2` candidates, while the constructor
    /// default (15) would surface all 5 — proving the request override drives
    /// the window in lockstep with the chunk lane.
    #[tokio::test]
    async fn candidate_window_follows_request_resolved_chunks_top_k() {
        let vdb = MockVectorDB::new();
        seed_chunks(&vdb, 5).await;
        let vector_db: Arc<dyn VectorDB> = Arc::new(vdb);
        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());

        // Constructor default is DEFAULT_TOP_K (15); the request-resolved value
        // passed in is what the window must follow.
        let retriever = truth_retriever(vector_db, graph_db, true);

        // Resolved chunks_top_k = 1 -> window = 2 -> at most 2 candidate ids.
        let narrow = retriever
            .candidate_chunk_ids(1, &QUERY_VECTOR, None, "OR")
            .await
            .unwrap();
        assert_eq!(
            narrow.len(),
            2,
            "window must follow the resolved chunks_top_k (1 * 2), not self.chunks_top_k"
        );

        // A wide resolved value surfaces every seeded chunk (window 30 >= 5).
        let wide = retriever
            .candidate_chunk_ids(super::DEFAULT_TOP_K, &QUERY_VECTOR, None, "OR")
            .await
            .unwrap();
        assert_eq!(
            wide.len(),
            5,
            "wide window should surface all seeded chunks"
        );
    }
}
