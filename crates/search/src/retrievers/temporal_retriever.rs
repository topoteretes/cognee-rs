use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, TimeZone, Utc};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::{GraphDBTrait, NodeData};
use cognee_llm::{GenerationOptions, Llm, LlmExt, Message};
use cognee_vector::VectorDB;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use cognee_session::SessionContext;

use crate::graph_retrieval::{GraphRetrievalConfig, RankedGraphEdge, brute_force_triplet_search};
use crate::retrievers::SearchRetriever;
use crate::types::{SearchContext, SearchError, SearchItem, SearchOutput, SearchType};
use crate::utils::{build_messages_with_history, render_user_prompt, resolve_system_prompt};

const DEFAULT_TOP_K: usize = 10;
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
const TEMPORAL_DATA_TYPE: &str = "Event";
const TEMPORAL_FIELD_NAME: &str = "name";
const DEFAULT_TEMPORAL_INTERVAL_PROMPT: &str = "Extract the temporal interval for a user question. Return JSON with optional string fields `start` and `end` in ISO-like format (YYYY, YYYY-MM, YYYY-MM-DD, or RFC3339). Leave missing bounds as null.";

const TEMPORAL_TIME_KEYS: [&str; 11] = [
    "timestamp",
    "event_time",
    "time",
    "date",
    "datetime",
    "occurred_at",
    "start_time",
    "start_date",
    "end_time",
    "end_date",
    "created_at",
];

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct QueryInterval {
    start: Option<String>,
    end: Option<String>,
}

#[derive(Debug, Clone)]
struct ParsedInterval {
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
}

