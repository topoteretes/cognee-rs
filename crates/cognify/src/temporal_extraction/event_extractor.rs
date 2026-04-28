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

/// Object wrapper for structured-output APIs that require a root JSON object.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct RawEventsOutput {
    #[serde(default)]
    pub events: Vec<RawEvent>,
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

        let raw: RawEventsOutput = match self
            .llm
            .create_structured_output::<RawEventsOutput>(
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

        let events = raw
            .events
            .into_iter()
            .filter_map(convert_raw_event)
            .collect();

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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use cognee_llm::error::{LlmError, LlmResult};
    use cognee_llm::types::{GenerationOptions, GenerationResponse, Message};
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
            unimplemented!("not used in event_extractor tests")
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

    #[tokio::test]
    async fn extract_events_happy_path() {
        // Mock returns two events: one point-in-time, one interval.
        let json = serde_json::json!({
            "events": [
                {
                    "name": "Moon Landing",
                    "description": "First humans on the Moon",
                    "time_from": { "year": 1969, "month": 7, "day": 20, "hour": 20, "minute": 17, "second": 0 },
                    "time_to": null,
                    "location": "Sea of Tranquility"
                },
                {
                    "name": "World War II",
                    "description": "Global conflict",
                    "time_from": { "year": 1939, "month": 9, "day": 1 },
                    "time_to": { "year": 1945, "month": 9, "day": 2 },
                    "location": null
                }
            ]
        });

        let llm = Arc::new(MockLlm::with_json(json));
        let extractor = TemporalEventExtractor::new(llm);

        let events = extractor.extract_events("some text").await.unwrap();
        assert_eq!(events.len(), 2);

        // First event: point-in-time (only time_from, no time_to).
        let e0 = &events[0];
        assert_eq!(e0.name, "Moon Landing");
        assert_eq!(e0.description.as_deref(), Some("First humans on the Moon"));
        assert_eq!(e0.location.as_deref(), Some("Sea of Tranquility"));
        assert!(e0.at.is_some(), "point-in-time event should have `at`");
        assert!(e0.during.is_none());
        let ts = e0.at.as_ref().unwrap();
        assert_eq!(ts.year, 1969);
        assert_eq!(ts.month, 7);
        assert_eq!(ts.day, 20);

        // Second event: interval (both time_from and time_to).
        let e1 = &events[1];
        assert_eq!(e1.name, "World War II");
        assert!(e1.at.is_none());
        assert!(e1.during.is_some(), "interval event should have `during`");
        let interval = e1.during.as_ref().unwrap();
        assert_eq!(interval.time_from.year, 1939);
        assert_eq!(interval.time_to.year, 1945);
    }

    #[tokio::test]
    async fn extract_events_returns_empty_on_llm_error() {
        let llm = Arc::new(MockLlm::with_error("service unavailable"));
        let extractor = TemporalEventExtractor::new(llm);

        let events = extractor.extract_events("some text").await.unwrap();
        assert!(events.is_empty(), "LLM error should yield empty vec");
    }

    #[tokio::test]
    async fn extract_events_filters_empty_names() {
        let json = serde_json::json!({
            "events": [
                {
                    "name": "",
                    "description": null,
                    "time_from": null,
                    "time_to": null,
                    "location": null
                },
                {
                    "name": "Valid Event",
                    "description": "Has a name",
                    "time_from": { "year": 2020, "month": 1, "day": 1 },
                    "time_to": null,
                    "location": null
                }
            ]
        });

        let llm = Arc::new(MockLlm::with_json(json));
        let extractor = TemporalEventExtractor::new(llm);

        let events = extractor.extract_events("some text").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "Valid Event");
    }

    #[test]
    fn convert_raw_event_point_in_time() {
        let raw = RawEvent {
            name: "Launch".to_string(),
            description: Some("Rocket launch".to_string()),
            time_from: Some(RawExtractedTimestamp {
                year: 2024,
                month: 3,
                day: 15,
                hour: 10,
                minute: 30,
                second: 0,
            }),
            time_to: None,
            location: Some("Cape Canaveral".to_string()),
        };

        let event = convert_raw_event(raw).unwrap();
        assert_eq!(event.name, "Launch");
        assert!(event.at.is_some());
        assert!(event.during.is_none());
        let ts = event.at.unwrap();
        assert_eq!(ts.year, 2024);
        assert_eq!(ts.month, 3);
        assert_eq!(ts.day, 15);
        assert_eq!(ts.hour, 10);
        assert_eq!(ts.minute, 30);
        assert_eq!(ts.timestamp_str, "2024-03-15 10:30:00");
    }

    #[test]
    fn convert_raw_event_interval() {
        let raw = RawEvent {
            name: "Conference".to_string(),
            description: None,
            time_from: Some(RawExtractedTimestamp {
                year: 2025,
                month: 6,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            }),
            time_to: Some(RawExtractedTimestamp {
                year: 2025,
                month: 6,
                day: 5,
                hour: 0,
                minute: 0,
                second: 0,
            }),
            location: None,
        };

        let event = convert_raw_event(raw).unwrap();
        assert_eq!(event.name, "Conference");
        assert!(event.at.is_none());
        assert!(event.during.is_some());
        let interval = event.during.unwrap();
        assert_eq!(interval.time_from.year, 2025);
        assert_eq!(interval.time_from.day, 1);
        assert_eq!(interval.time_to.day, 5);
    }

    #[test]
    fn convert_raw_event_invalid_timestamp() {
        // Month 13 is invalid — to_cognify_timestamp returns None.
        // For a point-in-time case (only time_from), the event is still
        // returned but with at: None and during: None.
        let raw = RawEvent {
            name: "Bad Date".to_string(),
            description: None,
            time_from: Some(RawExtractedTimestamp {
                year: 2024,
                month: 13,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            }),
            time_to: None,
            location: None,
        };

        let event = convert_raw_event(raw).expect("event with invalid timestamp is still returned");
        assert!(
            event.at.is_none(),
            "Invalid month should cause `at` to be None"
        );
        assert!(event.during.is_none());

        // For an interval case, if time_from is invalid the entire interval
        // is dropped — convert_raw_event returns None because `?` propagates
        // the None from to_cognify_timestamp inside the (Some, Some) branch.
        let raw_interval = RawEvent {
            name: "Bad Interval".to_string(),
            description: None,
            time_from: Some(RawExtractedTimestamp {
                year: 2024,
                month: 13,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            }),
            time_to: Some(RawExtractedTimestamp {
                year: 2024,
                month: 6,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            }),
            location: None,
        };

        let result = convert_raw_event(raw_interval);
        assert!(
            result.is_none(),
            "Invalid month in interval should cause convert_raw_event to return None"
        );
    }
}
