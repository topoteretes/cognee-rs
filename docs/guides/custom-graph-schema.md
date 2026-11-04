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
DataPoint infrastructure fields: the keys a stored node's base carries (`id`,
`type`, `version`, `metadata`, the timestamps, the `source_*` provenance
fields, ...), which a custom property would overwrite. Python is laxer here:
its generated subclass may redeclare any `DataPoint` field. The two Python
`DataPoint` fields the Rust base does not carry, `ontology_uri` and the
removed `valid_to`, are ordinary custom fields, as in Python.

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
float (`3` and `"3"` become `3.0`), an `integer` field accepts `3.0` and `"3"` as
`3`, a `date-time` is a timezone-aware datetime (`2024-03-01 00:00:00+00:00`; a
Unix timestamp is read as UTC), a `date`, `time` or `uuid` prints as Python's
`2024-01-02`, `03:04:05` and hyphenated lower-case text, an enum
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

Edge sentences (`"{source} {field} {target}."`) label a custom
node with the same Python `str()` of its first non-blank index field, trimmed to
80 characters with whitespace collapsed (`Role.lead_dev`, `True`,
`['x', 'y']`), else its `name` stripped but otherwise untouched, else the model
name. For a union of models (`anyOf` of `$ref`s) the first declared model the
value validates against wins, keys it does not declare being ignored: the
generated `DataPoint` subclasses keep Pydantic from ranking candidates.