impl QueryInterval {
    fn parse(self) -> ParsedInterval {
        ParsedInterval {
            start: self
                .start
                .as_deref()
                .and_then(|value| parse_bound(value, false)),
            end: self
                .end
                .as_deref()
                .and_then(|value| parse_bound(value, true)),
        }
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
            triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
            temporal_interval_prompt,
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
        }
    }

    async fn extract_interval(&self, query: &str) -> Result<Option<ParsedInterval>, SearchError> {
        let system_prompt = self
            .temporal_interval_prompt
            .as_deref()
            .unwrap_or(DEFAULT_TEMPORAL_INTERVAL_PROMPT)
            .to_string();

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

        let parsed = interval.parse();
        if parsed.start.is_none() && parsed.end.is_none() {
            return Ok(None);
        }

        Ok(Some(parsed))
    }

    fn get_graph_retrieval_config(&self) -> GraphRetrievalConfig {
        GraphRetrievalConfig {
            top_k: self.top_k,
            wide_search_top_k: self.wide_search_top_k,
            triplet_distance_penalty: self.triplet_distance_penalty,
        }
    }

    async fn get_ranked_graph_edges(
        &self,
        query: &str,
    ) -> Result<Vec<RankedGraphEdge>, SearchError> {
        brute_force_triplet_search(
            query,
            self.vector_db.as_ref(),
            self.embedding_engine.as_ref(),
            self.graph_db.as_ref(),
            &self.get_graph_retrieval_config(),
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
                }),
            })
            .collect()
    }

    async fn get_fallback_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        let ranked_edges = self.get_ranked_graph_edges(query).await?;
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

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        if self.graph_db.is_empty().await? {
            return Ok(vec![]);
        }

        let Some(interval) = self.extract_interval(query).await? else {
            return self.get_fallback_context(query).await;
        };

        let (nodes, _) = self.graph_db.get_graph_data().await?;
        let mut event_node_ids = HashSet::new();

        for (node_id, node_data) in &nodes {
            if !is_event_node(node_data) {
                continue;
            }

            let Some(event_time) = extract_event_time(node_data) else {
                continue;
            };

            if is_within_interval(event_time, &interval) {
                event_node_ids.insert(node_id.clone());
            }
        }

        if event_node_ids.is_empty() {
            return self.get_fallback_context(query).await;
        }

        let ranked_edges = self.get_ranked_graph_edges(query).await?;
        let ranked_events = self
            .rank_temporal_events(query, &event_node_ids, &ranked_edges)
            .await?;

        let nodes_by_id: HashMap<String, NodeData> = nodes.into_iter().collect();
        let mut temporal_context = Vec::new();

        for (event_id, score) in ranked_events.into_iter().take(self.top_k) {
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
                    "event_time": extract_event_time(event_node).map(|time| time.to_rfc3339()),
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
    ) -> Result<SearchOutput, SearchError> {
        let completion_context = match context {
            Some(existing_context) => existing_context,
            None => self.get_context(query).await?,
        };

        let system_prompt = resolve_system_prompt(
            self.system_prompt.as_deref(),
            self.system_prompt_path.as_deref(),
        )?;

        let user_prompt = render_user_prompt(
            self.user_prompt_template.as_deref(),
            query,
            &Self::temporal_context_to_text(&completion_context),
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

fn is_event_node(node_data: &NodeData) -> bool {
    let type_keys = ["type", "node_type", "kind", "label", "labels", "class"];

    let matches_event_marker = type_keys.iter().any(|key| {
        node_data
            .get(*key)
            .map(value_contains_event_marker)
            .unwrap_or(false)
    });

    matches_event_marker || extract_event_time(node_data).is_some()
}

fn value_contains_event_marker(value: &Value) -> bool {
    match value {
        Value::String(text) => text.to_lowercase().contains("event"),
        Value::Array(values) => values.iter().any(value_contains_event_marker),
        _ => false,
    }
}

fn extract_event_time(node_data: &NodeData) -> Option<DateTime<Utc>> {
    for key in TEMPORAL_TIME_KEYS {
        if let Some(value) = node_data.get(key)
            && let Some(parsed) = parse_temporal_value(value)
        {
            return Some(parsed);
        }
    }

    for (key, value) in node_data {
        let key_lower = key.to_lowercase();
        if (key_lower.contains("time") || key_lower.contains("date"))
            && let Some(parsed) = parse_temporal_value(value)
        {
            return Some(parsed);
        }
    }

    None
}

fn parse_temporal_value(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => parse_bound(text, false),
        Value::Number(number) => {
            let raw_value = number.as_i64()?;

            if raw_value > 1_000_000_000_000 {
                Utc.timestamp_millis_opt(raw_value).single()
            } else {
                Utc.timestamp_opt(raw_value, 0).single()
            }
        }
        _ => None,
    }
}

fn parse_bound(input: &str, is_end: bool) -> Option<DateTime<Utc>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(timestamp) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(timestamp.with_timezone(&Utc));
    }

    if let Ok(naive_dt) = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
        return Some(Utc.from_utc_datetime(&naive_dt));
    }

    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return to_datetime(date, is_end);
    }

    if trimmed.len() == 7 {
        let month_candidate = format!("{trimmed}-01");
        if let Ok(date) = NaiveDate::parse_from_str(&month_candidate, "%Y-%m-%d") {
            return if is_end {
                let (next_year, next_month) = if date.month() == 12 {
                    (date.year() + 1, 1)
                } else {
                    (date.year(), date.month() + 1)
                };

                let next_month_start = NaiveDate::from_ymd_opt(next_year, next_month, 1)?;
                let month_end = next_month_start.pred_opt()?;
                to_datetime(month_end, true)
            } else {
                to_datetime(date, false)
            };
        }
    }

    if trimmed.len() == 4
        && trimmed.chars().all(|character| character.is_ascii_digit())
        && let Ok(year) = trimmed.parse::<i32>()
    {
        let date = if is_end {
            NaiveDate::from_ymd_opt(year, 12, 31)?
        } else {
            NaiveDate::from_ymd_opt(year, 1, 1)?
        };

        return to_datetime(date, is_end);
    }

    None
}

