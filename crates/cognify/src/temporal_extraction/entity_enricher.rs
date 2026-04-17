use std::collections::HashMap;
use std::sync::Arc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json;
use cognee_llm::{GenerationOptions, Llm, LlmExt};
use cognee_models::{EventAttribute, TemporalEvent};

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
    /// Returns the same events with `.attributes` populated.
    /// On LLM or parse failure: returns the original events unchanged (warns, does not error).
    pub async fn enrich(
        &self,
        mut events: Vec<TemporalEvent>,
    ) -> Result<Vec<TemporalEvent>, CognifyError> {
        // Build the user prompt: serialise event name + description as the input list.
        let input: Vec<serde_json::Value> = events
            .iter()
            .map(|e| serde_json::json!({
                "event_name": e.name,
                "description": e.description,
            }))
            .collect();

        let user_prompt = serde_json::to_string(&input)
            .map_err(|e| CognifyError::SerializationError(e.to_string()))?;

        let options = GenerationOptions {
            temperature: Some(0.1),
            max_tokens: Some(8000),
            ..Default::default()
        };

        let enriched: Vec<RawEnrichedEvent> = match self
            .llm
            .create_structured_output::<Vec<RawEnrichedEvent>>(
                &user_prompt,
                TEMPORAL_ENTITY_ENRICHMENT_PROMPT,
                Some(options),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "Entity enrichment failed: {e}. Events returned without attributes."
                );
                return Ok(events);
            }
        };

        // Match enriched entries back to events by name (same approach as Python).
        let enriched_map: HashMap<String, Vec<EventAttribute>> = enriched
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
