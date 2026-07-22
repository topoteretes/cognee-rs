use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::{GraphDBTrait, NodeData};
use cognee_llm::{GenerationOptions, Llm, LlmExt, Message};
use cognee_models::{RawExtractedTimestamp, to_cognify_timestamp};
use cognee_vector::VectorDB;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use cognee_session::SessionContext;

use crate::graph_retrieval::{
    DEFAULT_TRIPLET_DISTANCE_PENALTY, GraphRetrievalConfig, RankedGraphEdge,
    brute_force_triplet_search,
};
use crate::retrievers::SearchRetriever;
use crate::types::{
    SearchContext, SearchError, SearchItem, SearchOutput, SearchParams, SearchType,
};
use crate::utils::{build_messages_with_history, render_graph_user_prompt, resolve_system_prompt};

const DEFAULT_TOP_K: usize = 10;
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;
const TEMPORAL_DATA_TYPE: &str = "Event";
const TEMPORAL_FIELD_NAME: &str = "name";
const DEFAULT_TEMPORAL_INTERVAL_PROMPT: &str = "You are tasked with identifying relevant time periods where the answer to a given query should be searched.\nCurrent date is:  `{time_now}`. Determine relevant period(s) and return structured intervals.\n\nExtraction rules:\n\n1. Query without specific timestamp: use the time period with starts_at set to None and ends_at set to now.\n2. Explicit time intervals: If the query specifies a range (e.g., from 2010 to 2020, between January and March 2023), extract both start and end dates. Always assign the earlier date to starts_at and the later date to ends_at.\n3. Single timestamp: If the query refers to one specific moment (e.g., in 2015, on March 5, 2022), set starts_at and ends_at to that same timestamp.\n4. Open-ended time references: For phrases such as \"before X\" or \"after X\", represent the unspecified side as None. For example: before 2009 → starts_at: None, ends_at: 2009; after 2009 → starts_at: 2009, ends_at: None.\n5. Current-time references (\"now\", \"current\", \"today\"): If the query explicitly refers to the present, set both starts_at and ends_at to now (the ingestion timestamp).\n6. \"Who is\" and \"Who was\" questions: These imply a general identity or biographical inquiry without a specific temporal scope. Set both starts_at and ends_at to None.\n7. Ordering rule: Always ensure the earlier date is assigned to starts_at and the later date to ends_at.\n8. No temporal information: If no valid or inferable time reference is found, set both starts_at and ends_at to None.";

/// The interval extracted from a query by the LLM.
///
/// Each bound reuses [`RawExtractedTimestamp`] from `cognee-models` — the same
/// structured model the temporal cognify pipeline extracts into (and a faithful
/// port of Python cognee's `Timestamp`, `cognee/tasks/temporal_graph/models.py`).
/// Its `year` field is **required** while the finer components default (month/day
/// to 1, time to 0). A required integer `year` is what reliably drives the LLM to
/// extract a concrete date even for a coarse query such as "in 2021": the earlier
/// free-form `Option<String>` schema let the model return `null` for the whole
/// bound on broad year queries, which routed them down the empty triplet-fallback
/// path (all events dropped), while narrower month-scoped queries happened to
/// extract cleanly.
///
/// Unspecified finer components resolve to the *start* of their unit (a date-only
/// bound → 00:00:00, a single-moment query where `starts_at == ends_at` → one
/// instant). This is intentional parity with Python cognee, whose `Timestamp`
/// applies the same 1/0 defaults and whose `date_to_int` performs no end-of-period
/// expansion; it is deliberately not "widened" to end-of-day here so the two SDKs
/// return matching event sets.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct QueryInterval {
    starts_at: Option<RawExtractedTimestamp>,
    ends_at: Option<RawExtractedTimestamp>,
}

/// Millisecond bounds (epoch UTC) of an extracted interval; `None` on a side
/// means that bound is open.
#[derive(Debug, Clone)]
struct ParsedInterval {
    start: Option<i64>,
    end: Option<i64>,
}

impl QueryInterval {
    /// Convert the extracted bounds to millisecond epoch bounds.
    ///
    /// Returns `None` — signalling "no usable interval, fall back to triplet
    /// search" — in two cases: no bound was provided at all (both sides absent),
    /// or a *provided* bound is not a valid calendar date (e.g. a hallucinated
    /// `2024-02-30`). A single-sided interval (only `starts_at` or only `ends_at`)
    /// is kept. Discarding the whole interval on an impossible date mirrors Python
    /// (where `date_to_int` raises) and, crucially, avoids silently dropping one
    /// side and widening a bounded query into an open-ended scan of all history.
    fn into_millis_interval(self) -> Option<ParsedInterval> {
        let start = match self.starts_at {
            Some(ts) => Some(to_cognify_timestamp(ts)?.time_at),
            None => None,
        };
        let end = match self.ends_at {
            Some(ts) => Some(to_cognify_timestamp(ts)?.time_at),
            None => None,
        };

        if start.is_none() && end.is_none() {
            return None;
        }

        Some(ParsedInterval { start, end })
    }
}

pub struct TemporalRetriever {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    graph_db: Arc<dyn GraphDBTrait>,
    llm: Arc<dyn Llm>,
    top_k: usize,
    wide_search_top_k: usize,
    triplet_distance_penalty: f32,
    feedback_influence: f32,
    temporal_interval_prompt: Option<String>,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

impl TemporalRetriever {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        top_k: Option<usize>,
        wide_search_top_k: Option<usize>,
        triplet_distance_penalty: Option<f32>,
        temporal_interval_prompt: Option<String>,
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
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            wide_search_top_k: wide_search_top_k.unwrap_or(DEFAULT_WIDE_SEARCH_TOP_K),
            triplet_distance_penalty: triplet_distance_penalty
                .unwrap_or(DEFAULT_TRIPLET_DISTANCE_PENALTY),
            feedback_influence: 0.0,
            temporal_interval_prompt,
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }

    async fn extract_interval(&self, query: &str) -> Result<Option<ParsedInterval>, SearchError> {
        let now = chrono::Local::now().format("%d-%m-%Y").to_string();
        let prompt_template = self
            .temporal_interval_prompt
            .as_deref()
            .unwrap_or(DEFAULT_TEMPORAL_INTERVAL_PROMPT);
        let system_prompt = prompt_template.replace("{time_now}", &now);

        let interval = match self
            .llm
            .create_structured_output_with_messages::<QueryInterval>(
                vec![
                    Message::system(system_prompt),
                    Message::user(query.to_string()),
                ],
                self.generation_options.clone(),
            )
            .await
        {
            Ok(interval) => interval,
            Err(_) => return Ok(None),
        };

        // `None` here means "no usable interval" (no bound, or a provided bound
        // was an impossible date) — degrade to the triplet fallback.
        Ok(interval.into_millis_interval())
    }

    fn get_graph_retrieval_config(&self, params: &SearchParams) -> GraphRetrievalConfig {
        GraphRetrievalConfig {
            top_k: params.top_k_or(self.top_k),
            wide_search_top_k: params.wide_search_top_k_or(self.wide_search_top_k),
            triplet_distance_penalty: params
                .triplet_distance_penalty_or(self.triplet_distance_penalty),
            feedback_influence: params.feedback_influence_or(self.feedback_influence),
            node_type: params.node_type.clone(),
            node_name: params.node_name.clone(),
            node_name_filter_operator: params
                .node_name_filter_operator
                .as_deref()
                .unwrap_or("OR")
                .to_string(),
        }
    }

    async fn get_ranked_graph_edges(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<Vec<RankedGraphEdge>, SearchError> {
        brute_force_triplet_search(
            query,
            self.vector_db.as_ref(),
            self.embedding_engine.as_ref(),
            self.graph_db.as_ref(),
            &self.get_graph_retrieval_config(params),
        )
        .await
    }

    fn ranked_edges_to_context(ranked_edges: Vec<RankedGraphEdge>) -> SearchContext {
        ranked_edges
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
            .collect()
    }

    async fn get_fallback_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        let ranked_edges = self.get_ranked_graph_edges(query, params).await?;
        Ok(Self::ranked_edges_to_context(ranked_edges))
    }

