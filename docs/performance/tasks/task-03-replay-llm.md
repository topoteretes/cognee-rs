# T3 — `ReplayLlm` content-aware mock

**Status:** Implemented
**Crate:** `cognee-llm` (`mock` feature)
**Depends on:** T1 (T2 for the round-trip test)
**Unblocks:** T4

---

## Rationale

This is the Rust equivalent of Python's `_install_mocks()` LLM substitution — the
thing that lets the whole pipeline run with **no API calls**. Where Python matches
by title substring and returns hand-authored graphs, the Rust mock matches by the
T1 content hash and returns the **recorded** response from a cassette. Same chunk
in → same graph out, regardless of batching or call order.

## Expected output

- `crates/llm/src/mock/replay.rs` with:
  ```rust
  pub enum MissPolicy { EmptyGraph, Error }   // EmptyGraph = default (Python parity)
  pub struct ReplayLlm { cassette: LlmCassette, miss: MissPolicy, model: String }
  impl ReplayLlm {
      pub fn from_path(path: impl AsRef<Path>) -> LlmResult<Self>;
      pub fn with_miss_policy(self, p: MissPolicy) -> Self;
  }
  impl Llm for ReplayLlm { /* replay by hash */ }
  ```

## Step-by-step implementation

1. **Constructor.** `from_path` loads via `LlmCassette::load` (T1). Default
   `miss = MissPolicy::EmptyGraph`. `model` = cassette's `model` field (so
   `self.model()` reports what was recorded).

2. **Implement `Llm`:**
   - `create_structured_output_with_messages_raw(messages, schema, opts)`: compute
     `input_hash(&messages, Some(schema))`. On hit, return
     `entry.response.clone()`. On miss, apply `miss`:
     - `EmptyGraph` → **branch on the response schema** (the two callers expect
       incompatible shapes):
       - Graph extraction — `KnowledgeGraph`
         ([`fact_extraction/models.rs`](../../../crates/cognify/src/fact_extraction/models.rs),
         fields `nodes`/`edges`, both `#[serde(default)]`): return
         `json!({"nodes": [], "edges": []})` (matches Python's empty
         `KnowledgeGraph(nodes=[], edges=[])`).
       - Summarization — `SummarizedContent`
         ([`summarization/models.rs`](../../../crates/cognify/src/summarization/models.rs),
         fields `summary: String` + `description: String`, **both required**): an
         empty graph object would fail to deserialize, so return the stub
         `json!({"summary": "Mock summary.", "description": ""})` (matches Python's
         `SummarizedContent(summary="Mock summary.", description="")`).
       - Detect which via the schema's `title` (the `JsonSchema` derive emits the
         type name) or by probing for known fields; default to the empty graph.
     - `Error` → `Err(LlmError::…)` describing the missing hash + input preview.
   - `generate(messages, opts)`: hash with `None` schema; hit → wrap the stored
     string in a `GenerationResponse { content, model: self.model.clone(), usage:
     None, finish_reason: Some("stop") }`; miss → policy.
   - `transcribe_image(bytes, mime, opts)`: `vision_hash`; hit → the stored
     string; miss → `LlmError::FeatureNotSupported`-style or policy error.
   - `model()` returns `&self.model`. Leave other introspection methods at the
     trait defaults unless a recorded value is available.

3. **Empty-graph helper.** Factor the empty/stub responses into small private fns
   so the miss policy is unit-testable in isolation.

4. **Tests:**
   - **Round-trip (closes the T2 loop):** record a known graph through
     `RecordingLlm` over a **local stub `Llm`** (define one in the test module, as
     T2's `recording.rs` tests do — do **not** use `cognee_test_utils::MockLlm`:
     `cognee-test-utils` depends on `cognee-llm` *without* the `mock` feature, so
     pulling it into a `mock`-feature test target builds two distinct copies of the
     `Llm` trait that fail to unify — see the note in `recording.rs`), flush, load
     with `ReplayLlm`, and assert the replayed `Value` equals the original.
   - Hit returns the recorded value.
   - Miss with `EmptyGraph` returns `{"nodes":[],"edges":[]}`.
   - Miss with `Error` returns `Err`.
   - `generate` and `transcribe_image` hit/miss paths.

## Acceptance / verification

- `cargo test -p cognee-llm --features mock` passes, including the record→replay
  round-trip.
- Wire a `ReplayLlm` into a minimal cognify unit/integration path (or defer the
  full pipeline check to T6) to confirm a recorded graph flows through extraction.
- `cargo clippy -p cognee-llm --features mock -- -D warnings` clean.
