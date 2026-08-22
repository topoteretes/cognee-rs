use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(feature = "engine")]
use super::REFERENCE_DATASET;
use super::ReferenceError;
use crate::engine::{RecallRequest, RecallResponse};

#[cfg(feature = "engine")]
const REFERENCE_EMBEDDING_MODEL: &str = "text-embedding-3-large";
#[cfg(feature = "engine")]
const REFERENCE_EMBEDDING_DIMENSIONS: u32 = 3072;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceProviderFingerprint {
    pub provider: String,
    pub endpoint_class: String,
    pub model: String,
    pub dimensions: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceEngineIdentity {
    pub cognee_rs_commit: String,
    pub adapter_version: String,
    pub user_agent: String,
    pub llm: ReferenceProviderFingerprint,
    pub embedding: ReferenceProviderFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceEngineOpen {
    pub root: PathBuf,
    pub dataset: String,
    pub read_only: bool,
    pub user_agent: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReferenceEngineInput {
    pub content: String,
    pub label: String,
    pub external_metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceRecallProbe {
    pub query: String,
    pub expected_event_id: String,
}

#[async_trait]
pub trait ReferenceWriteEngine: Send {
    async fn add_and_cognify(
        &mut self,
        dataset: &str,
        inputs: Vec<ReferenceEngineInput>,
    ) -> Result<(), ReferenceError>;

    async fn close(self: Box<Self>) -> Result<(), ReferenceError>;
}

#[async_trait]
pub trait ReferenceReadEngine: Send {
    async fn recall_contains(
        &mut self,
        dataset: &str,
        probe: &ReferenceRecallProbe,
    ) -> Result<bool, ReferenceError>;

    async fn recall(&mut self, _request: RecallRequest) -> Result<RecallResponse, ReferenceError> {
        Err(ReferenceError::Unavailable)
    }

    async fn close(self: Box<Self>) -> Result<(), ReferenceError>;
}

#[async_trait]
pub trait ReferenceEngineFactory: Send + Sync {
    fn identity(&self) -> ReferenceEngineIdentity;

    async fn open_writer(
        &self,
        request: &ReferenceEngineOpen,
    ) -> Result<Box<dyn ReferenceWriteEngine>, ReferenceError>;

    async fn open_reader(
        &self,
        request: &ReferenceEngineOpen,
    ) -> Result<Box<dyn ReferenceReadEngine>, ReferenceError>;
}

#[cfg(feature = "engine")]
#[derive(Clone)]
pub struct CogneeReferenceEngineFactory {
    config: crate::config::AgentConfig,
    identity: ReferenceEngineIdentity,
}

#[cfg(feature = "engine")]
impl CogneeReferenceEngineFactory {
    pub fn new(config: crate::config::AgentConfig) -> Result<Self, ReferenceError> {
        let embedding = config
            .embedding
            .as_ref()
            .ok_or(ReferenceError::Unavailable)?;
        if config.llm.provider.trim().is_empty()
            || config.llm.endpoint.trim().is_empty()
            || config.llm.model.trim().is_empty()
            || embedding.provider.trim().is_empty()
            || embedding.endpoint.trim().is_empty()
            || embedding.model != REFERENCE_EMBEDDING_MODEL
            || embedding.dimensions != REFERENCE_EMBEDDING_DIMENSIONS
            || config.proxy_key().is_empty()
        {
            return Err(ReferenceError::Unavailable);
        }
        let identity = ReferenceEngineIdentity {
            cognee_rs_commit: env!("COGNEE_RS_COMMIT").to_owned(),
            adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
            user_agent: crate::config::apex_user_agent(),
            llm: ReferenceProviderFingerprint {
                provider: config.llm.provider.clone(),
                endpoint_class: crate::config::endpoint_class(&config.llm.endpoint),
                model: config.llm.model.clone(),
                dimensions: None,
            },
            embedding: ReferenceProviderFingerprint {
                provider: embedding.provider.clone(),
                endpoint_class: crate::config::endpoint_class(&embedding.endpoint),
                model: embedding.model.clone(),
                dimensions: Some(embedding.dimensions),
            },
        };
        Ok(Self { config, identity })
    }

    pub fn settings_for(
        &self,
        request: &ReferenceEngineOpen,
    ) -> Result<cognee::config::Settings, ReferenceError> {
        if request.dataset != REFERENCE_DATASET
            || request.user_agent != self.identity.user_agent
            || request.root.as_os_str().is_empty()
        {
            return Err(ReferenceError::InvalidInput);
        }
        let embedding = self
            .config
            .embedding
            .as_ref()
            .ok_or(ReferenceError::Unavailable)?;
        let data = request.root.join("data");
        Ok(cognee::config::Settings {
            read_only: request.read_only,
            system_root_directory: request.root.join("system").display().to_string(),
            data_root_directory: data.display().to_string(),
            cache_root_directory: request.root.join("cache").display().to_string(),
            logs_root_directory: request.root.join("logs").display().to_string(),
            db_provider: "sqlite".into(),
            relational_db_url: format!("sqlite://{}?mode=rwc", data.join("cognee.db").display()),
            vector_db_provider: "lancedb".into(),
            vector_db_url: request.root.join("vector").display().to_string(),
            graph_database_provider: "ladybug".into(),
            graph_file_path: request.root.join("graph").display().to_string(),
            cache_backend: "seaorm".into(),
            default_dataset_name: REFERENCE_DATASET.to_owned(),
            llm_provider: self.config.llm.provider.clone(),
            llm_model: self.config.llm.model.clone(),
            llm_endpoint: self.config.llm.endpoint.clone(),
            llm_api_key: self.config.proxy_key().expose().to_owned(),
            user_agent: request.user_agent.clone(),
            llm_max_parallel_requests: 1,
            llm_max_retries: 0,
            embedding_provider: embedding.provider.clone(),
            embedding_model_name: embedding.model.clone(),
            embedding_dimensions: embedding.dimensions,
            embedding_endpoint: embedding.endpoint.clone(),
            embedding_api_key: self.config.proxy_key().expose().to_owned(),
            embedding_batch_size: self.config.limits.embedding_batch_size,
            ..Default::default()
        })
    }
}

#[cfg(feature = "engine")]
#[async_trait]
impl ReferenceEngineFactory for CogneeReferenceEngineFactory {
    fn identity(&self) -> ReferenceEngineIdentity {
        self.identity.clone()
    }

    async fn open_writer(
        &self,
        request: &ReferenceEngineOpen,
    ) -> Result<Box<dyn ReferenceWriteEngine>, ReferenceError> {
        if request.read_only {
            return Err(ReferenceError::ReadOnly);
        }
        Ok(Box::new(CogneeReferenceWriteEngine {
            state: cognee_bindings_common::HandleState::from_settings(self.settings_for(request)?),
        }))
    }

    async fn open_reader(
        &self,
        request: &ReferenceEngineOpen,
    ) -> Result<Box<dyn ReferenceReadEngine>, ReferenceError> {
        if !request.read_only {
            return Err(ReferenceError::InvalidInput);
        }
        Ok(Box::new(CogneeReferenceReadEngine {
            state: cognee_bindings_common::HandleState::from_settings(self.settings_for(request)?),
        }))
    }
}

#[cfg(feature = "engine")]
struct CogneeReferenceWriteEngine {
    state: cognee_bindings_common::HandleState,
}

#[cfg(feature = "engine")]
#[async_trait]
impl ReferenceWriteEngine for CogneeReferenceWriteEngine {
    async fn add_and_cognify(
        &mut self,
        dataset: &str,
        inputs: Vec<ReferenceEngineInput>,
    ) -> Result<(), ReferenceError> {
        use cognee::models::DataInput;
        use cognee_bindings_common::ops::pipeline::{
            existing_data_ids, partition_added, resolve_dataset, run_cognify_on_items,
        };

        if dataset != REFERENCE_DATASET || inputs.is_empty() {
            return Err(ReferenceError::InvalidInput);
        }
        let inputs = inputs
            .into_iter()
            .map(|input| {
                let metadata = serde_json::to_string(&input.external_metadata)
                    .map_err(|_| ReferenceError::CorruptRecord)?;
                Ok(DataInput::DataItem {
                    data: Box::new(DataInput::Text(input.content)),
                    label: input.label,
                    external_metadata: Some(metadata),
                })
            })
            .collect::<Result<Vec<_>, ReferenceError>>()?;

        let services = self
            .state
            .services()
            .await
            .map_err(|_| ReferenceError::Unavailable)?;
        let owner_id = self
            .state
            .owner_id()
            .await
            .map_err(|_| ReferenceError::Unavailable)?;
        let existing = existing_data_ids(&services, dataset, owner_id, None)
            .await
            .map_err(|_| ReferenceError::Unavailable)?;
        let returned = services
            .add_pipeline
            .add(inputs, dataset, owner_id, None)
            .await
            .map_err(|_| ReferenceError::Unavailable)?;
        let (new_items, _) = partition_added(returned, &existing);
        if new_items.is_empty() {
            return Ok(());
        }
        let resolved = resolve_dataset(&services, dataset, owner_id, None)
            .await
            .map_err(|_| ReferenceError::Unavailable)?;
        run_cognify_on_items(&services, &resolved, owner_id, new_items, &Value::Null)
            .await
            .map_err(|_| ReferenceError::Unavailable)?;
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), ReferenceError> {
        self.state.close().await;
        Ok(())
    }
}

#[cfg(feature = "engine")]
struct CogneeReferenceReadEngine {
    state: cognee_bindings_common::HandleState,
}

#[cfg(feature = "engine")]
#[async_trait]
impl ReferenceReadEngine for CogneeReferenceReadEngine {
    async fn recall_contains(
        &mut self,
        dataset: &str,
        probe: &ReferenceRecallProbe,
    ) -> Result<bool, ReferenceError> {
        if dataset != REFERENCE_DATASET {
            return Err(ReferenceError::InvalidInput);
        }
        let options = serde_json::json!({
            "datasets": [dataset],
            "searchType": "CHUNKS",
            "topK": 3,
            "autoRoute": false,
            "saveInteraction": false,
        });
        let result =
            cognee_bindings_common::ops::retrieval::recall(&self.state, &probe.query, &options)
                .await
                .map_err(|_| ReferenceError::Unavailable)?;
        Ok(recall_result_contains_probe(&result, probe))
    }

    async fn recall(&mut self, request: RecallRequest) -> Result<RecallResponse, ReferenceError> {
        if request.dataset != REFERENCE_DATASET {
            return Err(ReferenceError::InvalidInput);
        }
        let mut options = serde_json::json!({
            "datasets": [request.dataset],
            "topK": request.top_k,
            "autoRoute": false,
            "saveInteraction": false,
        });
        if let Some(search_type) = request.search_type.as_deref() {
            options["searchType"] = Value::String(search_type.to_owned());
        }
        let result =
            cognee_bindings_common::ops::retrieval::recall(&self.state, &request.query, &options)
                .await
                .map_err(|_| ReferenceError::Unavailable)?;
        let items = result
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|item| crate::engine::normalize_recall_item(item, &request))
            .collect();
        Ok(RecallResponse {
            items,
            search_type_used: result
                .get("searchTypeUsed")
                .and_then(Value::as_str)
                .map(str::to_owned),
            auto_routed: false,
        })
    }

    async fn close(self: Box<Self>) -> Result<(), ReferenceError> {
        self.state.close().await;
        Ok(())
    }
}

#[cfg(feature = "engine")]
fn recall_result_contains_probe(value: &Value, probe: &ReferenceRecallProbe) -> bool {
    if value.get("searchTypeUsed").and_then(Value::as_str) != Some("CHUNKS") {
        return false;
    }
    let Some(items) = value.get("items").and_then(Value::as_array) else {
        return false;
    };
    items.iter().any(|item| {
        if item.get("source").and_then(Value::as_str) != Some("graph") {
            return false;
        }
        let Some(content) = item.get("content") else {
            return false;
        };
        json_contains_string(content, &probe.expected_event_id)
            || (!probe.query.is_empty()
                && content
                    .pointer("/payload/text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.contains(&probe.query)))
    })
}

#[cfg(feature = "engine")]
fn json_contains_string(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(value) => value.contains(expected),
        Value::Array(values) => values
            .iter()
            .any(|value| json_contains_string(value, expected)),
        Value::Object(values) => values
            .values()
            .any(|value| json_contains_string(value, expected)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

#[cfg(all(test, feature = "engine"))]
mod tests {
    use serde_json::json;

    use super::{ReferenceRecallProbe, recall_result_contains_probe};

    #[test]
    fn serialized_chunks_response_matches_relevant_content_without_provenance() {
        let response = json!({
            "items": [{
                "source": "graph",
                "content": {
                    "id": "30d5ce68-63a4-42bb-a64d-07ca7d1a7c4a",
                    "score": 0.98,
                    "payload": {
                        "type": "DocumentChunk",
                        "text": "The APEX reference sentinel is cobalt-orchid-742.",
                        "dataset_id": "bd91b9e2-0f28-43b8-8fe5-10a602b0fef3"
                    }
                },
                "score": 1.0
            }],
            "searchTypeUsed": "CHUNKS",
            "autoRouted": false,
            "searchResponse": {
                "search_type": "CHUNKS",
                "result": {
                    "kind": "Items",
                    "data": [{
                        "id": "30d5ce68-63a4-42bb-a64d-07ca7d1a7c4a",
                        "score": 0.98,
                        "payload": {
                            "type": "DocumentChunk",
                            "text": "The APEX reference sentinel is cobalt-orchid-742.",
                            "dataset_id": "bd91b9e2-0f28-43b8-8fe5-10a602b0fef3"
                        }
                    }]
                }
            }
        });
        let relevant = ReferenceRecallProbe {
            query: "APEX reference sentinel is cobalt-orchid-742".to_owned(),
            expected_event_id: "event-id-not-present-in-chunks".to_owned(),
        };
        let unrelated = ReferenceRecallProbe {
            query: "different source content".to_owned(),
            expected_event_id: "event-id-not-present-in-chunks".to_owned(),
        };

        assert!(recall_result_contains_probe(&response, &relevant));
        assert!(!recall_result_contains_probe(&response, &unrelated));
    }
}
