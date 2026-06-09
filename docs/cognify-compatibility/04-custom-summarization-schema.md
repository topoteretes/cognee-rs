# Item 4 — Custom summarization output schema

Parent: [../cognify-compatibility-implementation-plan.md](../cognify-compatibility-implementation-plan.md)
Effort: **small–medium** · Independent of the Postgres work
Status: ✅ Implemented

> **Decision D2 (resolved):** the ticket's "custom LLM model for summarization"
> does **not** match Python — Python has no per-stage LLM. The real Python feature
> is `summarization_model`: a configurable **output schema** (Pydantic class,
> default `SummarizedContent`). This item implements the schema-customization
> path, not a separate LLM.

---

## Problem & Python reference

Python summarization is parameterized by an **output model class**, not an LLM:

- `CognifyConfig.summarization_model: object = SummarizedContent`
  ([cognify/config.py:10](/tmp/cognee-python/cognee/modules/cognify/config.py))
- `summarize_text(data_chunks, summarization_model=None)` falls back to
  `get_cognify_config().summarization_model` and calls
  `extract_summary(chunk.text, summarization_model)`
  ([summarize_text.py:18,64-69](/tmp/cognee-python/cognee/tasks/summarization/summarize_text.py))
- Public setter: `cognee.config.set_summarization_model(CustomSummary)`
  ([config.py:183-192](/tmp/cognee-python/cognee/api/v1/config/config.py))
- The summary text is read as `chunk_summaries[i].summary` — so any custom model
  is still expected to expose a `summary` field.

