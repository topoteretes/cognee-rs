# Custom Graph & Summarization Schemas

cognee-rust mirrors two Python cognify knobs that swap the LLM's structured
output shape: `graph_model` (graph extraction) and `summarization_model`
(summaries). Both are consumed by the standalone cognify pipeline; the graph
schema is reachable from every entry point (see
[Entry points](#entry-points)).

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
nested list — are rejected rather than stored as opaque properties. A `$defs`
entry that is not a model — an `enum`, a `const` or a non-object scalar type —
is a value, as Python generates an `Enum` or `RootModel` for it: a field that
references one is a scalar property, an answer outside the enum is rejected, and
the enum stays in the schema sent to the LLM. External
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
value is not indexed, as in Python. An `index_fields` name that is not a scalar
field of the model is skipped with a warning, as Python's `index_data_points`
skips it. The UUID is `uuid5(NAMESPACE_OID, "{Model}:{values}")` over each
identity value as Python `str()`s it after Pydantic validation: a `number` is a
float (`3` becomes `3.0`), an `integer` field accepts `3.0` as `3`, a
`date-time` is a timezone-aware datetime (`2024-03-01 00:00:00+00:00`), an enum
member prints as `Enum.member` with datamodel-code-generator's member name
(`"_x"` is `field_x`, `"mro"` is `mro_`, `1e20` is `number_1e_20`), lists use
Python `repr` quoting, and only plain strings are lower-cased with spaces turned
into `_`. An omitted optional identity field contributes `None`. A field
filled from its schema `default` keeps the raw JSON value, because Pydantic does
not validate defaults: a `number` default `2` contributes `2`, not `2.0`, and a
`date-time` or enum default is a plain string. A DataPoint
base field the schema does not declare contributes the value a fresh Python
instance holds: `type` is the model name, `metadata` the declared default,
`version` `1`, `ontology_valid` `False`, the `source_*` fields `None`, and so
on. `id`, `created_at`, `updated_at` and any name that is neither a field nor a
base field make every node fall back to a random UUID, as in Python (a warning
is logged).

Edge sentences (`"{source} {field} {target}."`) and triplet text label a custom
node with the same Python `str()` of its first non-blank index field, trimmed to
80 characters with whitespace collapsed (`Role.lead_dev`, `True`,
`['x', 'y']`), else its `name` stripped but otherwise untouched, else the model
name. For a union of models (`anyOf` of `$ref`s) the first declared model the
value validates against wins, keys it does not declare being ignored: the
generated `DataPoint` subclasses keep Pydantic from ranking candidates.

Answers are validated as Pydantic validates the generated model: a `date-time`
must be an ISO 8601 datetime with an offset, as speedate accepts it (`T`, `t`,
`_` or space separator, `.` or `,` fraction, no surrounding whitespace, no
second 60); an `integer` accepts an integral float below 2^63 in magnitude. A
non-object `$defs` entry that wraps a model (`People = list[Person]`) is
rejected at compile time. Known gaps: Unix-timestamp values for a `date-time`
field and integer literals beyond the u64 range are refused, though Pydantic
accepts both, and enum member names use Rust's Unicode tables, so a few
non-ASCII values (combining marks, numeric ideographs) can be named differently
from Python.

Within one storage batch (one data item's `chunks_per_batch` chunks, as Python's
`add_data_points` call) a node id is claimed by its first occurrence: a later
occurrence with the same id, in the same answer or another chunk's, contributes
neither its properties nor its outgoing edges, though the edge leading to it is
kept. `transparent: true` makes a structural
container disappear during persistence so its DataPoint descendants take its
place; a truthy scalar it carries is dropped with one warning per type and
field. Python's `GraphSchemaSpec` compiler never emits `transparent`, so it only
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

Each chunk is extracted with the same system prompt the default path sends,
as Python's `extract_content_graph` does for any response model: the default
graph prompt with that chunk's temporal normalization hints, or a
`custom_extraction_prompt` verbatim, with no hints. Only the response schema
differs.

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

### Entry points

`graph_schema` and `chunk_attachment` are per-call options, like Python's
`graph_model` and `chunk_attachment` arguments; there is no persistent setting
or environment variable for either.

- **Rust**: `CognifyConfig::with_graph_schema` / `with_chunk_attachment`, as in
  the example above. `cognee_cognify::parse_graph_schema` reads a schema given
  as an object or a JSON string, and `ChunkAttachment::parse` (or `FromStr`)
  reads `"direct"` / `"all"`.
- **CLI**: `cognee-cli cognify --graph-schema schema.json --chunk-attachment all`;
  `add-and-cognify` takes the same two flags.
- **Bindings** (Python handle, TypeScript, Java, C): the cognify and
  add-and-cognify options `graphSchema` (an object or a JSON string; alias
  `graphModel`) and `chunkAttachment`. Python also accepts `graph_schema` /
  `chunk_attachment`; Java has `CognifyOptions.graphSchema(...)` and
  `.chunkAttachment(...)`.
- **Python compat** (`cognee_py.compat`): `cognify(graph_model=..., chunk_attachment=...)`
  and `add_and_cognify(...)`, as upstream. `graph_model` takes a Pydantic model
  class, converted as upstream's `graph_model_to_graph_schema` does (through
  that function when upstream `cognee` is importable, otherwise with
  `model_json_schema()` minus the default field titles), or the schema as a
  dict or JSON string. Upstream's own `KnowledgeGraph` means the default
  pipeline. As upstream, `chunk_attachment` with a class that is not a
  `DataPoint` subclass raises `CogneeValidationError` — checked when
  upstream's `DataPoint` is importable. Without upstream `cognee` there is no
  `DataPoint` to subclass, so a class is accepted as its schema; a schema
  dict or string is always accepted, as upstream's HTTP route takes it.

`null` and `{}` mean the default `KnowledgeGraph`, as Python's HTTP route reads
an omitted or empty `graph_model`. Every entry point validates before any side
effect, so `add-and-cognify` adds nothing when the options are invalid: an
attachment other than `"direct"` / `"all"` (case-sensitive, as Python compares
it), an attachment without a schema, a schema that does not compile, or a
schema combined with `temporal_cognify`, a graph backend or the `cascade`
strategy is a validation error (exit code 2 in the CLI, `VALIDATION_ERROR` in
the bindings). In the bindings a wrongly-typed value for any known cognify
option is an error too, and an unknown option is logged as a warning, as
Python's `cognify()` forwards unknown keywords instead of raising.

### Ownership and deletion

Custom nodes and field edges are recorded in the ownership ledger per data
item before the graph and vector stores see them, as the default path records
its entities. A node that two data items produce keeps both owners and is
removed only with the last of them. Data, dataset and `forget` deletes and the
sweep after a failed run remove the item's custom nodes, their
`{Model}_{field}` points and the `EdgeType` points of their field edges. The
store-wide wipe (`DeleteScope::All`) drops the custom collections too, also on
a vector backend that cannot list its collections.

## Pointers

- [`CognifyConfig`](../../crates/cognify/src/config.rs) — schema and chunk-attachment configuration.
- [`crates/cognify/src/dynamic_graph.rs`](../../crates/cognify/src/dynamic_graph.rs) — runtime schema compiler, validation, identity, and storage walk.
- [`crates/cognify/src/summarization/extractor.rs`](../../crates/cognify/src/summarization/extractor.rs) — summary schema consumption.
