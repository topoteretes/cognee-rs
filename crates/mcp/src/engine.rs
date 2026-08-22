//! Short-lived memory-engine contract shared by drains and MCP calls.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::AgentError;
use crate::event::{EventEnvelope, EventKind};

#[derive(Debug, Clone, PartialEq)]
pub enum ApplyPlan {
    SessionEntry {
        dataset: String,
        session_id: String,
        entry: Value,
        options: Value,
    },
    Remember {
        dataset: String,
        input: Value,
        options: Value,
    },
}

pub fn plan_event_application(event: &EventEnvelope) -> Result<ApplyPlan, AgentError> {
    let external_options = || json!({"externalEventId": event.external_event_id()});
    let trace =
        |origin_function: &str, method_params: Value, method_return_value: Option<Value>| {
            let mut entry = json!({
                "type": "trace",
                "originFunction": origin_function,
                "status": "success",
                "methodParams": method_params,
                "generateFeedbackWithLlm": false
            });
            if let Some(method_return_value) = method_return_value {
                entry["methodReturnValue"] = method_return_value;
            }
            ApplyPlan::SessionEntry {
                dataset: event.dataset.clone(),
                session_id: event.session_id.clone(),
                entry,
                options: external_options(),
            }
        };

    Ok(match event.event {
        EventKind::SessionStart => trace("apex.session_start", event.payload.clone(), None),
        EventKind::BeforeAgent => trace(
            "apex.before_agent",
            json!({"prompt": required_value(&event.payload, "prompt")?}),
            None,
        ),
        EventKind::AfterTool => trace(
            required_string(&event.payload, "tool_name")?,
            required_value(&event.payload, "tool_input")?,
            Some(required_value(&event.payload, "tool_response")?),
        ),
        EventKind::AfterAgent => ApplyPlan::SessionEntry {
            dataset: event.dataset.clone(),
            session_id: event.session_id.clone(),
            entry: json!({
                "type": "qa",
                "question": required_value(&event.payload, "prompt")?,
                "answer": required_value(&event.payload, "prompt_response")?,
                "context": ""
            }),
            options: external_options(),
        },
        EventKind::PreCompress => trace("apex.pre_compress", event.payload.clone(), None),
        EventKind::SessionEnd => trace("apex.session_end", event.payload.clone(), None),
        EventKind::McpRemember => {
            let mut options = external_options();
            options["selfImprovement"] = Value::Bool(
                event
                    .payload
                    .get("self_improvement")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            );
            if let Some(session_id) = event.payload.get("session_id").and_then(Value::as_str) {
                options["sessionId"] = Value::String(session_id.to_owned());
            }
            ApplyPlan::Remember {
                dataset: event.dataset.clone(),
                input: json!({
                    "type": "text",
                    "text": required_value(&event.payload, "data")?
                }),
                options,
            }
        }
    })
}

fn required_value(payload: &Value, field: &'static str) -> Result<Value, AgentError> {
    payload
        .get(field)
        .cloned()
        .ok_or(AgentError::InvalidEvent(field))
}

fn required_string<'a>(payload: &'a Value, field: &'static str) -> Result<&'a str, AgentError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(AgentError::InvalidEvent(field))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReceipt {
    pub entry_id: Option<String>,
}