fn to_datetime(date: NaiveDate, is_end: bool) -> Option<DateTime<Utc>> {
    let naive_dt = if is_end {
        date.and_hms_opt(23, 59, 59)?
    } else {
        date.and_hms_opt(0, 0, 0)?
    };

    Some(Utc.from_utc_datetime(&naive_dt))
}

fn is_within_interval(event_time: DateTime<Utc>, interval: &ParsedInterval) -> bool {
    if let Some(start) = interval.start
        && event_time < start
    {
        return false;
    }

    if let Some(end) = interval.end
        && event_time > end
    {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use cognee_embedding::EmbeddingResult;
    use cognee_embedding::engine::EmbeddingEngine;
    use cognee_graph::{EdgeData, GraphDBResult, GraphDBTrait, GraphNode, NodeData};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };
    use cognee_vector::{SearchResult, VectorDB, VectorDBResult, VectorPoint};

    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{QueryInterval, TemporalRetriever};
    use crate::retrievers::SearchRetriever;

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

        async fn get_neighbors(&self, _node_id: &str) -> GraphDBResult<Vec<NodeData>> {
            Ok(vec![])
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
            _messages: Vec<Message>,
            _json_schema: &serde_json::Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<serde_json::Value> {
            if self.fail_structured_output {
                return Err(LlmError::ConfigError("forced failure".to_string()));
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

    fn event_node(id: &str, name: &str, date: &str) -> GraphNode {
        (
            id.to_string(),
            HashMap::from([
                (Cow::Borrowed("id"), json!(id)),
                (Cow::Borrowed("name"), json!(name)),
                (Cow::Borrowed("type"), json!("Event")),
                (Cow::Borrowed("date"), json!(date)),
                (
                    Cow::Borrowed("description"),
                    json!(format!("Description for {name}")),
                ),
            ]),
        )
    }

    #[tokio::test]
    async fn returns_temporal_event_context_when_interval_matches() {
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
                        id: uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
                        score: 0.95,
                        metadata: HashMap::new(),
                    }],
                ),
            ]),
        });

        let embedding_engine = Arc::new(TestEmbeddingEngine);
        let graph_db = Arc::new(TestGraphDb {
            nodes: vec![
                event_node(
                    "11111111-1111-1111-1111-111111111111",
                    "Launch event",
                    "2024-03-15",
                ),
                event_node(
                    "22222222-2222-2222-2222-222222222222",
                    "Old event",
                    "2020-01-10",
                ),
            ],
            edges: vec![
                (
                    "11111111-1111-1111-1111-111111111111".to_string(),
                    "entity-a".to_string(),
                    "relates_to".to_string(),
                    HashMap::new(),
                ),
                (
                    "22222222-2222-2222-2222-222222222222".to_string(),
                    "entity-b".to_string(),
                    "relates_to".to_string(),
                    HashMap::new(),
                ),
            ],
        });
        let llm = Arc::new(TestLlm {
            completion_response: "temporal answer".to_string(),
            interval_response: Some(QueryInterval {
                start: Some("2024-01-01".to_string()),
                end: Some("2024-12-31".to_string()),
            }),
            fail_structured_output: false,
            last_messages: Mutex::new(vec![]),
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
            .get_context("What happened in 2024?")
            .await
            .unwrap();

        assert_eq!(context.len(), 1);
        assert_eq!(
            context[0].payload.get("event_name").and_then(Value::as_str),
            Some("Launch event")
        );
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
        });
        let llm = Arc::new(TestLlm {
            completion_response: "fallback answer".to_string(),
            interval_response: None,
            fail_structured_output: true,
            last_messages: Mutex::new(vec![]),
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

        let context = retriever.get_context("What happened?").await.unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(
            context[0]
                .payload
                .get("relationship")
                .and_then(Value::as_str),
            Some("connected_to")
        );
    }
}