Answers are validated as Pydantic validates the generated model, in lax mode
(`model_validate_json`, the call instructor makes for a structured-output
answer), and a field holds the coerced value, not the raw JSON. The table below
was recorded from real Pydantic 2.12 (`crates/cognify/tests/fixtures/pydantic_lax/`
holds the generator and the full ground-truth table that the unit tests replay,
including the node ids Python's `uuid5` computes).

| Field | Accepted besides the exact JSON type | Held as (`str()` feeds identity and labels) |
|---|---|---|
| `number` (`float`) | an integer, a bool (`true` is `1.0`), numeric text (`"3"`, `" 3.5 "`, `"1e3"`, `"1_0"`, `".5"`) | `3` and `"3"` are `3.0` |
| `integer` (`int`) | an integral float below 2^63 (`3.0`), a bool, text of digits (`"3"`, `" 3 "`, `"+3"`, `"3.0"`, `"1_0"`) | `"3.0"` is `3` |
| `boolean` | `0`/`1`/`0.0`/`1.0`, and the texts `0 off f false n no` / `1 on t true y yes` in any case | `"yes"` is `True` |
| string enum | the exact text only | `Role.lead_dev` |
| `integer` enum (`IntEnum`) | the member value read as an `integer` (`true`, `1.0`, `"1"`) | prints the value, `1` |
| any other enum | Python equality: `true` is `1`, `1` is `true`, `1.0` is `1`; numeric text is not read | `Flag.bool_True` |
| `date-time` | ISO text with an offset, or a Unix timestamp as a number or as text (`1700000000`, `"1700000000.5"`, `"1.7e9"`); past 2e10 a timestamp is milliseconds | `2023-11-14 22:13:20+00:00`, stored `2023-11-14T22:13:20+00:00` |
| `date` | `YYYY-MM-DD`, a datetime text or timestamp that is exactly midnight (`"2024-01-02T00:00:00+02:00"`, `86400`, `"86400"`) | `1970-01-02` |
| `time` | `HH:MM[:SS[.f]]` with an optional offset, or a number of seconds in `[0, 86400)` (read as UTC) | `01:01:01+00:00` |
| `uuid` | hyphenated, 32 hex digits, `{...}` and `urn:uuid:` forms, any case | `abcdef78-1234-...` lower-case |

Pydantic still rejects `"3.5"` and `3.5` for an `integer`, `2` for a `boolean`,
`"1"` for a string enum or an untyped integer enum, a bare number for a `string`
field, a naive or date-only `date-time`, a date or `"1700000000"` that is not
midnight for a `date`, and a bare-digits string for a `time`; so does Rust. The
same coercions apply inside a list (`[1, "2"]` for `number` items holds
`[1.0, 2.0]`) and per variant of a union. A union follows Pydantic's smart
mode: every variant is tried, the most exact match wins and the first declared
variant breaks a tie (`"3"` is `3` in `integer | number` but `3.0` in
`number | integer`). Pydantic grades a JSON input as *exact* (a JSON integer
for `integer`, a float for `number`, a bool for `boolean`, any string for
`uuid`), *strict* (an integer for `number`, any string for `string`, ISO or
timestamp text for `date-time` and `time`, a date or integer text for `date`,
a scalar `$defs` entry, which becomes a `RootModel`, at best) or *lax* (every
other coercion, and every enum match). So `1700000000` is the float
`1700000000.0` in `date-time | number`, while `"1700000000"` is a datetime in
`number | date-time`; and `"1"` is `1.0` in `number | Digits` for a string
enum `Digits`, because an enum match is never better than lax.

Stored properties are the values Python holds in the form its storage encoder
writes: a `number` as a float, an `integer` as an int, a `boolean` as a bool, an
enum as its member value, a `date-time`, `date` or `time` as `isoformat()` text
(`2023-11-14T22:13:20+00:00`) and a `uuid` as lower-case hyphenated text. A
`date-time` string that Rust used to keep verbatim is now normalised the same
way (`2024-03-01T00:00:00Z` is stored as `2024-03-01T00:00:00+00:00`).

Details that follow speedate and pydantic-core: a `date-time` accepts `T`, `t`,
`_` or space as the separator, `.` or `,` fractions (truncated to microseconds),
no surrounding whitespace and no second 60; a JSON float timestamp is floored
and its fraction added as an absolute value (`-1.25` is `23:59:58.25`, while the
text `"-1.25"` is `23:59:58.75`); a timestamp text must be `[+-]digits` or
contain a `.` (`"1e9"` is not one, `"1.7e9"` is). A non-object `$defs` entry
that wraps a model (`People = list[Person]`) is rejected at compile time.

Known gaps, where Pydantic accepts a value that is refused here:

- a value `serde_json` cannot hold: numeric text that parses to `inf` or `nan`
  (`"inf"`, `"NaN"`, `"1e400"`), and an integer beyond the `i64`/`u64` range
  (`"99999999999999999999"`, `18446744073709551616`);
- `null` for a field that is not declared nullable: the generated Python model
  makes every property missing from `required` optional, so it takes `null`,
  while Rust reads the schema as written;
- enum member names use Rust's Unicode tables, so a few non-ASCII values
  (combining marks, numeric ideographs) can be named differently from Python;
- the `uuid1`..`uuid5` formats and other string formats are plain strings, as
  before.

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
Finally, `validate()` compiles the schema, so an invalid one fails before any
document is classified or chunked, as Python builds the model class before
running.

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
Chunk attachment requires a custom graph schema.

### FromIdentity references (Rust-only keyword)

A reference field marked `"x-cognee-from-identity": true` is answered by the
LLM with the target's identity string (a list of them for an array field)
instead of a nested object. The string is then widened back into the target
node: `"Acme"` becomes `{"name": "Acme"}` for a target whose `identity_fields`
is `["name"]`, so it gets the same id as the full `Acme` node, extracted
alongside it or already in the graph. This is Python's
`Annotated[Company, FromIdentity()]` marker:

```json
"employer": {
    "anyOf": [{ "$ref": "#/$defs/Company" }, { "type": "null" }],
    "default": null,
    "x-cognee-from-identity": true
}
```

**The keyword is Rust-only.** Python's marker exists only on hand-written
Pydantic classes; its schema converter and spec DSL have no JSON form for it.
A schema that uses the keyword still runs in Python, but Python ignores the
keyword and asks for nested objects.

The rules are Python's `_llm_identity_field_for`. They are checked when the
schema is compiled, so a violation fails before any LLM call (as
`DynamicGraphError::InvalidReferenceType`, with Python's
`InvalidReferenceTypeError` wording):

- **Shape:** the field holds one model as a `$ref`, a `$ref` or `null`, or an
  array of `$ref` (optionally or `null`). A union of models, a scalar, a
  non-model `$defs` entry and an array of nullable items are rejected.
- **Identity:** the target declares exactly one `identity_fields` entry.
- **Constructible:** every other required field of the target has a default,
  so the target can be built from the identity string alone.
- **Marker value:** the keyword takes `true` or `false`. It may sit on the
  field or inside it, such as on the `anyOf` variant, as Python finds a marker
  at any depth. The keyword is removed from the schema sent to the LLM in
  every case, `false` included, so a strict structured-output request never
  sees it.

Python does not check that the target's identity field is one of its declared
fields, and neither does Rust. When it is not, every reference gets a random
id and so becomes a node of its own; the compile step logs a warning that the
identity has no stable value.

In the schema sent to the LLM the field becomes `string`, `string` or `null`,
or an array of `string`. Its `default` and `title` are kept, and its
description is replaced with Python's `"The {identity field} of a {Target}
already in the graph"`. An answered object is still accepted as the nested
model, as Python leaves a non-string answer untouched.

A reference becomes a node holding only the identity value and the target's
defaults, which has two consequences, both as in Python:

- **Within one storage batch the first occurrence of an id wins** (see above),
  and the walk follows the models' field *declaration* order, not the
  answer's key order. Declare the field that lists targets in full before the
  fields that reference them, or the reference-only node is the one stored.
- **A reference stored in a later batch replaces the stored node's
  properties** with those defaults: both Python's and Rust's Ladybug adapters
  overwrite the whole property blob on `MERGE ... ON MATCH SET`. A reference
  resolves to the right node but resets its other fields.

### Spec DSL

A custom graph model can also be written in the hand-writable spec DSL that
Python's `cognee.modules.graph_models` defines (and the cognee frontend's
graph-model editor produces): entities with `primitive`, `enum` and
`relation` fields. Rust's port,
[`crates/cognify/src/graph_spec.rs`](../../crates/cognify/src/graph_spec.rs),
compiles a spec into the JSON Schema above, key for key what Python's
`graph_spec_to_json_schema` emits, so a spec runs through the same
`graph_schema` path:

