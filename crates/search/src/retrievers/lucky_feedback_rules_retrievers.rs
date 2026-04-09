use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use cognee_graph::{GraphDBTrait, GraphDBTraitExt};
use cognee_llm::{GenerationOptions, Llm, LlmExt, Message};
use cognee_session::SessionContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::retrievers::{SearchRetriever, SearchRetrieverRef};
use crate::types::{Rule, SearchContext, SearchError, SearchItem, SearchOutput, SearchType};

const DEFAULT_FEELING_LUCKY_PROMPT: &str = "You are a search method selector. Return ONLY one valid search type name in SCREAMING_SNAKE_CASE.";
const DEFAULT_FEELING_LUCKY_FALLBACK: SearchType = SearchType::RagCompletion;

const DEFAULT_FEEDBACK_PROMPT: &str = "Extract user feedback sentiment and a numeric score in range [-1, 1]. Return JSON with fields: sentiment (string), score (number).";
const DEFAULT_FEEDBACK_EDGE_REL: &str = "HAS_FEEDBACK";
const DEFAULT_FEEDBACK_LAST_K: usize = 5;

const DEFAULT_RULE_NODE_SET: &str = "coding_agent_rules";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct FeedbackAnalysis {
    sentiment: String,
    score: f32,
}

#[derive(Debug, Serialize)]
struct FeedbackNode {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    text: String,
    sentiment: String,
    score: f32,
    created_at: String,
}

pub struct FeelingLuckyRetriever {
    llm: Arc<dyn Llm>,
    retrievers: HashMap<SearchType, SearchRetrieverRef>,
    fallback_search_type: SearchType,
    generation_options: Option<GenerationOptions>,
}

impl FeelingLuckyRetriever {
    pub fn new(
        llm: Arc<dyn Llm>,
        retrievers: HashMap<SearchType, SearchRetrieverRef>,
        fallback_search_type: Option<SearchType>,
        generation_options: Option<GenerationOptions>,
    ) -> Self {
        Self {
            llm,
            retrievers,
            fallback_search_type: fallback_search_type.unwrap_or(DEFAULT_FEELING_LUCKY_FALLBACK),
            generation_options,
        }
    }

    fn fallback_retriever(&self) -> Result<SearchRetrieverRef, SearchError> {
        self.retrievers
            .get(&self.fallback_search_type)
            .cloned()
            .ok_or(SearchError::UnsupportedSearchType(
                self.fallback_search_type,
            ))
    }

    fn parse_search_type(raw: &str) -> Option<SearchType> {
        let normalized = raw
            .trim()
            .trim_matches('"')
            .replace([' ', '-'], "_")
            .to_ascii_uppercase();

        serde_json::from_value::<SearchType>(Value::String(normalized)).ok()
    }

    async fn select_retriever(&self, query: &str) -> Result<SearchRetrieverRef, SearchError> {
        let allowed_types = self
            .retrievers
            .keys()
            .copied()
            .filter(|search_type| *search_type != SearchType::FeelingLucky)
            .map(|search_type| format!("{:?}", search_type).to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(", ");

        let selector_prompt = format!(
            "{DEFAULT_FEELING_LUCKY_PROMPT}\nAllowed types: {allowed_types}\nReturn only one value."
        );

        let response = self
            .llm
            .generate(
                vec![
                    Message::system(selector_prompt),
                    Message::user(query.to_string()),
                ],
                self.generation_options.clone(),
            )
            .await;

        let selected_type = response
            .ok()
            .and_then(|completion| Self::parse_search_type(completion.content.as_str()))
            .filter(|search_type| *search_type != SearchType::FeelingLucky);

        match selected_type.and_then(|search_type| self.retrievers.get(&search_type).cloned()) {
            Some(retriever) => Ok(retriever),
            None => self.fallback_retriever(),
        }
    }
}

#[async_trait]
impl SearchRetriever for FeelingLuckyRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::FeelingLucky
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        self.select_retriever(query).await?.get_context(query).await
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
    ) -> Result<SearchOutput, SearchError> {
        self.select_retriever(query)
            .await?
            .get_completion(query, context, session)
            .await
    }
}

pub struct FeedbackRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
    llm: Arc<dyn Llm>,
    last_k: usize,
    generation_options: Option<GenerationOptions>,
}

