# Custom Graph & Summarization Schemas

cognee-rust mirrors two Python cognify knobs that swap the LLM's structured
output shape: `graph_model` (graph extraction) and `summarization_model`
(summaries). **Their wiring status differs — read carefully.**

## Summarization schema — wired

### What it does

Replaces the default `SummarizedContent` shape requested from the LLM during the
summarization stage with your own JSON Schema. Mirrors Python's
`CognifyConfig.summarization_model`.

### Requirement

The schema **must** contain a string `summary` property — the pipeline reads
`summary` to build each `TextSummary`. This is validated up front by
[`validate_summary_schema`](../../crates/cognify/src/config.rs), so a bad schema
fails at config time rather than mid-pipeline.

### Example (programmatic)

```rust
use cognee_cognify::{ChunkAttachment, CognifyConfig};
use serde_json::json;

let schema = json!({
    "type": "object",
    "properties": {
        "summary":  { "type": "string" },
        "keywords": { "type": "array", "items": { "type": "string" } }
    },
    "required": ["summary"]
});

let config = CognifyConfig::default()
    .with_summary_schema(schema)?;   // returns Err if `summary` is missing/non-string
```

This path **is consumed**: the summarization task constructs
`SummaryExtractor::new_with_schema(llm, config.summary_schema)`
([`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs)), and the
extractor requests the custom schema when it is `Some`
([`crates/cognify/src/summarization/extractor.rs`](../../crates/cognify/src/summarization/extractor.rs)).

### Via top-level config

`cognee` exposes a runtime setter mirroring Python's
`cognee.config.set_summarization_model(...)`:

```rust
settings.set_summarization_schema(schema)?;  // crates/lib/src/config.rs
```

## Graph extraction schema — wired

### What it does

Replaces the default `KnowledgeGraph` extraction shape with the JSON Schema
produced by a custom Pydantic graph model. Rust compiles that schema at runtime,
requests structured output from the LLM, walks the returned model, and persists
its nodes and relationships through the normal graph, provenance, and vector
indexing stages.

### Schema contract

Pass the JSON Schema emitted by Pydantic's `model_json_schema()`. Local `$defs`
and `#/$defs/...` references are supported. External references are rejected so
the schema is self-contained and deterministic. The DataPoint `metadata`
property is consumed by the storage walk and removed from the schema sent to
the LLM; descriptive JSON Schema annotations remain available to guide it.

Every stored model must ultimately expose at least one DataPoint-shaped object.
Python encodes runtime policy in the `metadata` property's default object. Its
`identity_fields` determine the stable UUID, `index_fields` select vectorized
scalar fields, and `transparent: true` makes a structural container disappear
during persistence so its DataPoint descendants take its place.

### Example (programmatic)

```rust
use cognee_cognify::CognifyConfig;
use serde_json::json;

let schema = json!({
    "title": "ResearchGraph",
    "type": "object",
    "properties": {
        "paper": { "$ref": "#/$defs/Paper" },
        "metadata": {
            "type": "object",
            "default": { "transparent": true }
        }
    },
    "required": ["paper"],
    "$defs": {
        "Paper": {
            "title": "Paper",
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "metadata": {
                    "type": "object",
                    "default": {
                        "identity_fields": ["title"],
                        "index_fields": ["title"]
                    }
                }
            },
            "required": ["title"]
        }
    }
});

let config = CognifyConfig::default()
    .with_graph_schema(schema)
    .with_chunk_attachment(ChunkAttachment::Direct);
```

By default each chunk links directly to the custom graph root. Use
`with_chunk_attachment(ChunkAttachment::All)` to link it to every persisted
DataPoint reached by the transparent storage walk.

## Pointers

- [`CognifyConfig`](../../crates/cognify/src/config.rs) — schema and chunk-attachment configuration.
- [`crates/cognify/src/dynamic_graph.rs`](../../crates/cognify/src/dynamic_graph.rs) — runtime schema compiler, validation, identity, and storage walk.
- [`crates/cognify/src/summarization/extractor.rs`](../../crates/cognify/src/summarization/extractor.rs) — summary schema consumption.
