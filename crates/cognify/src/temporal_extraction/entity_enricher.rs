use cognee_llm::{GenerationOptions, Llm, LlmExt};
use cognee_models::{EventAttribute, TemporalEvent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;

use crate::CognifyError;

const TEMPORAL_ENTITY_ENRICHMENT_PROMPT: &str =
    include_str!("prompts/temporal_entity_enrichment.txt");

/// LLM output for a single enriched event.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct RawEnrichedEvent {
    pub event_name: String,
    #[serde(default)]
    pub attributes: Vec<RawAttribute>,
}

/// Object wrapper for structured-output APIs that require a root JSON object.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct RawEnrichedEventsOutput {
    #[serde(default)]
    pub events: Vec<RawEnrichedEvent>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct RawAttribute {
    pub entity: String,
    pub entity_type: String,
    pub relationship: String,
}

pub struct TemporalEntityEnricher {
    pub(crate) llm: Arc<dyn Llm>,
}

impl TemporalEntityEnricher {
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }

    /// Enrich a batch of events with typed entity attributes.
    ///
    /// Returns the same events, in the same order, with `.attributes`
    /// populated. Callers rely on that order to keep each event next to the
    /// data item that produced it.
    ///
    /// An LLM or parse failure is *returned*, not swallowed. Handing the events
    /// back unenriched looked like graceful degradation but produced a
    /// half-cognified item — every event stripped of its entity edges — that no
    /// retry could tell from a complete one. Python propagates here too
    /// (`extract_knowledge_graph_from_events` awaits `enrich_events` bare).
    pub async fn enrich(
        &self,
        mut events: Vec<TemporalEvent>,
    ) -> Result<Vec<TemporalEvent>, CognifyError> {
        if events.is_empty() {
            return Ok(events);
        }

        // Build the user prompt: serialise event name + description as the input list.
        let input: Vec<serde_json::Value> = events
            .iter()
            .map(|e| {
                serde_json::json!({
                    "event_name": e.name,
                    "description": e.description,
                })
            })
            .collect();

        let user_prompt = serde_json::to_string(&input)
            .map_err(|e| CognifyError::SerializationError(e.to_string()))?;

        // Python parity: `acreate_structured_output` passes no output cap on
        // extraction, so responses use the model's full default budget. A small
        // max_tokens truncates large enriched-event lists mid-JSON. Leave None.
        let options = GenerationOptions {
            temperature: Some(0.1),
            max_tokens: None,
            ..Default::default()
        };

        // The `warn!` this used to emit lives at the call site now, next to the
        // `failures.record(...)` that names the batch's chunks.
        let enriched: RawEnrichedEventsOutput = self
            .llm
            .create_structured_output::<RawEnrichedEventsOutput>(
                &user_prompt,
                TEMPORAL_ENTITY_ENRICHMENT_PROMPT,
                Some(options),
            )
            .await
            .map_err(|e| CognifyError::FactExtractionError(e.to_string()))?;

        // Match enriched entries back to events by name (same approach as Python).
        let enriched_map: HashMap<String, Vec<EventAttribute>> = enriched
            .events
            .into_iter()
            .map(|r| {
                let attrs = r
                    .attributes
                    .into_iter()
                    .map(|a| EventAttribute {
                        entity: a.entity,
                        entity_type: a.entity_type,
                        relationship: a.relationship,
                    })
                    .collect();
                (r.event_name, attrs)
            })
            .collect();

        for event in &mut events {
            if let Some(attrs) = enriched_map.get(&event.name) {
                event.attributes = attrs.clone();
            }
        }

        Ok(events)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cognee_llm::error::{LlmError, LlmResult};
    use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
    use cognee_models::TemporalEvent;
    use serde_json::Value;

    /// Mock LLM that returns a pre-configured JSON value from
    /// `create_structured_output_with_messages_raw`.
    struct MockLlm {
        response: Result<Value, String>,
    }

    impl MockLlm {
        fn with_json(value: Value) -> Self {
            Self {
                response: Ok(value),
            }
        }

        fn with_error(msg: &str) -> Self {
            Self {
                response: Err(msg.to_string()),
            }
        }
    }

    #[async_trait]
    impl Llm for MockLlm {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<GenerationResponse> {
            unimplemented!("not used in entity_enricher tests")
        }

        async fn create_structured_output_with_messages_raw(
            &self,
            _messages: Vec<Message>,
            _json_schema: &Value,
            _options: Option<GenerationOptions>,
        ) -> LlmResult<Value> {
            match &self.response {
                Ok(v) => Ok(v.clone()),
                Err(msg) => Err(LlmError::ApiError(msg.clone())),
            }
        }

        fn model(&self) -> &str {
            "mock-llm"
        }
    }

    fn make_event(name: &str) -> TemporalEvent {
        TemporalEvent {
            name: name.to_string(),
            description: Some(format!("Description of {name}")),
            location: None,
            at: None,
            during: None,
            attributes: vec![],
        }
    }

    #[tokio::test]
    async fn enrich_populates_attributes() {
        let json = serde_json::json!({
            "events": [
                {
                    "event_name": "Moon Landing",
                    "attributes": [
                        { "entity": "Neil Armstrong", "entity_type": "Person", "relationship": "participant" },
                        { "entity": "NASA", "entity_type": "Organization", "relationship": "organizer" }
                    ]
                }
            ]
        });

        let llm = Arc::new(MockLlm::with_json(json));
        let enricher = TemporalEntityEnricher::new(llm);

        let events = vec![make_event("Moon Landing")];
        let result = enricher.enrich(events).await.unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].attributes.len(), 2);
        assert_eq!(result[0].attributes[0].entity, "Neil Armstrong");
        assert_eq!(result[0].attributes[0].entity_type, "Person");
        assert_eq!(result[0].attributes[0].relationship, "participant");
        assert_eq!(result[0].attributes[1].entity, "NASA");
        assert_eq!(result[0].attributes[1].entity_type, "Organization");
        assert_eq!(result[0].attributes[1].relationship, "organizer");
    }

    /// Returning the events unenriched read as a deliberate softening, but an
    /// event with no attributes has none of the entity nodes or edges the
    /// enrichment pass exists to produce — a partially-cognified item that no
    /// later run can distinguish from a finished one. The failure surfaces now
    /// and the caller decides how tolerant to be.
    #[tokio::test]
    async fn enrich_surfaces_the_llm_error() {
        let llm = Arc::new(MockLlm::with_error("service unavailable"));
        let enricher = TemporalEntityEnricher::new(llm);

        let events = vec![make_event("Moon Landing")];
        let error = enricher
            .enrich(events)
            .await
            .expect_err("an LLM failure must reach the caller");
        assert!(
            matches!(error, CognifyError::FactExtractionError(ref msg) if msg.contains("service unavailable")),
            "the error names what actually went wrong: {error}"
        );
    }

    #[tokio::test]
    async fn enrich_matches_by_name() {
        // LLM only returns enrichment for "Event A", not "Event B".
        let json = serde_json::json!({
            "events": [
                {
                    "event_name": "Event A",
                    "attributes": [
                        { "entity": "Alice", "entity_type": "Person", "relationship": "subject" }
                    ]
                }
            ]
        });

        let llm = Arc::new(MockLlm::with_json(json));
        let enricher = TemporalEntityEnricher::new(llm);

        let events = vec![make_event("Event A"), make_event("Event B")];
        let result = enricher.enrich(events).await.unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].attributes.len(), 1, "Event A should be enriched");
        assert_eq!(result[0].attributes[0].entity, "Alice");
        assert!(
            result[1].attributes.is_empty(),
            "Event B should remain unenriched"
        );
    }

    #[tokio::test]
    async fn enrich_empty_events() {
        // Even though we provide a mock, it should never be called for empty input.
        // But the function should return Ok(vec![]) regardless.
        let json = serde_json::json!({ "events": [] });
        let llm = Arc::new(MockLlm::with_json(json));
        let enricher = TemporalEntityEnricher::new(llm);

        let result = enricher.enrich(vec![]).await.unwrap();
        assert!(result.is_empty());
    }
}