impl FeedbackRetriever {
    pub fn new(
        graph_db: Arc<dyn GraphDBTrait>,
        llm: Arc<dyn Llm>,
        last_k: Option<usize>,
        generation_options: Option<GenerationOptions>,
    ) -> Self {
        Self {
            graph_db,
            llm,
            last_k: last_k.unwrap_or(DEFAULT_FEEDBACK_LAST_K),
            generation_options,
        }
    }

    async fn extract_feedback(&self, feedback_text: &str) -> Result<FeedbackAnalysis, SearchError> {
        let analysis: FeedbackAnalysis = self
            .llm
            .create_structured_output_with_messages(
                vec![
                    Message::system(DEFAULT_FEEDBACK_PROMPT.to_string()),
                    Message::user(feedback_text.to_string()),
                ],
                self.generation_options.clone(),
            )
            .await
            .map_err(SearchError::from)?;
        Ok(analysis)
    }

    fn is_interaction_node(node_data: &HashMap<Cow<'static, str>, Value>) -> bool {
        ["type", "node_type", "kind", "label", "labels"]
            .iter()
            .any(|key| {
                node_data
                    .get(*key)
                    .map(|value| match value {
                        Value::String(text) => text.to_ascii_lowercase().contains("interaction"),
                        Value::Array(values) => values.iter().any(|item| {
                            item.as_str()
                                .map(|text| text.to_ascii_lowercase().contains("interaction"))
                                .unwrap_or(false)
                        }),
                        _ => false,
                    })
                    .unwrap_or(false)
            })
    }

    fn parse_node_timestamp(
        node_data: &HashMap<Cow<'static, str>, Value>,
    ) -> Option<DateTime<Utc>> {
        ["updated_at", "created_at", "timestamp"]
            .iter()
            .filter_map(|key| node_data.get(*key).and_then(Value::as_str))
            .find_map(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time.with_timezone(&Utc))
    }

    async fn recent_interaction_ids(&self) -> Result<Vec<String>, SearchError> {
        let (nodes, _) = self.graph_db.get_graph_data().await?;

        let mut interactions = nodes
            .into_iter()
            .filter(|(_, node_data)| Self::is_interaction_node(node_data))
            .map(|(node_id, node_data)| (node_id, Self::parse_node_timestamp(&node_data)))
            .collect::<Vec<_>>();

        interactions.sort_by(|left, right| right.1.cmp(&left.1));

        Ok(interactions
            .into_iter()
            .take(self.last_k)
            .map(|(node_id, _)| node_id)
            .collect())
    }

    async fn store_feedback(
        &self,
        feedback_text: &str,
        analysis: &FeedbackAnalysis,
    ) -> Result<Uuid, SearchError> {
        let feedback_id = Uuid::new_v4();

        let node = FeedbackNode {
            id: feedback_id.to_string(),
            kind: "Feedback".to_string(),
            text: feedback_text.to_string(),
            sentiment: analysis.sentiment.clone(),
            score: analysis.score,
            created_at: Utc::now().to_rfc3339(),
        };

        self.graph_db.add_node(&node).await?;

        let interaction_ids = self.recent_interaction_ids().await?;
        let edge_props = HashMap::from([
            (Cow::Borrowed("score"), json!(analysis.score)),
            (Cow::Borrowed("sentiment"), json!(analysis.sentiment)),
        ]);

        for interaction_id in interaction_ids {
            self.graph_db
                .add_edge(
                    &feedback_id.to_string(),
                    &interaction_id,
                    DEFAULT_FEEDBACK_EDGE_REL,
                    Some(edge_props.clone()),
                )
                .await?;
        }

        Ok(feedback_id)
    }
}

#[async_trait]
impl SearchRetriever for FeedbackRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::Feedback
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        let analysis = self.extract_feedback(query).await?;

        Ok(vec![SearchItem {
            id: None,
            score: Some(analysis.score),
            payload: json!({
                "feedback_text": query,
                "sentiment": analysis.sentiment,
                "score": analysis.score,
            }),
        }])
    }

    async fn get_completion(
        &self,
        query: &str,
        _context: Option<SearchContext>,
        _session: &SessionContext,
    ) -> Result<SearchOutput, SearchError> {
        let analysis = self.extract_feedback(query).await?;
        let feedback_id = self.store_feedback(query, &analysis).await?;

        Ok(SearchOutput::Ack {
            message: format!(
                "Feedback stored (id: {feedback_id}, sentiment: {}, score: {:.3})",
                analysis.sentiment, analysis.score
            ),
        })
    }
}

