use std::sync::Arc;

use cognee_llm::{GenerationOptions, Llm, LlmExt};
use cognee_models::{CognifyInterval, RawExtractedTimestamp, TemporalEvent, to_cognify_timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::CognifyError;

const TEMPORAL_EVENT_EXTRACTION_PROMPT: &str =
    include_str!("prompts/temporal_event_extraction.txt");

/// Raw event as returned by the LLM.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct RawEvent {
    pub name: String,
    pub description: Option<String>,
    pub time_from: Option<RawExtractedTimestamp>,
    pub time_to: Option<RawExtractedTimestamp>,
    pub location: Option<String>,
}

pub struct TemporalEventExtractor {
    pub(crate) llm: Arc<dyn Llm>,
}

impl TemporalEventExtractor {
    pub fn new(llm: Arc<dyn Llm>) -> Self {
        Self { llm }
    }

    /// Extract events from a single chunk of text.
    /// Returns an empty Vec (with a warning log) on LLM or parse errors
    /// — extraction failures must not abort the cognify pipeline.
    pub async fn extract_events(
        &self,
        chunk_text: &str,
    ) -> Result<Vec<TemporalEvent>, CognifyError> {
        let options = GenerationOptions {
            temperature: Some(0.1),
            max_tokens: Some(4000),
            ..Default::default()
        };

        let raw: Vec<RawEvent> = match self
            .llm
            .create_structured_output::<Vec<RawEvent>>(
                chunk_text,
                TEMPORAL_EVENT_EXTRACTION_PROMPT,
                Some(options),
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Temporal event extraction failed: {e}");
                return Ok(vec![]);
            }
        };

        let events = raw.into_iter().filter_map(convert_raw_event).collect();

        Ok(events)
    }
}

fn convert_raw_event(raw: RawEvent) -> Option<TemporalEvent> {
    if raw.name.trim().is_empty() {
        return None;
    }

    // If both bounds are present, build an Interval instead of a single point.
    let (at, during) = match (raw.time_from, raw.time_to) {
        (Some(from), Some(to)) => {
            let ts_from = to_cognify_timestamp(from)?;
            let ts_to = to_cognify_timestamp(to)?;
            (
                None,
                Some(CognifyInterval {
                    time_from: ts_from,
                    time_to: ts_to,
                }),
            )
        }
        (Some(from), None) => (to_cognify_timestamp(from), None),
        (None, Some(to)) => (to_cognify_timestamp(to), None),
        (None, None) => (None, None),
    };

    Some(TemporalEvent {
        name: raw.name,
        description: raw.description,
        location: raw.location,
        at,
        during,
        attributes: vec![], // populated by Phase 4
    })
}