    async fn rank_temporal_events(
        &self,
        query: &str,
        event_ids: &HashSet<String>,
        ranked_edges: &[RankedGraphEdge],
    ) -> Result<Vec<(String, f32)>, SearchError> {
        let mut scores = HashMap::<String, f32>::new();

        for edge in ranked_edges {
            if event_ids.contains(&edge.source_id) {
                let score = scores.entry(edge.source_id.clone()).or_insert(edge.score);
                *score = score.max(edge.score);
            }
            if event_ids.contains(&edge.target_id) {
                let score = scores.entry(edge.target_id.clone()).or_insert(edge.score);
                *score = score.max(edge.score);
            }
        }

        if self
            .vector_db
            .has_collection(TEMPORAL_DATA_TYPE, TEMPORAL_FIELD_NAME)
            .await?
        {
            let query_embeddings = self.embedding_engine.embed(&[query]).await?;
            let query_vector = query_embeddings.into_iter().next().ok_or_else(|| {
                SearchError::InvalidInput("embedding engine returned no vectors".to_string())
            })?;

            let semantic_results = self
                .vector_db
                .search_similar(
                    TEMPORAL_DATA_TYPE,
                    TEMPORAL_FIELD_NAME,
                    &query_vector,
                    self.wide_search_top_k.max(self.top_k),
                )
                .await?;

            for result in semantic_results {
                let event_id = result.id.to_string();
                if !event_ids.contains(&event_id) {
                    continue;
                }

                let score = scores.entry(event_id).or_insert(result.score);
                *score = score.max(result.score);
            }
        }

        let mut ranked = event_ids
            .iter()
            .map(|event_id| {
                (
                    event_id.clone(),
                    scores.get(event_id).copied().unwrap_or(0.0),
                )
            })
            .collect::<Vec<_>>();

        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.0.cmp(&right.0))
        });

        Ok(ranked)
    }

    fn temporal_context_to_text(context: &SearchContext) -> String {
        context
            .iter()
            .map(|item| {
                if item.payload.get("event_id").is_some() {
                    let name = item
                        .payload
                        .get("event_name")
                        .and_then(Value::as_str)
                        .unwrap_or("Unnamed event");
                    let description = item
                        .payload
                        .get("event_description")
                        .and_then(Value::as_str)
                        .unwrap_or("No description");
                    let time = item
                        .payload
                        .get("event_time")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown time");

                    return format!("{name} ({time}): {description}");
                }

                let source = item
                    .payload
                    .get("source_name")
                    .and_then(Value::as_str)
                    .or_else(|| item.payload.get("source_id").and_then(Value::as_str))
                    .unwrap_or("unknown_source");
                let target = item
                    .payload
                    .get("target_name")
                    .and_then(Value::as_str)
                    .or_else(|| item.payload.get("target_id").and_then(Value::as_str))
                    .unwrap_or("unknown_target");
                let relationship = item
                    .payload
                    .get("relationship")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        item.payload
                            .get("relationship_name")
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("related_to");

                format!("{source} -[{relationship}]-> {target}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
impl SearchRetriever for TemporalRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::Temporal
    }

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        if self.graph_db.is_empty().await? {
            return Ok(vec![]);
        }

        let Some(interval) = self.extract_interval(query).await? else {
            return self.get_fallback_context(query, params).await;
        };

        // Fix 1: Use typed query to find Timestamp nodes instead of full graph scan.
        let (candidate_timestamps, _) = self
            .graph_db
            .get_filtered_graph_data(&HashMap::from([(
                Cow::Borrowed("type"),
                vec![json!("Timestamp")],
            )]))
            .await?;

        let interval_from_ms = interval.start;
        let interval_to_ms = interval.end;

        let matching_ts_ids: Vec<String> = candidate_timestamps
            .into_iter()
            .filter_map(|(id, props)| {
                let time_at = props.get("time_at")?.as_i64()?;
                is_within_interval_ms(time_at, interval_from_ms, interval_to_ms).then_some(id)
            })
            .collect();

        // Fix 2: Collect Event nodes reachable within 1-2 hops from matching Timestamps.
        let mut event_node_ids = HashSet::new();
        for ts_id in &matching_ts_ids {
            for node_props in self.graph_db.get_neighbors(ts_id).await? {
                let node_type = node_props.get("type").and_then(|v| v.as_str());
                match node_type {
                    Some("Event") => {
                        if let Some(id) = node_props.get("id").and_then(|v| v.as_str()) {
                            event_node_ids.insert(id.to_string());
                        }
                    }
                    Some("Interval") => {
                        // Hop through Interval node to reach Event nodes (hop 2).
                        if let Some(interval_id) = node_props.get("id").and_then(|v| v.as_str()) {
                            for inner_props in self.graph_db.get_neighbors(interval_id).await? {
                                if inner_props.get("type").and_then(|v| v.as_str()) == Some("Event")
                                    && let Some(id) = inner_props.get("id").and_then(|v| v.as_str())
                                {
                                    event_node_ids.insert(id.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if event_node_ids.is_empty() {
            return self.get_fallback_context(query, params).await;
        }

        let ranked_edges = self.get_ranked_graph_edges(query, params).await?;
        let ranked_events = self
            .rank_temporal_events(query, &event_node_ids, &ranked_edges)
            .await?;

        // Fetch Event nodes by ID for building the context payload.
        let event_id_list: Vec<String> = ranked_events
            .iter()
            .take(params.top_k_or(self.top_k))
            .map(|(id, _)| id.clone())
            .collect();
        let event_nodes = self.graph_db.get_nodes(&event_id_list).await?;
        let nodes_by_id: HashMap<String, NodeData> =
            event_id_list.into_iter().zip(event_nodes).collect();

        let mut temporal_context = Vec::new();

        for (event_id, score) in ranked_events.into_iter().take(params.top_k_or(self.top_k)) {
            let Some(event_node) = nodes_by_id.get(&event_id) else {
                continue;
            };

            temporal_context.push(SearchItem {
                id: None,
                score: Some(score),
                payload: json!({
                    "event_id": event_id,
                    "event_name": extract_node_name(event_node),
                    "event_description": extract_node_description(event_node),
                }),
            });
        }

        if temporal_context.is_empty() {
            return Ok(Self::ranked_edges_to_context(ranked_edges));
        }

        Ok(temporal_context)
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

        let user_prompt = render_graph_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &Self::temporal_context_to_text(&completion_context),
        );

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
}

fn extract_node_name(node_data: &NodeData) -> String {
    node_data
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| node_data.get("title").and_then(Value::as_str))
        .unwrap_or("Unnamed event")
        .to_string()
}

fn extract_node_description(node_data: &NodeData) -> String {
    node_data
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| node_data.get("text").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

// Fix 3: millisecond-based interval check for Timestamp nodes.
fn is_within_interval_ms(time_at_ms: i64, from_ms: Option<i64>, to_ms: Option<i64>) -> bool {
    from_ms.is_none_or(|from| time_at_ms >= from) && to_ms.is_none_or(|to| time_at_ms <= to)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::borrow::Cow;
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_graph::{EdgeData, GraphDBResult, GraphDBTrait, GraphNode, NodeData};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};

    use chrono::{TimeZone, Utc};
    use serde_json::{Value, json};
    use uuid::Uuid;

    use cognee_session::SessionContext;

    use super::{QueryInterval, TemporalRetriever};
    use crate::graph_retrieval::RankedGraphEdge;
    use crate::retrievers::SearchRetriever;
    use crate::types::{SearchItem, SearchOutput, SearchParams};
    use cognee_models::RawExtractedTimestamp;

    struct TestEmbeddingEngine;

    #[async_trait]
    impl EmbeddingEngine for TestEmbeddingEngine {
        async fn embed(&self, _texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
            Ok(vec![vec![0.3, 0.7]])
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
            Ok(self
                .collections
                .get(&Self::key(data_type, field_name))
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

    struct TestGraphDb {
        nodes: Vec<GraphNode>,
        edges: Vec<EdgeData>,
        /// Maps node_id -> list of neighbor NodeData returned by get_neighbors.
        neighbors: HashMap<String, Vec<NodeData>>,
    }

    #[async_trait]
    impl GraphDBTrait for TestGraphDb {
        async fn initialize(&self) -> GraphDBResult<()> {
            Ok(())
        }

        async fn is_empty(&self) -> GraphDBResult<bool> {
            Ok(self.nodes.is_empty())
        }

        async fn query(
            &self,
            _query: &str,
            _params: Option<HashMap<Cow<'static, str>, Value>>,
        ) -> GraphDBResult<Vec<Vec<Value>>> {
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

        async fn get_nodes(&self, node_ids: &[String]) -> GraphDBResult<Vec<NodeData>> {
            let nodes_map: HashMap<&str, &NodeData> = self
                .nodes
                .iter()
                .map(|(id, data)| (id.as_str(), data))
                .collect();
            Ok(node_ids
                .iter()
                .filter_map(|id| nodes_map.get(id.as_str()).map(|d| (*d).clone()))
                .collect())
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
            _properties: Option<HashMap<Cow<'static, str>, Value>>,
        ) -> GraphDBResult<()> {
            Ok(())
        }

        async fn add_edges(&self, _edges: &[EdgeData]) -> GraphDBResult<()> {
            Ok(())
        }

        async fn get_edges(&self, _node_id: &str) -> GraphDBResult<Vec<EdgeData>> {
            Ok(vec![])
        }

        async fn get_neighbors(&self, node_id: &str) -> GraphDBResult<Vec<NodeData>> {
            Ok(self.neighbors.get(node_id).cloned().unwrap_or_default())
        }

        async fn get_connections(
            &self,
            _node_id: &str,
        ) -> GraphDBResult<Vec<(NodeData, HashMap<Cow<'static, str>, Value>, NodeData)>> {
            Ok(vec![])
        }

        async fn get_graph_data(&self) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok((self.nodes.clone(), self.edges.clone()))
        }

        async fn get_graph_metrics(
            &self,
            _include_optional: bool,
        ) -> GraphDBResult<HashMap<Cow<'static, str>, Value>> {
            Ok(HashMap::new())
        }

        async fn get_filtered_graph_data(
            &self,
            _attribute_filters: &HashMap<Cow<'static, str>, Vec<Value>>,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok((self.nodes.clone(), self.edges.clone()))
        }

        async fn get_nodeset_subgraph(
            &self,
            _node_type: &str,
            _node_names: &[String],
            _node_name_filter_operator: &str,
        ) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
            Ok((self.nodes.clone(), self.edges.clone()))
        }
    }

    #[derive(Default)]
    struct TestLlm {
        completion_response: String,
        interval_response: Option<QueryInterval>,
        fail_structured_output: bool,
        last_messages: Mutex<Vec<Message>>,
        /// When set, `create_structured_output_with_messages_raw` returns this
        /// value instead of serializing `interval_response`. Used by tests that
        /// exercise the response_schema path in `get_completion`.
        structured_completion_response: Mutex<Option<Value>>,
        /// Messages captured by the most recent `create_structured_output_with_messages_raw` call.
        last_structured_messages: Mutex<Vec<Message>>,
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
                content: self.completion_response.clone(),
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
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<serde_json::Value> {
            self.last_structured_messages
                .lock()
                .unwrap()
                .clone_from(&messages);

            if self.fail_structured_output {
                return Err(LlmError::ConfigError("forced failure".to_string()));
            }

            // If a custom structured completion response is set, return it.
            if let Some(value) = self.structured_completion_response.lock().unwrap().clone() {
                return Ok(value);
            }

            let response = self
                .interval_response
                .clone()
                .ok_or_else(|| LlmError::ConfigError("missing interval response".to_string()))?;

            serde_json::to_value(response).map_err(|error| LlmError::ConfigError(error.to_string()))
        }

        fn model(&self) -> &str {
            "test-model"
        }
    }

    /// Build a `RawExtractedTimestamp` for tests (date-only; time is 00:00:00).
    fn qts(year: u16, month: u8, day: u8) -> RawExtractedTimestamp {
        RawExtractedTimestamp {
            year,
            month,
            day,
            hour: 0,
            minute: 0,
            second: 0,
        }
    }

    /// Epoch-millis for a UTC date at 00:00:00, for asserting interval bounds.
    fn millis_at(year: i32, month: u32, day: u32) -> i64 {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0)
            .unwrap()
            .timestamp_millis()
    }

    fn event_node_data(id: &str, name: &str) -> NodeData {
        HashMap::from([
            (Cow::Borrowed("id"), json!(id)),
            (Cow::Borrowed("name"), json!(name)),
            (Cow::Borrowed("type"), json!("Event")),
            (
                Cow::Borrowed("description"),
                json!(format!("Description for {name}")),
            ),
        ])
    }

    fn timestamp_node(id: &str, time_at_ms: i64) -> GraphNode {
        (
            id.to_string(),
            HashMap::from([
                (Cow::Borrowed("id"), json!(id)),
                (Cow::Borrowed("type"), json!("Timestamp")),
                (Cow::Borrowed("time_at"), json!(time_at_ms)),
            ]),
        )
    }

    fn event_graph_node(id: &str, name: &str) -> GraphNode {
        (id.to_string(), event_node_data(id, name))
    }

    #[tokio::test]
    async fn returns_temporal_event_context_when_interval_matches() {
        // 2024-03-15 00:00:00 UTC in milliseconds
        let launch_event_ms: i64 = 1710460800000;
        // 2020-01-10 00:00:00 UTC in milliseconds
        let old_event_ms: i64 = 1578614400000;

        let ts_in_2024 = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        let ts_in_2020 = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        let event_launch = "11111111-1111-1111-1111-111111111111";
        let event_old = "22222222-2222-2222-2222-222222222222";

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([
                (
                    TestVectorDb::key("Entity", "name"),
                    vec![SearchResult {
                        id: uuid::Uuid::new_v4(),
                        score: 0.8,
                        metadata: HashMap::from([(String::from("type"), json!("entity"))]),
                    }],
                ),
                (
                    TestVectorDb::key("Event", "name"),
                    vec![SearchResult {
                        id: uuid::Uuid::parse_str(event_launch).unwrap(),
                        score: 0.95,
                        metadata: HashMap::new(),
                    }],
                ),
            ]),
        });

        let embedding_engine = Arc::new(TestEmbeddingEngine);
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                timestamp_node(ts_in_2024, launch_event_ms),
                timestamp_node(ts_in_2020, old_event_ms),
                event_graph_node(event_launch, "Launch event"),
                event_graph_node(event_old, "Old event"),
            ],
            edges: vec![
                (
                    event_launch.to_string(),
                    ts_in_2024.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
                (
                    event_old.to_string(),
                    ts_in_2020.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
            ],
            neighbors: HashMap::from([
                // The 2024 Timestamp node has the Launch event as a neighbor.
                (
                    ts_in_2024.to_string(),
                    vec![event_node_data(event_launch, "Launch event")],
                ),
                // The 2020 Timestamp node has the Old event as a neighbor.
                (
                    ts_in_2020.to_string(),
                    vec![event_node_data(event_old, "Old event")],
                ),
            ]),
        });
        let llm = Arc::new(TestLlm {
            completion_response: "temporal answer".to_string(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2024, 1, 1)),
                ends_at: Some(qts(2024, 12, 31)),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            structured_completion_response: Mutex::new(None),
            last_structured_messages: Mutex::new(vec![]),
        });

        let retriever = TemporalRetriever::new(
            vector_db,
            embedding_engine,
            graph_db,
            llm,
            Some(5),
            Some(10),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        );

        let context = retriever
            .get_context("What happened in 2024?", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 1);
        assert_eq!(
            context[0].payload.get("event_name").and_then(Value::as_str),
            Some("Launch event")
        );
    }

    // ── QueryInterval::into_millis_interval tests ───────────────────────

    #[test]
    fn year_only_bound_deserializes_and_defaults_to_january_first() {
        // The coarse-query path the whole fix hinges on: the LLM returns only a
        // `year`, and serde must default month/day to 1 (not 0, which would make
        // the date invalid) and time to 0. Exercised via real JSON deserialization
        // so a regression in the `#[serde(default …)]` attributes is caught here.
        let qi: QueryInterval =
            serde_json::from_value(json!({ "starts_at": { "year": 2021 }, "ends_at": null }))
                .unwrap();
        let parsed = qi
            .into_millis_interval()
            .expect("a year-only start bound is a usable interval");
        assert_eq!(parsed.start, Some(millis_at(2021, 1, 1)));
        assert_eq!(parsed.end, None);
    }

    #[test]
    fn invalid_present_bound_discards_whole_interval() {
        // A hallucinated impossible date (Feb 30) on one side must NOT silently
        // collapse to a half-open range (which would widen the search to all of
        // history); the entire interval is discarded so the caller falls back to
        // triplet search.
        let qi = QueryInterval {
            starts_at: Some(RawExtractedTimestamp {
                year: 2024,
                month: 2,
                day: 30,
                hour: 0,
                minute: 0,
                second: 0,
            }),
            ends_at: Some(qts(2024, 3, 15)),
        };
        assert!(qi.into_millis_interval().is_none());
    }

    #[test]
    fn both_bounds_none_yields_no_interval() {
        let qi = QueryInterval {
            starts_at: None,
            ends_at: None,
        };
        assert!(qi.into_millis_interval().is_none());
    }

    // ── is_within_interval_ms tests ───────────────────────────────────

    #[test]
    fn is_within_interval_ms_basic_cases() {
        use super::is_within_interval_ms;

        // In range
        assert!(is_within_interval_ms(500, Some(100), Some(1000)));
        // At lower boundary (inclusive)
        assert!(is_within_interval_ms(100, Some(100), Some(1000)));
        // At upper boundary (inclusive)
        assert!(is_within_interval_ms(1000, Some(100), Some(1000)));
        // Below range
        assert!(!is_within_interval_ms(50, Some(100), Some(1000)));
        // Above range
        assert!(!is_within_interval_ms(1500, Some(100), Some(1000)));
    }

    #[test]
    fn is_within_interval_ms_open_ended_bounds() {
        use super::is_within_interval_ms;

        // No lower bound (open start)
        assert!(is_within_interval_ms(50, None, Some(1000)));
        assert!(!is_within_interval_ms(1500, None, Some(1000)));

        // No upper bound (open end)
        assert!(is_within_interval_ms(1500, Some(100), None));
        assert!(!is_within_interval_ms(50, Some(100), None));

        // Both bounds None (everything matches)
        assert!(is_within_interval_ms(0, None, None));
        assert!(is_within_interval_ms(i64::MAX, None, None));
        assert!(is_within_interval_ms(i64::MIN, None, None));
    }

    // ── QueryInterval::parse tests ────────────────────────────────────

    #[test]
    fn into_millis_interval_both_bounds() {
        let qi = QueryInterval {
            starts_at: Some(qts(2024, 1, 1)),
            ends_at: Some(qts(2024, 12, 31)),
        };
        let parsed = qi.into_millis_interval().expect("both bounds present");
        assert_eq!(parsed.start, Some(millis_at(2024, 1, 1)));
        assert_eq!(parsed.end, Some(millis_at(2024, 12, 31)));
    }

    #[test]
    fn into_millis_interval_partial_bounds() {
        // Only starts_at
        let qi = QueryInterval {
            starts_at: Some(qts(2024, 6, 1)),
            ends_at: None,
        };
        let parsed = qi.into_millis_interval().expect("start bound present");
        assert_eq!(parsed.start, Some(millis_at(2024, 6, 1)));
        assert!(parsed.end.is_none());

        // Only ends_at
        let qi = QueryInterval {
            starts_at: None,
            ends_at: Some(qts(2024, 12, 31)),
        };
        let parsed = qi.into_millis_interval().expect("end bound present");
        assert!(parsed.start.is_none());
        assert_eq!(parsed.end, Some(millis_at(2024, 12, 31)));
    }

    #[tokio::test]
    async fn falls_back_to_graph_context_when_interval_extraction_fails() {
        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Entity", "name"),
                vec![SearchResult {
                    id: uuid::Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
                    score: 0.9,
                    metadata: HashMap::new(),
                }],
            )]),
        });

        let embedding_engine = Arc::new(TestEmbeddingEngine);
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                (
                    "33333333-3333-3333-3333-333333333333".to_string(),
                    HashMap::from([
                        (
                            Cow::Borrowed("id"),
                            json!("33333333-3333-3333-3333-333333333333"),
                        ),
                        (Cow::Borrowed("name"), json!("Entity A")),
                    ]),
                ),
                (
                    "44444444-4444-4444-4444-444444444444".to_string(),
                    HashMap::from([
                        (
                            Cow::Borrowed("id"),
                            json!("44444444-4444-4444-4444-444444444444"),
                        ),
                        (Cow::Borrowed("name"), json!("Entity B")),
                    ]),
                ),
            ],
            edges: vec![(
                "33333333-3333-3333-3333-333333333333".to_string(),
                "44444444-4444-4444-4444-444444444444".to_string(),
                "connected_to".to_string(),
                HashMap::new(),
            )],
            neighbors: HashMap::new(),
        });
        let llm = Arc::new(TestLlm {
            completion_response: "fallback answer".to_string(),
            interval_response: None,
            fail_structured_output: true,
            last_messages: Mutex::new(vec![]),
            structured_completion_response: Mutex::new(None),
            last_structured_messages: Mutex::new(vec![]),
        });

        let retriever = TemporalRetriever::new(
            vector_db,
            embedding_engine,
            graph_db,
            llm,
            Some(3),
            Some(10),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        );

        let context = retriever
            .get_context("What happened?", &SearchParams::default())
            .await
            .unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(
            context[0]
                .payload
                .get("relationship")
                .and_then(Value::as_str),
            Some("connected_to")
        );
    }

    fn build_retriever_with_llm(llm: TestLlm) -> TemporalRetriever {
        TemporalRetriever::new(
            Arc::new(TestVectorDb {
                collections: HashMap::new(),
            }),
            Arc::new(TestEmbeddingEngine),
            Arc::new(TestGraphDb {
                nodes: vec![],
                edges: vec![],
                neighbors: HashMap::new(),
            }),
            Arc::new(llm),
            Some(5),
            Some(10),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn extract_interval_returns_parsed_interval_from_llm() {
        let llm = TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2024, 1, 1)),
                ends_at: Some(qts(2024, 12, 31)),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        };
        let retriever = build_retriever_with_llm(llm);

        let result = retriever
            .extract_interval("What happened in 2024?")
            .await
            .unwrap();

        let parsed = result.expect("should return Some(ParsedInterval)");
        assert_eq!(parsed.start, Some(millis_at(2024, 1, 1)));
        assert_eq!(parsed.end, Some(millis_at(2024, 12, 31)));
    }

    #[tokio::test]
    async fn extract_interval_returns_none_when_llm_returns_none_none() {
        let llm = TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: None,
                ends_at: None,
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        };
        let retriever = build_retriever_with_llm(llm);

        let result = retriever
            .extract_interval("Who is Einstein?")
            .await
            .unwrap();

        assert!(
            result.is_none(),
            "both fields None means no interval detected"
        );
    }

    #[tokio::test]
    async fn extract_interval_returns_none_when_llm_fails() {
        let llm = TestLlm {
            completion_response: String::new(),
            interval_response: None,
            fail_structured_output: true,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        };
        let retriever = build_retriever_with_llm(llm);

        let result = retriever.extract_interval("What happened?").await.unwrap();

        assert!(result.is_none(), "error should be swallowed gracefully");
    }

    #[tokio::test]
    async fn extract_interval_with_only_starts_at() {
        let llm = TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2024, 1, 1)),
                ends_at: None,
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        };
        let retriever = build_retriever_with_llm(llm);

        let result = retriever
            .extract_interval("What happened after 2024?")
            .await
            .unwrap();

        let parsed = result.expect("should return Some(ParsedInterval)");
        assert_eq!(parsed.start, Some(millis_at(2024, 1, 1)));
        assert_eq!(parsed.end, None);
    }

    #[tokio::test]
    async fn extract_interval_with_only_ends_at() {
        let llm = TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: None,
                ends_at: Some(qts(2024, 12, 31)),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        };
        let retriever = build_retriever_with_llm(llm);

        let result = retriever
            .extract_interval("What happened before 2025?")
            .await
            .unwrap();

        let parsed = result.expect("should return Some(ParsedInterval)");
        assert_eq!(parsed.start, None);
        assert_eq!(parsed.end, Some(millis_at(2024, 12, 31)));
    }

    // ---- Phase 3: get_context edge case tests ----

    fn build_retriever(
        vector_db: Arc<dyn VectorDB>,
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
    ) -> TemporalRetriever {
        TemporalRetriever::new(
            vector_db,
            Arc::new(TestEmbeddingEngine),
            graph_db,
            llm,
            Some(10),
            Some(100),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn get_context_with_time_from_only() {
        // Timestamps: 2020-06-15, 2024-01-15, 2024-07-15
        let ts_2020 = "aa000000-0000-0000-0000-000000000001";
        let ts_2024_jan = "aa000000-0000-0000-0000-000000000002";
        let ts_2024_jul = "aa000000-0000-0000-0000-000000000003";
        let ev_old = "bb000000-0000-0000-0000-000000000001";
        let ev_jan = "bb000000-0000-0000-0000-000000000002";
        let ev_jul = "bb000000-0000-0000-0000-000000000003";

        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                timestamp_node(ts_2020, 1592179200000),
                timestamp_node(ts_2024_jan, 1705276800000),
                timestamp_node(ts_2024_jul, 1721001600000),
                event_graph_node(ev_old, "Old event"),
                event_graph_node(ev_jan, "Jan event"),
                event_graph_node(ev_jul, "Jul event"),
            ],
            edges: vec![
                (
                    ev_old.to_string(),
                    ts_2020.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
                (
                    ev_jan.to_string(),
                    ts_2024_jan.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
                (
                    ev_jul.to_string(),
                    ts_2024_jul.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
            ],
            neighbors: HashMap::from([
                (
                    ts_2020.to_string(),
                    vec![event_node_data(ev_old, "Old event")],
                ),
                (
                    ts_2024_jan.to_string(),
                    vec![event_node_data(ev_jan, "Jan event")],
                ),
                (
                    ts_2024_jul.to_string(),
                    vec![event_node_data(ev_jul, "Jul event")],
                ),
            ]),
        });

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Event", "name"),
                vec![
                    SearchResult {
                        id: uuid::Uuid::parse_str(ev_jan).unwrap(),
                        score: 0.9,
                        metadata: HashMap::new(),
                    },
                    SearchResult {
                        id: uuid::Uuid::parse_str(ev_jul).unwrap(),
                        score: 0.85,
                        metadata: HashMap::new(),
                    },
                ],
            )]),
        });

        let llm = Arc::new(TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2024, 1, 1)),
                ends_at: None,
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = build_retriever(vector_db, graph_db, llm);

        let context = retriever
            .get_context("What happened since 2024?", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 2, "should have 2 items (both 2024 events)");

        let event_names: Vec<&str> = context
            .iter()
            .filter_map(|item| item.payload.get("event_name").and_then(Value::as_str))
            .collect();
        assert!(
            event_names.contains(&"Jan event"),
            "Jan event should be present"
        );
        assert!(
            event_names.contains(&"Jul event"),
            "Jul event should be present"
        );
        assert!(
            !event_names.contains(&"Old event"),
            "Old event should NOT be present"
        );
    }

    #[tokio::test]
    async fn get_context_with_time_to_only() {
        let ts_2020 = "aa000000-0000-0000-0000-000000000001";
        let ts_2024_jan = "aa000000-0000-0000-0000-000000000002";
        let ts_2024_jul = "aa000000-0000-0000-0000-000000000003";
        let ev_old = "bb000000-0000-0000-0000-000000000001";
        let ev_jan = "bb000000-0000-0000-0000-000000000002";
        let ev_jul = "bb000000-0000-0000-0000-000000000003";

        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                timestamp_node(ts_2020, 1592179200000),
                timestamp_node(ts_2024_jan, 1705276800000),
                timestamp_node(ts_2024_jul, 1721001600000),
                event_graph_node(ev_old, "Old event"),
                event_graph_node(ev_jan, "Jan event"),
                event_graph_node(ev_jul, "Jul event"),
            ],
            edges: vec![
                (
                    ev_old.to_string(),
                    ts_2020.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
                (
                    ev_jan.to_string(),
                    ts_2024_jan.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
                (
                    ev_jul.to_string(),
                    ts_2024_jul.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
            ],
            neighbors: HashMap::from([
                (
                    ts_2020.to_string(),
                    vec![event_node_data(ev_old, "Old event")],
                ),
                (
                    ts_2024_jan.to_string(),
                    vec![event_node_data(ev_jan, "Jan event")],
                ),
                (
                    ts_2024_jul.to_string(),
                    vec![event_node_data(ev_jul, "Jul event")],
                ),
            ]),
        });

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Event", "name"),
                vec![SearchResult {
                    id: uuid::Uuid::parse_str(ev_old).unwrap(),
                    score: 0.88,
                    metadata: HashMap::new(),
                }],
            )]),
        });

        let llm = Arc::new(TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: None,
                ends_at: Some(qts(2021, 12, 31)),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = build_retriever(vector_db, graph_db, llm);

        let context = retriever
            .get_context("What happened before 2022?", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 1, "should have 1 item (only the 2020 event)");
        assert_eq!(
            context[0].payload.get("event_name").and_then(Value::as_str),
            Some("Old event")
        );
    }

    #[tokio::test]
    async fn get_context_falls_back_when_no_events_in_range() {
        let ts_2020 = "aa000000-0000-0000-0000-000000000010";
        let ts_2021 = "aa000000-0000-0000-0000-000000000011";
        let ev_2020 = "bb000000-0000-0000-0000-000000000010";
        let ev_2021 = "bb000000-0000-0000-0000-000000000011";
        let entity_a = "cc000000-0000-0000-0000-000000000001";
        let entity_b = "cc000000-0000-0000-0000-000000000002";

        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                timestamp_node(ts_2020, 1577836800000), // 2020-01-01
                timestamp_node(ts_2021, 1609459200000), // 2021-01-01
                event_graph_node(ev_2020, "Event 2020"),
                event_graph_node(ev_2021, "Event 2021"),
                (
                    entity_a.to_string(),
                    HashMap::from([
                        (Cow::Borrowed("id"), json!(entity_a)),
                        (Cow::Borrowed("name"), json!("Entity A")),
                        (Cow::Borrowed("type"), json!("Entity")),
                    ]),
                ),
                (
                    entity_b.to_string(),
                    HashMap::from([
                        (Cow::Borrowed("id"), json!(entity_b)),
                        (Cow::Borrowed("name"), json!("Entity B")),
                        (Cow::Borrowed("type"), json!("Entity")),
                    ]),
                ),
            ],
            edges: vec![
                (
                    ev_2020.to_string(),
                    ts_2020.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
                (
                    ev_2021.to_string(),
                    ts_2021.to_string(),
                    "at".to_string(),
                    HashMap::new(),
                ),
                (
                    entity_a.to_string(),
                    entity_b.to_string(),
                    "connected_to".to_string(),
                    HashMap::new(),
                ),
            ],
            neighbors: HashMap::from([
                (
                    ts_2020.to_string(),
                    vec![event_node_data(ev_2020, "Event 2020")],
                ),
                (
                    ts_2021.to_string(),
                    vec![event_node_data(ev_2021, "Event 2021")],
                ),
            ]),
        });

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Entity", "name"),
                vec![SearchResult {
                    id: uuid::Uuid::parse_str(entity_a).unwrap(),
                    score: 0.9,
                    metadata: HashMap::new(),
                }],
            )]),
        });

        let llm = Arc::new(TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2030, 1, 1)),
                ends_at: Some(qts(2031, 1, 1)),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = build_retriever(vector_db, graph_db, llm);

        let context = retriever
            .get_context("What happened in 2030?", &SearchParams::default())
            .await
            .unwrap();

        // Falls back to graph triplet search; context should have "relationship" in payload
        assert!(
            !context.is_empty(),
            "fallback should produce at least one result"
        );
        assert!(
            context
                .iter()
                .any(|item| item.payload.get("relationship").is_some()),
            "fallback context items should have 'relationship' in payload"
        );
        assert!(
            context
                .iter()
                .all(|item| item.payload.get("event_id").is_none()),
            "fallback context should NOT have 'event_id' (those are temporal items)"
        );
    }

    #[tokio::test]
    async fn get_context_on_empty_graph() {
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![],
            edges: vec![],
            neighbors: HashMap::new(),
        });

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::new(),
        });

        // LLM won't be called since the graph is empty
        let llm = Arc::new(TestLlm {
            completion_response: String::new(),
            interval_response: None,
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = build_retriever(vector_db, graph_db, llm);

        let context = retriever
            .get_context("Anything?", &SearchParams::default())
            .await
            .unwrap();

        assert!(
            context.is_empty(),
            "empty graph should return empty context"
        );
    }

    #[tokio::test]
    async fn get_context_respects_top_k() {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut neighbors = HashMap::new();
        let mut vector_results = Vec::new();

        for i in 1..=5 {
            let ts_id = format!("aa000000-0000-0000-0000-0000000000{i:02}");
            let ev_id = format!("bb000000-0000-0000-0000-0000000000{i:02}");
            let ev_name = format!("Event {i}");
            // All in 2024: Jan through May
            let time_ms = 1704067200000_i64 + (i as i64 - 1) * 30 * 86400 * 1000;

            nodes.push(timestamp_node(&ts_id, time_ms));
            nodes.push(event_graph_node(&ev_id, &ev_name));
            edges.push((
                ev_id.clone(),
                ts_id.clone(),
                "at".to_string(),
                HashMap::new(),
            ));
            neighbors.insert(ts_id, vec![event_node_data(&ev_id, &ev_name)]);
            vector_results.push(SearchResult {
                id: uuid::Uuid::parse_str(&ev_id).unwrap(),
                score: 0.9 - (i as f32 * 0.01),
                metadata: HashMap::new(),
            });
        }

        let graph_db = Arc::new(TestGraphDb {
            nodes,
            edges,
            neighbors,
        });

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(TestVectorDb::key("Event", "name"), vector_results)]),
        });

        let llm = Arc::new(TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2024, 1, 1)),
                ends_at: Some(qts(2024, 12, 31)),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = build_retriever(vector_db, graph_db, llm);

        let params = SearchParams {
            top_k: Some(2),
            ..Default::default()
        };

        let context = retriever
            .get_context("What happened in 2024?", &params)
            .await
            .unwrap();

        assert_eq!(context.len(), 2, "top_k=2 should limit results to 2 items");
    }

    #[tokio::test]
    async fn get_context_2hop_interval_traversal() {
        let ts1 = "aa000000-0000-0000-0000-000000000020";
        let ts2 = "aa000000-0000-0000-0000-000000000021";
        let interval_id = "ii000000-0000-0000-0000-000000000001";
        let event_id = "bb000000-0000-0000-0000-000000000020";

        let interval_node_data: NodeData = HashMap::from([
            (Cow::Borrowed("id"), json!(interval_id)),
            (Cow::Borrowed("type"), json!("Interval")),
            (Cow::Borrowed("name"), json!("Feb-Mar 2024")),
        ]);

        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                timestamp_node(ts1, 1706745600000), // 2024-02-01
                timestamp_node(ts2, 1709251200000), // 2024-03-01
                (interval_id.to_string(), interval_node_data.clone()),
                event_graph_node(event_id, "Team Meeting"),
            ],
            edges: vec![(
                event_id.to_string(),
                interval_id.to_string(),
                "during".to_string(),
                HashMap::new(),
            )],
            neighbors: HashMap::from([
                // Timestamp T1 -> Interval (1st hop)
                (ts1.to_string(), vec![interval_node_data.clone()]),
                // Timestamp T2 -> Interval (1st hop)
                (ts2.to_string(), vec![interval_node_data]),
                // Interval -> Event (2nd hop)
                (
                    interval_id.to_string(),
                    vec![event_node_data(event_id, "Team Meeting")],
                ),
            ]),
        });

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Event", "name"),
                vec![SearchResult {
                    id: uuid::Uuid::parse_str(event_id).unwrap(),
                    score: 0.92,
                    metadata: HashMap::new(),
                }],
            )]),
        });

        let llm = Arc::new(TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2024, 2, 1)),
                ends_at: Some(qts(2024, 3, 31)),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = build_retriever(vector_db, graph_db, llm);

        let context = retriever
            .get_context(
                "What meetings happened in Feb-Mar 2024?",
                &SearchParams::default(),
            )
            .await
            .unwrap();

        assert_eq!(
            context.len(),
            1,
            "should find 1 event via 2-hop traversal (Timestamp -> Interval -> Event)"
        );
        assert_eq!(
            context[0].payload.get("event_name").and_then(Value::as_str),
            Some("Team Meeting")
        );
    }

    #[tokio::test]
    async fn get_context_matches_non_midnight_event_within_range() {
        // Guards intra-period inclusivity: every other fixture pins events to
        // 00:00:00, so a mid-day event landing strictly inside the interval
        // (not on either boundary instant) would otherwise be unverified.
        let ts_id = "aa000000-0000-0000-0000-000000000030";
        let ev_id = "bb000000-0000-0000-0000-000000000030";
        // 2024-02-15T14:30:00Z — deliberately not midnight.
        let mid_day_ms = Utc
            .with_ymd_and_hms(2024, 2, 15, 14, 30, 0)
            .unwrap()
            .timestamp_millis();

        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                timestamp_node(ts_id, mid_day_ms),
                event_graph_node(ev_id, "Mid-day event"),
            ],
            edges: vec![(
                ev_id.to_string(),
                ts_id.to_string(),
                "at".to_string(),
                HashMap::new(),
            )],
            neighbors: HashMap::from([(
                ts_id.to_string(),
                vec![event_node_data(ev_id, "Mid-day event")],
            )]),
        });

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Event", "name"),
                vec![SearchResult {
                    id: uuid::Uuid::parse_str(ev_id).unwrap(),
                    score: 0.9,
                    metadata: HashMap::new(),
                }],
            )]),
        });

        // Interval [2024-01-01 00:00:00, 2024-12-31 00:00:00] strictly contains
        // the mid-day event.
        let llm = Arc::new(TestLlm {
            completion_response: String::new(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2024, 1, 1)),
                ends_at: Some(qts(2024, 12, 31)),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = build_retriever(vector_db, graph_db, llm);

        let context = retriever
            .get_context("What happened in 2024?", &SearchParams::default())
            .await
            .unwrap();

        assert_eq!(context.len(), 1, "mid-day event should be within range");
        assert_eq!(
            context[0].payload.get("event_name").and_then(Value::as_str),
            Some("Mid-day event")
        );
    }

    // -----------------------------------------------------------------------
    // Phase 4 — get_completion unit tests
    // -----------------------------------------------------------------------

    fn default_session() -> SessionContext {
        SessionContext {
            session_id: None,
            history: vec![],
            formatted_history: String::new(),
            graph_context: None,
        }
    }

    fn make_event_context() -> Vec<SearchItem> {
        vec![
            SearchItem {
                id: None,
                score: Some(0.9),
                payload: json!({
                    "event_id": "evt-1",
                    "event_name": "Product Launch",
                    "event_description": "Launched the new product",
                    "event_time": "2024-03-15",
                }),
            },
            SearchItem {
                id: None,
                score: Some(0.7),
                payload: json!({
                    "event_id": "evt-2",
                    "event_name": "Quarterly Review",
                    "event_description": "Reviewed Q1 results",
                    "event_time": "2024-04-01",
                }),
            },
        ]
    }

    fn simple_retriever(llm: Arc<TestLlm>) -> TemporalRetriever {
        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::new(),
        });
        let embedding_engine = Arc::new(TestEmbeddingEngine);
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![],
            edges: vec![],
            neighbors: HashMap::new(),
        });

        TemporalRetriever::new(
            vector_db,
            embedding_engine,
            graph_db,
            llm,
            Some(5),
            Some(10),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn get_completion_generates_text_from_context() {
        let llm = Arc::new(TestLlm {
            completion_response: "The product was launched in March 2024.".to_string(),
            last_messages: Mutex::new(vec![]),
            last_structured_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = simple_retriever(llm);
        let context = make_event_context();
        let session = default_session();

        let output = retriever
            .get_completion(
                "What happened in 2024?",
                Some(context),
                &session,
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => {
                assert_eq!(text, "The product was launched in March 2024.");
            }
            other => panic!("Expected SearchOutput::Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_completion_with_provided_context_passes_to_llm() {
        let llm = Arc::new(TestLlm {
            completion_response: "completion result".to_string(),
            last_messages: Mutex::new(vec![]),
            last_structured_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = simple_retriever(Arc::clone(&llm));
        let context = make_event_context();
        let session = default_session();

        retriever
            .get_completion(
                "What happened in 2024?",
                Some(context),
                &session,
                &SearchParams::default(),
            )
            .await
            .unwrap();

        let messages = llm.last_messages.lock().unwrap();
        assert_eq!(messages.len(), 2, "Expected system + user messages");

        // The user prompt should contain the temporal context text.
        let user_msg = &messages[1].content;
        assert!(
            user_msg.contains("Product Launch"),
            "User prompt should contain event name from context"
        );
        assert!(
            user_msg.contains("Quarterly Review"),
            "User prompt should contain second event name from context"
        );
    }

    #[tokio::test]
    async fn get_completion_without_context_calls_get_context() {
        // Setup a graph with temporal data so get_context can produce context.
        let launch_event_ms: i64 = 1710460800000; // 2024-03-15 UTC
        let ts_id = "ts-aaa";
        let event_id = "ev-111";

        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Entity", "name"),
                vec![SearchResult {
                    id: Uuid::new_v4(),
                    score: 0.8,
                    metadata: HashMap::new(),
                }],
            )]),
        });

        let embedding_engine = Arc::new(TestEmbeddingEngine);
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                timestamp_node(ts_id, launch_event_ms),
                event_graph_node(event_id, "Launch"),
            ],
            edges: vec![],
            neighbors: HashMap::from([(
                ts_id.to_string(),
                vec![event_node_data(event_id, "Launch")],
            )]),
        });

        let llm = Arc::new(TestLlm {
            completion_response: "answer from internal context".to_string(),
            interval_response: Some(QueryInterval {
                starts_at: Some(qts(2024, 1, 1)),
                ends_at: Some(qts(2024, 12, 31)),
            }),
            last_messages: Mutex::new(vec![]),
            last_structured_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = TemporalRetriever::new(
            vector_db,
            embedding_engine,
            graph_db,
            llm.clone(),
            Some(5),
            Some(10),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        );

        let session = default_session();

        let output = retriever
            .get_completion(
                "What happened in 2024?",
                None,
                &session,
                &SearchParams::default(),
            )
            .await
            .unwrap();

        match output {
            SearchOutput::Text(text) => {
                assert_eq!(text, "answer from internal context");
            }
            other => panic!("Expected SearchOutput::Text, got {other:?}"),
        }

        // Verify that the LLM's generate was called with messages containing context.
        let messages = llm.last_messages.lock().unwrap();
        assert!(!messages.is_empty(), "LLM generate should have been called");
        let user_msg = &messages[1].content;
        assert!(
            user_msg.contains("Launch"),
            "User prompt should reference the event from internal context"
        );
    }

    #[tokio::test]
    async fn get_completion_with_response_schema() {
        let structured_value = json!({
            "answer": "The product launched in 2024",
            "confidence": 0.95
        });

        let llm = Arc::new(TestLlm {
            completion_response: "should not be used".to_string(),
            structured_completion_response: Mutex::new(Some(structured_value.clone())),
            last_messages: Mutex::new(vec![]),
            last_structured_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = simple_retriever(llm);
        let context = make_event_context();
        let session = default_session();

        let params = SearchParams {
            response_schema: Some(json!({
                "type": "object",
                "properties": {
                    "answer": { "type": "string" },
                    "confidence": { "type": "number" }
                }
            })),
            ..SearchParams::default()
        };

        let output = retriever
            .get_completion("What happened in 2024?", Some(context), &session, &params)
            .await
            .unwrap();

        match output {
            SearchOutput::Structured(value) => {
                assert_eq!(value, structured_value);
            }
            other => panic!("Expected SearchOutput::Structured, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_completion_includes_session_history() {
        let llm = Arc::new(TestLlm {
            completion_response: "history-aware answer".to_string(),
            last_messages: Mutex::new(vec![]),
            last_structured_messages: Mutex::new(vec![]),
            ..TestLlm::default()
        });

        let retriever = simple_retriever(Arc::clone(&llm));
        let context = make_event_context();

        let session = SessionContext {
            session_id: Some("sess-1".to_string()),
            history: vec![
                Message::user("Previous question?".to_string()),
                Message::assistant("Previous answer.".to_string()),
            ],
            formatted_history: "Q: Previous question?\nA: Previous answer.".to_string(),
            graph_context: None,
        };

        retriever
            .get_completion(
                "Follow-up question?",
                Some(context),
                &session,
                &SearchParams::default(),
            )
            .await
            .unwrap();

        let messages = llm.last_messages.lock().unwrap();
        assert_eq!(messages.len(), 2, "Expected system + user messages");

        // The system prompt should contain session history (prepended via TASK:).
        let system_msg = &messages[0].content;
        assert!(
            system_msg.contains("Previous question?"),
            "System prompt should include session history"
        );
        assert!(
            system_msg.contains("Previous answer."),
            "System prompt should include session history answer"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 5 — rank_temporal_events unit tests
    // -----------------------------------------------------------------------

    fn make_ranked_edge(source_id: &str, target_id: &str, score: f32) -> RankedGraphEdge {
        RankedGraphEdge {
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            relationship_name: "related_to".to_string(),
            score,
            source_name: format!("Source-{source_id}"),
            target_name: format!("Target-{target_id}"),
            dataset_id: None,
            source_text: None,
            target_text: None,
            source_description: None,
            target_description: None,
        }
    }

    fn ranking_retriever() -> TemporalRetriever {
        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Event", "name"),
                vec![
                    SearchResult {
                        id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap(),
                        score: 0.9,
                        metadata: HashMap::new(),
                    },
                    SearchResult {
                        id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap(),
                        score: 0.5,
                        metadata: HashMap::new(),
                    },
                    SearchResult {
                        id: Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap(),
                        score: 0.3,
                        metadata: HashMap::new(),
                    },
                ],
            )]),
        });

        let embedding_engine = Arc::new(TestEmbeddingEngine);
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![],
            edges: vec![],
            neighbors: HashMap::new(),
        });

        let llm = Arc::new(TestLlm::default());

        TemporalRetriever::new(
            vector_db,
            embedding_engine,
            graph_db,
            llm,
            Some(5),
            Some(10),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        )
    }

    #[tokio::test]
    async fn rank_sorts_by_combined_score() {
        let retriever = ranking_retriever();

        let event_ids: HashSet<String> = [
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            "cccccccc-cccc-cccc-cccc-cccccccccccc",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let ranked_edges = vec![
            make_ranked_edge("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "other-node", 0.8),
            make_ranked_edge("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "other-node", 0.4),
            make_ranked_edge("cccccccc-cccc-cccc-cccc-cccccccccccc", "other-node", 0.2),
        ];

        let ranked = retriever
            .rank_temporal_events("test query", &event_ids, &ranked_edges)
            .await
            .unwrap();

        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].0, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
        assert_eq!(ranked[1].0, "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb");
        assert_eq!(ranked[2].0, "cccccccc-cccc-cccc-cccc-cccccccccccc");

        // Verify descending order.
        assert!(ranked[0].1 >= ranked[1].1);
        assert!(ranked[1].1 >= ranked[2].1);
    }

    #[tokio::test]
    async fn rank_events_not_in_vector_get_default_score() {
        let retriever = ranking_retriever();

        let event_ids: HashSet<String> = [
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "dddddddd-dddd-dddd-dddd-dddddddddddd", // Not in vector DB
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let ranked_edges = vec![make_ranked_edge(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "other-node",
            0.8,
        )];

        let ranked = retriever
            .rank_temporal_events("test query", &event_ids, &ranked_edges)
            .await
            .unwrap();

        assert_eq!(ranked.len(), 2);

        let unknown_event = ranked
            .iter()
            .find(|(id, _)| id == "dddddddd-dddd-dddd-dddd-dddddddddddd")
            .unwrap();
        assert!(
            unknown_event.1.abs() < f32::EPSILON,
            "Unknown event should have score 0.0, got {}",
            unknown_event.1
        );

        let known_event = ranked
            .iter()
            .find(|(id, _)| id == "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
            .unwrap();
        assert!(
            known_event.1 > unknown_event.1,
            "Known event should have higher score"
        );
    }

    #[tokio::test]
    async fn rank_empty_vector_results() {
        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::new(),
        });
        let embedding_engine = Arc::new(TestEmbeddingEngine);
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![],
            edges: vec![],
            neighbors: HashMap::new(),
        });
        let llm = Arc::new(TestLlm::default());

        let retriever = TemporalRetriever::new(
            vector_db,
            embedding_engine,
            graph_db,
            llm,
            Some(5),
            Some(10),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        );

        let event_ids: HashSet<String> = ["ev-1", "ev-2"].iter().map(|s| s.to_string()).collect();

        let ranked_edges = vec![
            make_ranked_edge("ev-1", "other", 0.6),
            make_ranked_edge("other", "ev-2", 0.3),
        ];

        let ranked = retriever
            .rank_temporal_events("query", &event_ids, &ranked_edges)
            .await
            .unwrap();

        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, "ev-1");
        assert!((ranked[0].1 - 0.6).abs() < f32::EPSILON);
        assert_eq!(ranked[1].0, "ev-2");
        assert!((ranked[1].1 - 0.3).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn rank_empty_event_ids() {
        let retriever = ranking_retriever();

        let event_ids: HashSet<String> = HashSet::new();
        let ranked_edges = vec![make_ranked_edge("some-node", "other-node", 0.5)];

        let ranked = retriever
            .rank_temporal_events("query", &event_ids, &ranked_edges)
            .await
            .unwrap();

        assert!(
            ranked.is_empty(),
            "Empty event_ids should yield empty result"
        );
    }

    #[tokio::test]
    async fn rank_mismatched_vector_ids() {
        let vector_db = Arc::new(TestVectorDb {
            collections: HashMap::from([(
                TestVectorDb::key("Event", "name"),
                vec![SearchResult {
                    id: Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap(),
                    score: 0.99,
                    metadata: HashMap::new(),
                }],
            )]),
        });
        let embedding_engine = Arc::new(TestEmbeddingEngine);
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![],
            edges: vec![],
            neighbors: HashMap::new(),
        });
        let llm = Arc::new(TestLlm::default());

        let retriever = TemporalRetriever::new(
            vector_db,
            embedding_engine,
            graph_db,
            llm,
            Some(5),
            Some(10),
            Some(0.0),
            None,
            None,
            None,
            None,
            None,
        );

        let event_ids: HashSet<String> =
            ["ev-abc", "ev-def"].iter().map(|s| s.to_string()).collect();

        let ranked_edges = vec![make_ranked_edge("ev-abc", "something", 0.4)];

        let ranked = retriever
            .rank_temporal_events("query", &event_ids, &ranked_edges)
            .await
            .unwrap();

        assert_eq!(ranked.len(), 2);
        let ev_abc = ranked.iter().find(|(id, _)| id == "ev-abc").unwrap();
        assert!((ev_abc.1 - 0.4).abs() < f32::EPSILON);

        let ev_def = ranked.iter().find(|(id, _)| id == "ev-def").unwrap();
        assert!(ev_def.1.abs() < f32::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Phase 6 — temporal_context_to_text unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn context_to_text_formats_event_items() {
        let context = vec![
            SearchItem {
                id: None,
                score: Some(0.9),
                payload: json!({
                    "event_id": "evt-1",
                    "event_name": "Product Launch",
                    "event_description": "Launched the new product",
                    "event_time": "2024-03-15",
                }),
            },
            SearchItem {
                id: None,
                score: Some(0.7),
                payload: json!({
                    "event_id": "evt-2",
                    "event_name": "Quarterly Review",
                    "event_description": "Reviewed Q1 results",
                    "event_time": "2024-04-01",
                }),
            },
        ];

        let text = TemporalRetriever::temporal_context_to_text(&context);
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            "Product Launch (2024-03-15): Launched the new product"
        );
        assert_eq!(
            lines[1],
            "Quarterly Review (2024-04-01): Reviewed Q1 results"
        );
    }

    #[test]
    fn context_to_text_formats_triplet_items() {
        let context = vec![
            SearchItem {
                id: None,
                score: Some(0.8),
                payload: json!({
                    "source_name": "Alice",
                    "target_name": "Bob",
                    "relationship": "knows",
                }),
            },
            SearchItem {
                id: None,
                score: Some(0.6),
                payload: json!({
                    "source_name": "Company X",
                    "target_name": "Product Y",
                    "relationship": "produces",
                }),
            },
        ];

        let text = TemporalRetriever::temporal_context_to_text(&context);
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "Alice -[knows]-> Bob");
        assert_eq!(lines[1], "Company X -[produces]-> Product Y");
    }

    #[test]
    fn context_to_text_empty_context() {
        let context: Vec<SearchItem> = vec![];
        let text = TemporalRetriever::temporal_context_to_text(&context);
        assert_eq!(text, "");
    }

    #[test]
    fn context_to_text_missing_fields_use_defaults() {
        let context = vec![SearchItem {
            id: None,
            score: Some(0.5),
            payload: json!({
                "event_id": "evt-bare",
            }),
        }];

        let text = TemporalRetriever::temporal_context_to_text(&context);
        assert_eq!(text, "Unnamed event (unknown time): No description");
    }

    #[test]
    fn context_to_text_mixed_items() {
        let context = vec![
            SearchItem {
                id: None,
                score: Some(0.9),
                payload: json!({
                    "event_id": "evt-1",
                    "event_name": "Conference",
                    "event_description": "Annual tech conference",
                    "event_time": "2024-06-15",
                }),
            },
            SearchItem {
                id: None,
                score: Some(0.7),
                payload: json!({
                    "source_name": "Speaker",
                    "target_name": "Conference",
                    "relationship": "presents_at",
                }),
            },
        ];

        let text = TemporalRetriever::temporal_context_to_text(&context);
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "Conference (2024-06-15): Annual tech conference");
        assert_eq!(lines[1], "Speaker -[presents_at]-> Conference");
    }
}