pub struct CodingRulesRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
    default_rule_sets: Vec<String>,
}

impl CodingRulesRetriever {
    pub fn new(graph_db: Arc<dyn GraphDBTrait>, default_rule_sets: Option<Vec<String>>) -> Self {
        Self {
            graph_db,
            default_rule_sets: default_rule_sets
                .unwrap_or_else(|| vec![DEFAULT_RULE_NODE_SET.to_string()]),
        }
    }

    fn parse_rule_sets(&self, query: &str) -> HashSet<String> {
        if query.trim().is_empty() {
            return self.default_rule_sets.iter().cloned().collect();
        }

        query
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(ToString::to_string)
            .collect()
    }

    fn is_rule_node(node_data: &HashMap<Cow<'static, str>, Value>) -> bool {
        ["type", "node_type", "kind", "label", "labels"]
            .iter()
            .any(|key| {
                node_data
                    .get(*key)
                    .map(|value| match value {
                        Value::String(text) => text.to_ascii_lowercase().contains("rule"),
                        Value::Array(values) => values.iter().any(|item| {
                            item.as_str()
                                .map(|text| text.to_ascii_lowercase().contains("rule"))
                                .unwrap_or(false)
                        }),
                        _ => false,
                    })
                    .unwrap_or(false)
            })
    }

    async fn load_rules(&self, query: &str) -> Result<Vec<Rule>, SearchError> {
        if self.graph_db.is_empty().await? {
            return Ok(vec![]);
        }

        let requested_sets = self.parse_rule_sets(query);
        let (nodes, _) = self.graph_db.get_graph_data().await?;

        let mut rules = nodes
            .into_iter()
            .filter_map(|(_node_id, node_data)| {
                if !Self::is_rule_node(&node_data) {
                    return None;
                }

                let node_set = node_data
                    .get("node_set")
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_RULE_NODE_SET)
                    .to_string();

                if !requested_sets.contains(&node_set) {
                    return None;
                }

                let text = node_data
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| node_data.get("rule").and_then(Value::as_str))
                    .unwrap_or("")
                    .trim()
                    .to_string();

                if text.is_empty() {
                    return None;
                }

                Some(Rule { node_set, text })
            })
            .collect::<Vec<_>>();

        rules.sort_by(|left, right| {
            left.node_set
                .cmp(&right.node_set)
                .then_with(|| left.text.cmp(&right.text))
        });

        Ok(rules)
    }
}

