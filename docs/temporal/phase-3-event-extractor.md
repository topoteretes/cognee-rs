# Phase 3 — Event Extractor

**File:** `crates/cognify/src/temporal_extraction/event_extractor.rs`  
**Status:** Done

---

## Goal

Implement `TemporalEventExtractor`: the first LLM pass that reads a `DocumentChunk` and returns a list of `TemporalEvent` objects (without entity attributes — those come from Phase 4).

---

## Python Reference

```python
# cognee/tasks/temporal_graph/extract_events_and_entities.py
async def extract_events_and_timestamps(data_chunks: List[DocumentChunk]) -> List[DocumentChunk]:
    for chunk in data_chunks:
        result = await extract_event_graph(chunk.text, EventList)
        for event in result.events:
            event_dp = await generate_event_datapoint(event)
            chunk.contains.append(event_dp)
    return data_chunks
```

`extract_event_graph` calls the LLM with `generate_event_graph_prompt.txt` as the system prompt and the chunk text as the user prompt, requesting structured JSON output.

---

## Internal Schema Types

Define these privately in the module (not exported). They map to the prompt output schema before conversion to the canonical `TemporalEvent` type from Phase 1.

```rust
/// Raw timestamp as returned by the LLM. Fields beyond year are optional in JSON.
#[derive(Debug, Deserialize, JsonSchema)]
struct RawTimestamp {
    pub year: u16,
    #[serde(default = "default_one")]
    pub month: u8,
    #[serde(default = "default_one")]
    pub day: u8,
    #[serde(default)]
    pub hour: u8,
    #[serde(default)]
    pub minute: u8,
    #[serde(default)]
    pub second: u8,
}

/// Raw event as returned by the LLM.
#[derive(Debug, Deserialize, JsonSchema)]
struct RawEvent {
    pub name: String,
    pub description: Option<String>,
    pub time_from: Option<RawTimestamp>,
    pub time_to: Option<RawTimestamp>,
    pub location: Option<String>,
}

fn default_one() -> u8 { 1 }
```

---

## Struct

```rust
pub struct TemporalEventExtractor {
    llm: Arc<dyn Llm>,
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
    ) -> Result<Vec<TemporalEvent>, CognifyError>;
}
```

---

## `extract_events` Implementation

```rust
pub async fn extract_events(&self, chunk_text: &str) -> Result<Vec<TemporalEvent>, CognifyError> {
    let options = GenerationOptions {
        temperature: Some(0.1),
        max_tokens: Some(4000),
        ..Default::default()
    };

    let raw: Vec<RawEvent> = match self
        .llm
        .structured_output(TEMPORAL_EVENT_EXTRACTION_PROMPT, chunk_text, &options)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Temporal event extraction failed: {e}");
            return Ok(vec![]);
        }
    };

    let events = raw
        .into_iter()
        .filter_map(|r| convert_raw_event(r))
        .collect();

    Ok(events)
}
```

---

## `convert_raw_event`

```rust
fn convert_raw_event(raw: RawEvent) -> Option<TemporalEvent> {
    if raw.name.trim().is_empty() {
        return None;
    }

    let at = raw.time_from.as_ref()
        .or(raw.time_to.as_ref())
        .and_then(|ts| to_cognify_timestamp_from_raw(ts.clone()));

    // If both bounds are present, build an Interval instead.
    let (at, during) = match (raw.time_from, raw.time_to) {
        (Some(from), Some(to)) => {
            let ts_from = to_cognify_timestamp_from_raw(from)?;
            let ts_to   = to_cognify_timestamp_from_raw(to)?;
            (None, Some(CognifyInterval { time_from: ts_from, time_to: ts_to }))
        }
        (Some(from), None) => (to_cognify_timestamp_from_raw(from), None),
        (None, Some(to))   => (to_cognify_timestamp_from_raw(to), None),
        (None, None)       => (None, None),
    };

    Some(TemporalEvent {
        name: raw.name,
        description: raw.description,
        location: raw.location,
        at,
        during,
        attributes: vec![],  // populated by Phase 4
    })
}
```

---

## Key Implementation Notes

- `max_tokens: 4000` — event lists from verbose documents can be long.
- On parse error: log `warn!` and return `Ok(vec![])` — temporal extraction is best-effort; the pipeline continues with non-temporal data.
- Use `to_cognify_timestamp` from Phase 1 to compute `time_at` in **milliseconds** and `timestamp_str`.
- Both `time_from` and `time_to` present → build `CognifyInterval { time_from, time_to }` (matching Python field names).
- Only one bound present → single `at` timestamp (Python convention: use `time_from` or `time_to`, not both).

---

## Verification

```bash
cargo check --all-targets -p cognee-cognify
```
