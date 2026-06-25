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
use cognee_cognify::CognifyConfig;
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

`cognee-lib` exposes a runtime setter mirroring Python's
`cognee.config.set_summarization_model(...)`:

```rust
settings.set_summarization_schema(schema)?;  // crates/lib/src/config.rs
```

## Graph extraction schema — set but NOT consumed (standalone pipeline)

### What it does (in Python)

Python's `graph_model` lets you replace the default `KnowledgeGraph` extraction
shape with a custom Pydantic model.

### Status in Rust

`CognifyConfig.graph_schema` exists and has a builder
([`CognifyConfig::with_graph_schema`](../../crates/cognify/src/config.rs)), **but
the standalone cognify graph-extraction task does not read it.** A `graph_schema`
you set on `CognifyConfig` is effectively a no-op for the in-process pipeline.

Its only live consumers today are:

- **Dataset-config persistence** — stored/retrieved via
  [`crates/database/src/ops/dataset_configurations.rs`](../../crates/database/src/ops/dataset_configurations.rs).
- **HTTP server** — accepted on dataset-config payloads in
  [`crates/http-server/src/routers/datasets.rs`](../../crates/http-server/src/routers/datasets.rs)
  (and validated by `graph_schema_to_graph_model` in
  [`crates/llm/src/dynamic_model.rs`](../../crates/llm/src/dynamic_model.rs)).

See [docs/roadmap/cognify-compatibility-plan.md](../roadmap/cognify-compatibility-plan.md)
— wiring `graph_schema` into the extraction task is tracked as follow-up work.

To customize extraction today, use a [custom prompt](custom-prompts.md) instead.

## Pointers

- [`CognifyConfig`](../../crates/cognify/src/config.rs) — `summary_schema`, `graph_schema`, builders, `validate_summary_schema`.
- [`crates/cognify/src/summarization/extractor.rs`](../../crates/cognify/src/summarization/extractor.rs) — summary schema consumption.
- [roadmap/cognify-compatibility-plan.md](../roadmap/cognify-compatibility-plan.md) — graph_schema gap.