```rust
use cognee_cognify::{CognifyConfig, GraphSchemaSpec, graph_spec_to_json_schema};
use serde_json::json;

let spec = GraphSchemaSpec::from_value(&json!({
    "entities": [
        {
            "name": "Person",
            "fields": [
                {"kind": "primitive", "name": "age", "primitive_type": "number"},
                {"kind": "enum", "name": "mood", "enum_values": ["happy", "grumpy"]},
                {"kind": "relation", "name": "works_at",
                 "relation": {"target_entity_name": "Organization"}},
                {"kind": "relation", "name": "knows",
                 "relation": {"target_entity_name": "Person", "cardinality": "many"}}
            ]
        },
        {"name": "Organization"}
    ]
}))?;
let config = CognifyConfig::default().with_graph_schema(graph_spec_to_json_schema(&spec)?);
```

The compiled schema puts every entity and a `{Name}Type` marker in `$defs` and
spreads the root entity (`root`, else the first) at the top level. Each entity
gets a required string `name` and an `is_type` marker: every instance of one
entity type points at a single `{Name}Type` node. `metadata` defaults to
`index_fields: ["name"]` (also when given as `[]`) and `identity_fields:
["name"]` (`[]` opts out of merging). `date` fields are ISO date strings, and a
relation field is never required. `inverse` and `options.auto_type_nodes` are
accepted and, as in Python, not compiled.

Validation follows Python's `GraphSchemaSpec`, in the same order, and a rule
violation carries Python's message verbatim:

- **Limits and names:** 1–50 entities and at most 40 fields per entity. Entity
  and field names are ASCII identifiers (`^[A-Za-z][A-Za-z0-9_]*$`). Rust also
  rejects a trailing newline, which Python's `$` lets through.
- **Uniqueness:** entity names are unique, and field names are unique within
  an entity.
- **Reserved names:** an entity may not be named `{Other}Type` for another
  entity `Other`. Fields may not reuse a `DataPoint` field name or `is_type`.
- **`name` and relations:** an explicit `name` field must be a string
  primitive. A relation must target a declared entity.
- **References:** `index_fields`, `identity_fields` and `primary_label_field`
  name `name` or a declared primitive or enum field.

Keys are snake_case with camelCase aliases (`target_entity_name` or
`targetEntityName`). An unknown key is rejected, and so is a field without its
`kind`. Boolean flags accept Pydantic's lax spellings (`"yes"`, `1`, `"off"`).

As in Python, a spec can compile but still not build: the root entity's name
must be a class name (see above), so `person` or `Org_unit` as a root fails when
the schema is compiled. `graph_model_from_spec` runs both steps.
`crates/cognify/tests/fixtures/graph_spec/` holds the generator and the table,
recorded from Python, that the unit tests replay.

### Entry points

`graph_schema` and `chunk_attachment` are per-call options, like Python's
`graph_model` and `chunk_attachment` arguments; there is no persistent setting
or environment variable for either.

- **Rust**: `CognifyConfig::with_graph_schema` / `with_chunk_attachment`, as in
  the example above. `cognee_cognify::parse_graph_schema` reads a schema given
  as an object or a JSON string, and `ChunkAttachment::parse` (or `FromStr`)
  reads `"direct"` / `"all"`.
  A spec goes through `graph_spec_to_json_schema` (or
  `graph_schema_from_spec_value` for an object or a JSON string), as above.
- **CLI**: `cognee-cli cognify --graph-schema schema.json --chunk-attachment all`,
  or `--graph-spec spec.json` for a spec (not both); `add-and-cognify` takes
  the same flags.
- **Bindings** (Python handle, TypeScript, Java, C): the cognify and
  add-and-cognify options `graphSchema` (an object or a JSON string; alias
  `graphModel`), `graphSpec` (a spec, compiled natively; not with
  `graphSchema`) and `chunkAttachment`. Python also accepts `graph_schema` /
  `graph_spec` / `chunk_attachment` and has
  `cognee_py.graph_spec_to_json_schema`; TypeScript types the spec as
  `GraphSchemaSpec`; Java has `CognifyOptions.graphSchema(...)`,
  `.graphSpec(...)` and `.chunkAttachment(...)`.
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
  `graph_spec_to_json_schema` is re-exported, as upstream's `cognee.low_level`
  does, so `graph_model=graph_spec_to_json_schema(spec)` works; `graph_spec=`
  passes a spec straight through.

`null` and `{}` mean the default `KnowledgeGraph`, as Python's HTTP route reads
an omitted or empty `graph_model`. Every entry point validates before any side
effect, so `add-and-cognify` adds nothing when the options are invalid: an
attachment other than `"direct"` / `"all"` (case-sensitive, as Python compares
it), an attachment without a schema, a schema that does not compile, or a schema
combined with a graph backend or the `cascade` strategy is a validation error
(exit code 2 in the CLI, `VALIDATION_ERROR` in the bindings). In the bindings a
wrongly-typed value for any known cognify option is an error too, and an unknown
option is logged as a warning, as Python's `cognify()` forwards unknown keywords
instead of raising.

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
- [`crates/cognify/src/graph_spec.rs`](../../crates/cognify/src/graph_spec.rs) — the spec DSL and its compiler.
- [`crates/cognify/src/summarization/extractor.rs`](../../crates/cognify/src/summarization/extractor.rs) — summary schema consumption.