impl ApplyReceipt {
    pub fn new(entry_id: Option<String>) -> Self {
        Self { entry_id }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImproveReceipt {
    pub sessions_persisted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallRequest {
    pub query: String,
    pub dataset: String,
    pub session_id: Option<String>,
    pub top_k: usize,
    pub search_type: Option<String>,
    pub auto_route: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallItem {
    pub source: RecallSource,
    pub content: String,
    pub score: Option<f64>,
    pub dataset: String,
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
    pub event_id: Option<String>,
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecallSource {
    Pending,
    Session,
    Graph,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecallResponse {
    pub items: Vec<RecallItem>,
    pub search_type_used: Option<String>,
    pub auto_routed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForgetTarget {
    Dataset(String),
    All,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetReceipt {
    pub target: String,
}

#[async_trait]
pub trait MemoryEngine: Send {
    async fn contains_event(&mut self, dataset: &str, event_id: &str) -> Result<bool, AgentError>;

    async fn contains_event_for(&mut self, event: &EventEnvelope) -> Result<bool, AgentError> {
        self.contains_event(&event.dataset, &event.external_event_id())
            .await
    }

    async fn apply_event(&mut self, event: &EventEnvelope) -> Result<ApplyReceipt, AgentError>;

    async fn improve(
        &mut self,
        dataset: &str,
        session_ids: &[String],
    ) -> Result<ImproveReceipt, AgentError>;

    async fn recall(&mut self, request: RecallRequest) -> Result<RecallResponse, AgentError>;

    async fn forget(&mut self, target: ForgetTarget) -> Result<ForgetReceipt, AgentError>;

    async fn close(self: Box<Self>);
}

#[async_trait]
pub trait EngineFactory: Send + Sync {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, AgentError>;
}

#[cfg(feature = "engine")]
#[derive(Clone)]
pub struct CogneeEngineFactory {
    config: crate::config::AgentConfig,
    generation: crate::embedding_generation::EmbeddingGeneration,
}

#[cfg(feature = "engine")]
impl CogneeEngineFactory {
    pub fn new(
        config: crate::config::AgentConfig,
        generation: crate::embedding_generation::EmbeddingGeneration,
    ) -> Self {
        Self { config, generation }
    }
}

#[cfg(feature = "engine")]
#[async_trait]
impl EngineFactory for CogneeEngineFactory {
    async fn open(&self) -> Result<Box<dyn MemoryEngine>, AgentError> {
        let settings = self
            .config
            .cognee_settings(&self.generation)
            .map_err(|_| AgentError::Blocked("configuration_drift"))?;
        Ok(Box::new(CogneeMemoryEngine {
            state: cognee_bindings_common::HandleState::from_settings(settings),
        }))
    }
}

#[cfg(feature = "engine")]
struct CogneeMemoryEngine {
    state: cognee_bindings_common::HandleState,
}

#[cfg(feature = "engine")]
#[async_trait]
impl MemoryEngine for CogneeMemoryEngine {
    async fn contains_event(&mut self, dataset: &str, event_id: &str) -> Result<bool, AgentError> {
        cognee_bindings_common::ops::memory::run_contains_external_event(
            &self.state,
            dataset,
            None,
            event_id,
        )
        .await
        .map_err(classify_sdk_error)
    }

    async fn contains_event_for(&mut self, event: &EventEnvelope) -> Result<bool, AgentError> {
        let session_id = match event.event {
            EventKind::McpRemember => event.payload.get("session_id").and_then(Value::as_str),
            _ => Some(event.session_id.as_str()),
        };
        cognee_bindings_common::ops::memory::run_contains_external_event(
            &self.state,
            &event.dataset,
            session_id,
            &event.external_event_id(),
        )
        .await
        .map_err(classify_sdk_error)
    }

    async fn apply_event(&mut self, event: &EventEnvelope) -> Result<ApplyReceipt, AgentError> {
        let result = match plan_event_application(event)? {
            ApplyPlan::SessionEntry {
                dataset,
                session_id,
                entry,
                options,
            } => cognee_bindings_common::ops::memory::run_remember_entry(
                &self.state,
                entry,
                &dataset,
                &session_id,
                &options,
            )
            .await
            .map_err(classify_sdk_error)?,
            ApplyPlan::Remember {
                dataset,
                input,
                options,
            } => cognee_bindings_common::ops::memory::run_remember(
                &self.state,
                input,
                &dataset,
                &options,
            )
            .await
            .map_err(classify_sdk_error)?,
        };
        let entry_id = result
            .get("entry_id")
            .or_else(|| result.get("pipeline_run_id"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(ApplyReceipt::new(entry_id))
    }

    async fn improve(
        &mut self,
        dataset: &str,
        session_ids: &[String],
    ) -> Result<ImproveReceipt, AgentError> {
        let result = cognee_bindings_common::ops::memory::run_improve(
            &self.state,
            &json!({"datasetName": dataset, "sessionIds": session_ids}),
        )
        .await
        .map_err(classify_sdk_error)?;
        Ok(ImproveReceipt {
            sessions_persisted: result
                .get("sessionsPersisted")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or_default(),
        })
    }

    async fn recall(&mut self, request: RecallRequest) -> Result<RecallResponse, AgentError> {
        let mut options = json!({
            "datasets": [request.dataset],
            "topK": request.top_k,
            "autoRoute": request.auto_route,
        });
        if let Some(session_id) = request.session_id.as_deref() {
            options["sessionId"] = Value::String(session_id.to_owned());
        }
        if let Some(search_type) = request.search_type.as_deref() {
            options["searchType"] = Value::String(search_type.to_owned());
        }
        let result =
            cognee_bindings_common::ops::retrieval::recall(&self.state, &request.query, &options)
                .await
                .map_err(classify_sdk_error)?;
        let items = result
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|item| normalize_recall_item(item, &request))
            .collect();
        Ok(RecallResponse {
            items,
            search_type_used: result
                .get("searchTypeUsed")
                .and_then(Value::as_str)
                .map(str::to_owned),
            auto_routed: result
                .get("autoRouted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    async fn forget(&mut self, target: ForgetTarget) -> Result<ForgetReceipt, AgentError> {
        let target_json = match target {
            ForgetTarget::Dataset(name) => {
                json!({"kind": "dataset", "dataset": {"name": name}})
            }
            ForgetTarget::All => json!({"kind": "all"}),
        };
        let result =
            cognee_bindings_common::ops::data::forget(&self.state, target_json, &Value::Null)
                .await
                .map_err(classify_sdk_error)?;
        Ok(ForgetReceipt {
            target: result
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }

    async fn close(self: Box<Self>) {
        self.state.close().await;
    }
}

#[cfg(feature = "engine")]
pub fn normalize_recall_item(item: &Value, request: &RecallRequest) -> RecallItem {
    let raw_source = item
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("graph");
    let source = if matches!(raw_source, "session" | "trace") {
        RecallSource::Session
    } else {
        RecallSource::Graph
    };
    let content = match item.get("content") {
        Some(Value::String(content)) => content.clone(),
        Some(content) => crate::event::canonical_json(content),
        None => String::new(),
    };
    let mut metadata = serde_json::Map::new();
    collect_reference_metadata(item.get("content").unwrap_or(&Value::Null), &mut metadata);
    let event_id = metadata
        .get("cognee_external_event_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    RecallItem {
        source,
        content,
        score: item.get("score").and_then(Value::as_f64),
        dataset: request.dataset.clone(),
        session_id: request.session_id.clone(),
        timestamp: None,
        event_id,
        metadata,
    }
}

#[cfg(feature = "engine")]
fn collect_reference_metadata(value: &Value, output: &mut serde_json::Map<String, Value>) {
    const KEYS: [&str; 6] = [
        "cognee_external_event_id",
        "reference_source_id",
        "reference_revision",
        "reference_label",
        "reference_content_type",
        "reference_content_sha256",
    ];
    match value {
        Value::Object(values) => {
            for key in KEYS {
                if let Some(value) = values.get(key) {
                    output
                        .entry(key.to_owned())
                        .or_insert_with(|| value.clone());
                }
            }
            if let Some(value) = values.get("content_type") {
                output
                    .entry("reference_content_type".to_owned())
                    .or_insert_with(|| value.clone());
            }
            if let Some(value) = values.get("content_sha256") {
                output
                    .entry("reference_content_sha256".to_owned())
                    .or_insert_with(|| value.clone());
            }
            for value in values.values() {
                collect_reference_metadata(value, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_reference_metadata(value, output);
            }
        }
        Value::String(value)
            if value.trim_start().starts_with('{') || value.trim_start().starts_with('[') =>
        {
            if let Ok(nested) = serde_json::from_str::<Value>(value) {
                collect_reference_metadata(&nested, output);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[cfg(feature = "engine")]
fn classify_sdk_error(error: cognee_bindings_common::SdkError) -> AgentError {
    use cognee_bindings_common::SdkError;

    let message = error.to_string().to_ascii_lowercase();
    if message.contains("429")
        || message.contains("rate limit")
        || message.contains("too many requests")
    {
        return AgentError::Retryable("proxy_429");
    }
    if message.contains("timeout") || message.contains("timed out") {
        return AgentError::Retryable("timeout");
    }
    if ["500", "502", "503", "504", "5xx"]
        .iter()
        .any(|status| message.contains(status))
    {
        return AgentError::Retryable("upstream_5xx");
    }
    match error {
        SdkError::Validation(_) | SdkError::Unsupported(_) => AgentError::Blocked("schema_drift"),
        SdkError::FeatureNotBuilt(_) => AgentError::Blocked("feature_not_built"),
        SdkError::Component(_)
        | SdkError::ServiceBuild(_)
        | SdkError::UserBootstrap(_)
        | SdkError::Runtime(_) => AgentError::Retryable("engine_unavailable"),
    }
}
