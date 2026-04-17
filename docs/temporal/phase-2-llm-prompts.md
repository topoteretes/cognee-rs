# Phase 2 — LLM Prompts

**Crate:** `cognee-cognify` (`crates/cognify/src/temporal_extraction/`)  
**Status:** Done

---

## Goal

Create the two prompt files (and the module skeleton) that drive the temporal LLM passes. The prompts are a direct port of the Python originals; the module structure follows the pattern already established in `crates/cognify/src/fact_extraction/`.

---

## Python Reference

| Prompt file | Purpose |
|---|---|
| `cognee/infrastructure/llm/prompts/generate_event_graph_prompt.txt` | System prompt for event + timestamp extraction |
| `cognee/infrastructure/llm/prompts/generate_event_entity_prompt.txt` | System prompt for entity enrichment |

---

## Directory Layout

```
crates/cognify/src/temporal_extraction/
├── mod.rs
├── event_extractor.rs    (Phase 3)
├── entity_enricher.rs    (Phase 4)
└── prompts/
    ├── temporal_event_extraction.txt
    └── temporal_entity_enrichment.txt
```

Add `pub mod temporal_extraction;` to `crates/cognify/src/lib.rs`.

---

## `temporal_event_extraction.txt`

Port `generate_event_graph_prompt.txt` verbatim. The key rules that must survive the port:

1. **Event definition** — anything with a date/timestamp is an event; anything that took place in time is an event; ANY verb is an event.
2. **Timestamped first** — every timestamp in the text must produce at least one event.
3. **Quantity over filtering** — extract 100+ events from a typical document; never skip.
4. **Point vs range events** — for instantaneous events set only `time_from` OR `time_to` (not both); for ranges set both.
5. **Descriptions** — always include "who did what / what happened", quoting the source text.
6. **Output schema** — JSON array matching the `RawExtractedEvent` struct used in Phase 3:

```json
[
  {
    "name": "string (concise)",
    "description": "string | null",
    "time_from": { "year": 1962, "month": 10, "day": 1 } | null,
    "time_to":   { "year": 1962, "month": 10, "day": 1 } | null,
    "location": "string | null"
  }
]
```

Timestamp objects must contain at minimum `year`; `month`, `day`, `hour`, `minute`, `second` are optional (omit unknown fields rather than setting them to null).

---

## `temporal_entity_enrichment.txt`

Port `generate_event_entity_prompt.txt` verbatim. Key rules:

1. **Input** — JSON array of `{ event_name, description }` objects.
2. **Extract all non-temporal entities** — people, places, organisations, objects, concepts, named temporal periods (eras, epochs). Skip raw dates/times.
3. **Quantity over filtering** — dozens of entities per event expected.
4. **Output schema** — same array augmented with an `attributes` key per event:

```json
[
  {
    "event_name": "string",
    "description": "string | null",
    "attributes": [
      {
        "entity": "string",
        "entity_type": "string",
        "relationship": "one_or_two_words"
      }
    ]
  }
]
```

5. **Relationship names** — snake_case, 1–2 words. Examples: `subject`, `participant`, `agent`, `instrument`, `source_cause`, `previous_owner`.

---

## `mod.rs`

```rust
pub mod event_extractor;
pub mod entity_enricher;
```

---

## Embedding prompts as constants

Both extractor modules embed their prompt via `include_str!`:

```rust
// in event_extractor.rs
const TEMPORAL_EVENT_EXTRACTION_PROMPT: &str =
    include_str!("prompts/temporal_event_extraction.txt");

// in entity_enricher.rs
const TEMPORAL_ENTITY_ENRICHMENT_PROMPT: &str =
    include_str!("prompts/temporal_entity_enrichment.txt");
```

This follows the pattern in `crates/cognify/src/fact_extraction/extractor.rs`.

---

## Verification

```bash
cargo check --all-targets -p cognee-cognify
```

The `include_str!` paths are resolved at compile time; any typo in the path surfaces as a compile error.