The Rust `SummaryExtractor` hardcodes the output type to the `SummarizedContent`
struct via the typed `create_structured_output::<SummarizedContent>()`
([extractor.rs:73-95](../../crates/cognify/src/summarization/extractor.rs#L73-L95)) — there is no way to
substitute a different schema.

### Two facts that shape the design

1. **The `Llm` trait already has a dynamic-schema path.**
   `Llm::create_structured_output_raw(text_input, system_prompt, json_schema: &Value, options)`
   ([llm_trait.rs:29](../../crates/llm/src/llm_trait.rs#L29)) takes a JSON schema as a
   `serde_json::Value` and returns a raw `Value`. The typed
   `LlmExt::create_structured_output::<T>()` is just a wrapper that generates the
   schema from `T: JsonSchema` and deserializes
   ([llm_trait.rs:124-135](../../crates/llm/src/llm_trait.rs#L124-L135)). So a custom-schema path needs **no
   new LLM API**.

2. **Do not copy the `graph_schema` pattern blindly.**
   `CognifyConfig.graph_schema` ([config.rs:125](../../crates/cognify/src/config.rs#L125)) exists but is
   **not consumed** by the core graph-extraction task — it is dead config in the
   standalone pipeline path (only used by dataset-config persistence + HTTP
   server). `summary_schema` must be **actually wired** into `SummaryExtractor`.

---

## Steps

### Step 4.1 — Add `summary_schema` to `CognifyConfig`

```rust
/// Optional JSON schema for the summarization output, mirroring Python's
/// `summarization_model` (default `SummarizedContent`). When `Some`, the
/// summarization stage requests this schema from the LLM instead of the built-in
/// `SummarizedContent` shape. The schema MUST contain a string `summary` field —
/// the pipeline reads `summary` to build each `TextSummary` (Python parity).
#[serde(skip)]
pub summary_schema: Option<serde_json::Value>,
```

`serde_json::Value` is `Debug`+`Clone`+`Serialize`+`Deserialize`, so unlike
`summary_llm` (rejected per D2) it does **not** need a newtype wrapper and could
even be serialized — but mark it `#[serde(skip)]` to match `graph_schema`'s
treatment and keep config snapshots stable. Confirm the `Default` impl sets it to
`None` (preserving today's behavior: built-in `SummarizedContent`).

### Step 4.2 — Builder + public config setter

Builder method (next to `with_graph_schema`,
[config.rs:326-327](../../crates/cognify/src/config.rs#L326-L327)):

```rust
pub fn with_summary_schema(mut self, schema: serde_json::Value) -> Self {
    self.summary_schema = Some(schema);
    self
}
```

Public API parity — add a `set_summarization_model` setter mirroring Python's
`cognee.config.set_summarization_model`. Place it alongside the other runtime
setters in `cognee-lib`'s config (`set_llm_*` / `set_embedding_*` live in
[crates/lib/src/config.rs](../../crates/lib/src/config.rs); see the project guide's `cognee-lib` entry). It should
accept a JSON schema `Value` and route it into the cognify config used by the
pipeline. Name it `set_summarization_model` for Python symmetry even though the
argument is a JSON schema rather than a class.

### Step 4.3 — Wire it into `SummaryExtractor`

`SummaryExtractor` currently takes only the `Llm`. Give it an optional schema and
branch on it in `extract_summary`:

```rust
pub struct SummaryExtractor {
    llm: Arc<dyn Llm>,
    summary_schema: Option<serde_json::Value>,   // None ⇒ built-in SummarizedContent
}

// in extract_summary():
let summarized: SummarizedContent = match &self.summary_schema {
    None => self.llm.create_structured_output::<SummarizedContent>(
        text, system_prompt, options,
    ).await?,
    Some(schema) => {
        let raw: serde_json::Value = self.llm
            .create_structured_output_raw(text, system_prompt, schema, options)
            .await?;
        // Python reads `.summary`; require a string `summary` field.
        let summary = raw.get("summary").and_then(|v| v.as_str())
            .ok_or_else(|| CognifyError::LlmError(
                "summary_schema output missing string `summary` field".into()))?;
        SummarizedContent { summary: summary.to_string(), ..Default::default() }
    }
};
```

Thread the schema from `summarize_text`
([tasks.rs:882](../../crates/cognify/src/tasks.rs#L882)):
`SummaryExtractor::new_with_schema(llm, config.summary_schema.clone())` (keep
`new(llm)` as a thin wrapper passing `None` for source compatibility).

Decide how strictly to map a custom schema's output back to the internal
`SummarizedContent`/`TextSummary`. Minimum viable: extract the `summary` string
(matches Python's `.summary` access). If callers want extra custom fields
persisted, that is a larger change to `TextSummary` payloads — out of scope here;
document the `summary`-field requirement.

### Step 4.4 — Validate the schema shape

Reuse or mirror the existing validation pattern (`graph_schema_to_graph_model`,
[crates/llm/src/dynamic_model.rs:245](../../crates/llm/src/dynamic_model.rs#L245)) to reject a `summary_schema`
that lacks a `summary` string property early (at config/setter time), rather than
failing mid-pipeline on the first chunk.

---

## Files touched

- [crates/cognify/src/config.rs](../../crates/cognify/src/config.rs) — `summary_schema` field + builder
- [crates/cognify/src/summarization/extractor.rs](../../crates/cognify/src/summarization/extractor.rs) — schema-aware extraction
- [crates/cognify/src/tasks.rs](../../crates/cognify/src/tasks.rs) — pass `config.summary_schema` to the extractor
- [crates/lib/src/config.rs](../../crates/lib/src/config.rs) — `set_summarization_model` runtime setter
- (consider) [crates/llm/src/dynamic_model.rs](../../crates/llm/src/dynamic_model.rs) — schema validation helper

## Acceptance criteria

- `summary_schema = None` (default) is byte-for-byte unchanged behavior: built-in
  `SummarizedContent`, typed path.
- With a custom schema containing a `summary` field, the summarization stage uses
  the dynamic path and `TextSummary.text` comes from the schema's `summary`
  output. A unit test with a mock `Llm` asserts the dynamic branch is taken and
  the text is extracted.
- A schema lacking a `summary` string field is rejected with a clear error at
  config/setter time.
- `set_summarization_model` is reachable from the `cognee-lib` public API.
- Config still round-trips through serde.

## Risks / notes

- **Scope guard:** this implements *schema* customization (Python parity), not a
  per-stage LLM. If a separate-LLM-for-summarization feature is ever wanted, that
  is a Rust-only extension beyond Python and should be its own ticket (and would
  use a `SummaryLlm(Arc<dyn Llm>)` newtype, since `Arc<dyn Llm>` is not `Debug`/
  `Serialize` and `CognifyConfig` derives both).
- Follow-up worth filing: `CognifyConfig.graph_schema` is currently dead in the
  standalone pipeline — wiring it into graph extraction is the symmetric fix and
  would reuse the same dynamic-output approach.
