# Custom Graph & Summarization Schemas

cognee-rust mirrors two Python cognify knobs that swap the LLM's structured
output shape: `graph_model` (graph extraction) and `summarization_model`
(summaries). Both are consumed by the standalone cognify pipeline.

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

Pass a Pydantic-compatible JSON Schema whose models declare their policy in a
`metadata` property. Python produces one in either of two ways:

- `graph_model_to_graph_schema()` applied to plain `pydantic.BaseModel`
  classes that declare an explicit `metadata: dict = {...}` field — the pattern
  Python's own example uses.
- `graph_spec_to_json_schema()` applied to a `GraphSchemaSpec`.

`graph_model_to_graph_schema()` applied to a `DataPoint` subclass drops every
field `DataPoint` declares, `metadata` included, so that schema carries no
policy: every node gets a random id, nothing is vector-indexed and nothing is
transparent. Python's own round trip produces the same result, so Rust compiles
it the same way rather than rejecting it. An unprocessed
`DataPoint.model_json_schema()` is rejected, because it declares the reserved
DataPoint infrastructure fields.

Local `$defs` and `#/$defs/...` references are supported, whether a field holds
one model, an optional model, a union of models, or a list of any of these;
`null` list items are skipped. A union value is stored as the first declared
model whose fields cover the object's keys and that validates. Other shapes that
reach a model — a union mixing models with scalars, a dictionary of models, a
nested list — are rejected rather than stored as opaque properties. External
references are rejected so the schema is self-contained and deterministic. The
`metadata` property is consumed by the storage walk and removed from the schema
sent to the LLM; descriptive JSON Schema annotations remain available to guide
it.

Policy lives in the `metadata` property's default object:

```json
"metadata": {
    "type": "object",
    "additionalProperties": true,
    "default": { "identity_fields": ["name"], "index_fields": ["name"] }
}
```

`identity_fields` determine the stable UUID, and `index_fields` select the
scalar fields written to `{Model}_{field}` vector collections; a `null` or blank
value is not indexed, as in Python. `transparent: true` makes a structural
container disappear during persistence so its DataPoint descendants take its
place. Python's `GraphSchemaSpec` compiler never emits `transparent`, so it only
comes from a hand-written or `BaseModel` schema. For useful output, a
transparent root must have at least one reachable non-transparent descendant;
otherwise the custom graph intentionally stores no nodes.

The root model needs a `title` that is a valid class name: it starts with an
uppercase letter and holds only letters and ASCII digits (`ResearchGraph`, not
`research_graph`, `Research Graph` or `Graph²`), and it may not be `None`,
`True`, `False` or `DataPoint`. Python looks the generated class up by that
title, and its code generator rewrites the other spellings, so Python fails on
them too. The Rust check is slightly stricter in one place: it rejects
non-ASCII digits, which Python keeps. A root given as a `$ref` still needs that
top-level `title`, and it must equal the `$defs` key the `$ref` points at
(`{"title": "Company", "$ref": "#/$defs/Company", ...}`): Python reads only the
top-level title, raising `KeyError` without one, and any other title makes its
generator wrap the model in a `RootModel` it cannot store.

Only the single-pass LLM extractor honours a custom schema: `validate()` rejects
`graph_schema` together with a `graph_backend` or the `cascade` extraction
strategy, as Python raises for a custom `graph_model` with a non-LLM extractor.
`validate()` also rejects `graph_schema` together with `temporal_cognify`. This
is a Rust decision: Python drops `temporal_cognify` and runs the custom model,
whereas the Rust temporal pipeline would ignore the schema. Finally,
`validate()` compiles the schema, so an invalid one fails before any document
is classified or chunked, as Python builds the model class before running.

### Example (programmatic)

```rust
use cognee_cognify::{ChunkAttachment, CognifyConfig};
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

By default each chunk links to the stored root, or to the children that replace
a transparent root. Use `with_chunk_attachment(ChunkAttachment::All)` to link
it to every persisted DataPoint reached by the transparent storage walk.
Chunk attachment requires a custom graph schema. It is also rejected for
temporal cognify, by the same Rust decision as `graph_schema` above: Python
ignores `temporal_cognify` on this path rather than rejecting it.

## Pointers

- [`CognifyConfig`](../../crates/cognify/src/config.rs) — schema and chunk-attachment configuration.
- [`crates/cognify/src/dynamic_graph.rs`](../../crates/cognify/src/dynamic_graph.rs) — runtime schema compiler, validation, identity, and storage walk.
- [`crates/cognify/src/summarization/extractor.rs`](../../crates/cognify/src/summarization/extractor.rs) — summary schema consumption.