#[async_trait]
impl SearchRetriever for CodingRulesRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::CodingRules
    }

    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
        Ok(self
            .load_rules(query)
            .await?
            .into_iter()
            .map(|rule| SearchItem {
                id: None,
                score: None,
                payload: json!({
                    "node_set": rule.node_set,
                    "text": rule.text,
                }),
            })
            .collect())
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
    ) -> Result<SearchOutput, SearchError> {
        let rules = match context {
            Some(items) => items
                .into_iter()
                .filter_map(|item| {
                    Some(Rule {
                        node_set: item.payload.get("node_set")?.as_str()?.to_string(),
                        text: item.payload.get("text")?.as_str()?.to_string(),
                    })
                })
                .collect::<Vec<_>>(),
            None => self.load_rules(query).await?,
        };

        Ok(SearchOutput::Rules(rules))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::Utc;
    use cognee_graph::{GraphDBTrait, GraphDBTraitExt, MockGraphDB};
    use cognee_llm::{
        GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message, TokenUsage,
    };

    use serde::Serialize;

    use cognee_session::SessionContext;

    use super::{CodingRulesRetriever, FeedbackAnalysis, FeedbackRetriever, FeelingLuckyRetriever};
    use crate::retrievers::{SearchRetriever, SearchRetrieverRef};
    use crate::types::{SearchContext, SearchError, SearchOutput, SearchType};
    use uuid::Uuid;

    #[derive(Default)]
    struct TestLlm {
        plain_responses: Mutex<VecDeque<String>>,
        feedback_response: Option<FeedbackAnalysis>,
    }

    #[async_trait]
    impl Llm for TestLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            Ok(GenerationResponse {
                content: self
                    .plain_responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| "RAG_COMPLETION".to_string()),
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
            let response = self
                .feedback_response
                .clone()
                .ok_or_else(|| LlmError::ConfigError("missing feedback response".to_string()))?;

            serde_json::to_value(response).map_err(|error| LlmError::ConfigError(error.to_string()))
        }

        fn model(&self) -> &str {
            "test-model"
        }
    }

    struct FixedRetriever {
        kind: SearchType,
        text: String,
    }

    #[async_trait]
    impl SearchRetriever for FixedRetriever {
        fn search_type(&self) -> SearchType {
            self.kind
        }

        async fn get_context(&self, _query: &str) -> Result<SearchContext, SearchError> {
            Ok(vec![])
        }

        async fn get_completion(
            &self,
            _query: &str,
            _context: Option<SearchContext>,
            _session: &SessionContext,
        ) -> Result<SearchOutput, SearchError> {
            Ok(SearchOutput::Text(self.text.clone()))
        }
    }

    #[derive(Serialize)]
    struct TestNode {
        id: String,
        #[serde(rename = "type")]
        kind: String,
        node_set: Option<String>,
        text: Option<String>,
        created_at: Option<String>,
    }

    #[tokio::test]
    async fn feeling_lucky_falls_back_on_invalid_selection() {
        let llm = Arc::new(TestLlm {
            plain_responses: Mutex::new(VecDeque::from(["NOT_A_REAL_TYPE".to_string()])),
            feedback_response: None,
        });

        let rag: SearchRetrieverRef = Arc::new(FixedRetriever {
            kind: SearchType::RagCompletion,
            text: "fallback rag".to_string(),
        });
        let chunks: SearchRetrieverRef = Arc::new(FixedRetriever {
            kind: SearchType::Chunks,
            text: "chunks result".to_string(),
        });

        let retriever = FeelingLuckyRetriever::new(
            llm,
            HashMap::from([
                (SearchType::RagCompletion, Arc::clone(&rag)),
                (SearchType::Chunks, chunks),
            ]),
            Some(SearchType::RagCompletion),
            None,
        );

        let output = retriever
            .get_completion("hello", None, &SessionContext::default())
            .await
            .unwrap();
        match output {
            SearchOutput::Text(text) => assert_eq!(text, "fallback rag"),
            _ => panic!("expected text output"),
        }
    }

    #[tokio::test]
    async fn feedback_retriever_creates_feedback_node_and_edges() {
        let graph_db = Arc::new(MockGraphDB::new());
        graph_db
            .add_node(&TestNode {
                id: Uuid::new_v4().to_string(),
                kind: "Interaction".to_string(),
                node_set: None,
                text: Some("Q/A interaction".to_string()),
                created_at: Some(Utc::now().to_rfc3339()),
            })
            .await
            .unwrap();

        let llm = Arc::new(TestLlm {
            plain_responses: Mutex::new(VecDeque::new()),
            feedback_response: Some(FeedbackAnalysis {
                sentiment: "positive".to_string(),
                score: 0.75,
            }),
        });

        let retriever = FeedbackRetriever::new(graph_db.clone(), llm, Some(3), None);
        let output = retriever
            .get_completion("Great answer", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Ack { message } => assert!(message.contains("Feedback stored")),
            _ => panic!("expected ack output"),
        }

        let (_nodes, edges) = graph_db.get_graph_data().await.unwrap();
        assert!(
            edges
                .iter()
                .any(|(_, _, relationship, _)| relationship == "HAS_FEEDBACK")
        );
    }

    #[tokio::test]
    async fn coding_rules_retriever_returns_rules_for_requested_set() {
        let graph_db = Arc::new(MockGraphDB::new());
        graph_db
            .add_node(&TestNode {
                id: Uuid::new_v4().to_string(),
                kind: "Rule".to_string(),
                node_set: Some("coding_agent_rules".to_string()),
                text: Some("Prefer explicit error handling".to_string()),
                created_at: None,
            })
            .await
            .unwrap();
        graph_db
            .add_node(&TestNode {
                id: Uuid::new_v4().to_string(),
                kind: "Rule".to_string(),
                node_set: Some("other_rules".to_string()),
                text: Some("Unrelated rule".to_string()),
                created_at: None,
            })
            .await
            .unwrap();

        let retriever = CodingRulesRetriever::new(graph_db, None);
        let output = retriever
            .get_completion("coding_agent_rules", None, &SessionContext::default())
            .await
            .unwrap();

        match output {
            SearchOutput::Rules(rules) => {
                assert_eq!(rules.len(), 1);
                assert_eq!(rules[0].node_set, "coding_agent_rules");
                assert_eq!(rules[0].text, "Prefer explicit error handling");
            }
            _ => panic!("expected rules output"),
        }
    }
}
