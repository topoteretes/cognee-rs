# Phase 4 — Entity Enrichment

**File:** `crates/cognify/src/temporal_extraction/entity_enricher.rs`  
**Status:** Done

---

## Goal

Implement `TemporalEntityEnricher`: the second LLM pass that takes the events produced by Phase 3 and enriches them with typed entity attributes (people, places, organisations, etc.) and their relationships to each event.

---

## Python Reference

```python
# cognee/tasks/temporal_graph/extract_knowledge_graph_from_events.py
async def extract_knowledge_graph_from_events(data_chunks):
    all_events = [e for chunk in data_chunks for e in chunk.contains if isinstance(e, Event)]
    enriched = await enrich_events(all_events)          # LLM call
    for event, enriched_event in zip(all_events, enriched):
        add_entities_to_event(event, enriched_event)    # attaches .attributes
    return data_chunks
```

`enrich_events` serialises the event list to JSON and posts it to the LLM with `generate_event_entity_prompt.txt` as the system prompt.

---

## Internal Schema Types

```rust
/// LLM output for a single enriched event.
#[derive(Debug, Deserialize, JsonSchema)]
struct RawEnrichedEvent {
    pub event_name: String,
    #[serde(default)]
    pub attributes: Vec<RawAttribute>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RawAttribute {
    pub entity: String,
    pub entity_type: String,
    pub relationship: String,
}
```

---

## Struct

```rust
pub struct TemporalEntityEnricher {
    llm: Arc<dyn Llm>,
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
        events: Vec<TemporalEvent>,
    ) -> Result<Vec<TemporalEvent>, CognifyError>;
}
```

---

## `enrich` Implementation

```rust
pub async fn enrich(&self, mut events: Vec<TemporalEvent>) -> Result<Vec<TemporalEvent>, CognifyError> {
    // Build the user prompt: serialise event name + description as the input list.
    let input: Vec<serde_json::Value> = events
        .iter()
        .map(|e| serde_json::json!({
            "event_name": e.name,
            "description": e.description,
        }))
        .collect();

    let user_prompt = serde_json::to_string(&input)
        .map_err(|e| CognifyError::Serialization(e.to_string()))?;

    let options = GenerationOptions {
        temperature: Some(0.1),
        max_tokens: Some(8000),
        ..Default::default()
    };

    let enriched: Vec<RawEnrichedEvent> = match self
        .llm
        .structured_output(TEMPORAL_ENTITY_ENRICHMENT_PROMPT, &user_prompt, &options)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Entity enrichment failed: {e}. Events returned without attributes.");
            return Ok(events);
        }
    };

    // Match enriched entries back to events by name (same approach as Python).
    let enriched_map: HashMap<String, Vec<EventAttribute>> = enriched
        .into_iter()
        .map(|r| {
            let attrs = r.attributes
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
```

---

## Key Implementation Notes

- `max_tokens: 8000` — enriched event lists can be very large; dozens of attributes per event.
- Match enriched output back to inputs by `event_name`. Python does the same — events are identified by name within a batch. If an event name is not found in the LLM output, it keeps empty `attributes`.
- On LLM or parse failure: log `warn!` and return the original (un-enriched) events rather than an error. Entity enrichment is best-effort; the temporal graph still works with bare events.
- Pass events **all at once** per batch (not one at a time). The prompt is designed for list input so the LLM can reason across events for consistency.

---

## Verification

```bash
cargo check --all-targets -p cognee-cognify
```
