# Task 04-06 — Add `cognee.llm.{model,provider}` fields to `OpenAIAdapter`

**Status**: ✅ implemented in commit d3409e9
**Owner**: _unassigned_
**Depends on**:
- [Task 04-02](02-tracing-constants-dedupe.md) — needs `cognee_utils::tracing_keys::COGNEE_LLM_MODEL`/`COGNEE_LLM_PROVIDER`.
**Blocks**:
- [Task 04-10 — Tests](10-tests.md) — OpenAI-side test cases.

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decisions**: #5 (INFO level), #8 (no feature gate).

---

## 1. Goal

The two existing `#[instrument]` blocks on `OpenAIAdapter` already
create `llm.api_call` and `llm.transcription_api_call` spans, but
they record only the `url` field. Add **two more fields** to each
span:

| Span | File | Line | Add |
|---|---|---|---|
| `llm.api_call` | [`crates/llm/src/adapters/openai.rs:138`](../../crates/llm/src/adapters/openai.rs#L138) | 138 | `cognee.llm.model = self.model`, `cognee.llm.provider = "openai"` |
| `llm.transcription_api_call` | [`crates/llm/src/adapters/openai.rs:729`](../../crates/llm/src/adapters/openai.rs#L729) | 729 | `cognee.llm.model = self.transcription_model`, `cognee.llm.provider = "openai"` |

This unblocks the question "which model answered this request?"
in both `/api/v1/activity/spans` views and OTLP consumers.

The LiteRT adapter is handled in [task 04-07](07-litert-llm-fields.md)
because it is feature-gated and has a different call shape.

## 2. Rationale

Python's `_enrich_llm_span()` helper
(`cognee/infrastructure/llm/structured_output_framework/litellm_instructor/llm/generic_llm_api/adapter.py:38-55`)
sets `cognee.llm.model` and `cognee.llm.provider` on the *current*
span (the surrounding `@observe`-decorated litellm wrapper) rather
than starting a new one. The Rust analogue is `tracing::Span::current().record(...)`
or — cleaner — declaring the fields directly in the
`#[instrument(... fields(...))]` macro since `self.model` is known at
function entry.

This is a one-line change per span (plus the constant import) and is
the smallest closure of the LLM portion of the gap.

## 3. Pre-conditions

- Task 04-02 (constants dedupe) is complete so `cognee_utils::tracing_keys`
  exposes `COGNEE_LLM_MODEL` and `COGNEE_LLM_PROVIDER`.
  *(Side note: those constants already exist in
  `cognee_utils::tracing_keys` today; task 04-02 only consolidates
  the duplicates from `cognee-search`. So this task can land before
  04-02 in a pinch.)*
- `cognee-llm` does **not** currently depend on `cognee-utils`. This
  task adds that edge.
- A clean `cargo check --all-targets` on `main`.

## 4. Step-by-step

### 4.1 Add `cognee-utils` dep to `cognee-llm`

Edit [`crates/llm/Cargo.toml`](../../crates/llm/Cargo.toml). Confirm
`tracing = { workspace = true }` is already present (the file uses
`use tracing::{debug, instrument, warn};` already). Add:

```toml
[dependencies]
# ... existing ...
cognee-utils = { path = "../utils" }
```

### 4.2 Update `call_api`

The current annotation at
[`crates/llm/src/adapters/openai.rs:138`](../../crates/llm/src/adapters/openai.rs#L138)
reads:

```rust
#[instrument(name = "llm.api_call", skip(self, request_body), fields(url = tracing::field::Empty))]
async fn call_api(&self, request_body: Value) -> LlmResult<OpenAIResponse> {
    let url = format!("{}/chat/completions", self.base_url);
    tracing::Span::current().record("url", url.as_str());
    // ...
}
```

Change to:

```rust
#[instrument(
    name = "llm.api_call",
    level = "info",
    skip(self, request_body),
    fields(
        url = tracing::field::Empty,
        cognee.llm.model = self.model.as_str(),
        cognee.llm.provider = "openai",
    ),
)]
async fn call_api(&self, request_body: Value) -> LlmResult<OpenAIResponse> {
    let url = format!("{}/chat/completions", self.base_url);
    tracing::Span::current().record("url", url.as_str());
    // ... rest unchanged ...
}
```

Important detail: `tracing`'s `fields(...)` evaluates Rust
expressions in the function's scope, including `self`. This compiles
because `&self` is in scope. Do **not** wrap `self.model.as_str()`
in a closure — `fields()` is not lazy.

The literal `"openai"` is fine as a static string. We use the
constant names defined in `cognee_utils::tracing_keys` for
documentation purposes only (the field-path syntax `cognee.llm.model`
is required by `tracing`'s macro grammar — string constants cannot
substitute).

### 4.3 Update `call_transcription_api`

The annotation at
[`crates/llm/src/adapters/openai.rs:729`](../../crates/llm/src/adapters/openai.rs#L729)
currently reads:

```rust
#[instrument(name = "llm.transcription_api_call", skip(self, form), fields(url = tracing::field::Empty))]
async fn call_transcription_api(
    &self,
    form: reqwest::multipart::Form,
) -> LlmResult<WhisperResponse> {
    let url = format!("{}/audio/transcriptions", self.base_url);
    tracing::Span::current().record("url", url.as_str());
    // ...
}
```

Change to:

```rust
#[instrument(
    name = "llm.transcription_api_call",
    level = "info",
    skip(self, form),
    fields(
        url = tracing::field::Empty,
        cognee.llm.model = self.transcription_model.as_str(),
        cognee.llm.provider = "openai",
    ),
)]
async fn call_transcription_api(
    &self,
    form: reqwest::multipart::Form,
) -> LlmResult<WhisperResponse> {
    let url = format!("{}/audio/transcriptions", self.base_url);
    tracing::Span::current().record("url", url.as_str());
    // ... rest unchanged ...
}
```

Confirm at task time that `self.transcription_model` is the field
name on `OpenAIAdapter` for the Whisper model (search for
`transcription_model` in
[`crates/llm/src/adapters/openai.rs`](../../crates/llm/src/adapters/openai.rs)).
If the field is named differently (e.g. `whisper_model`), substitute
accordingly.

### 4.4 Imports

The `tracing::instrument` macro is already imported. No new
`cognee_utils` import is strictly required at this site because the
field names are field-paths (not Rust identifiers), but adding

```rust
use cognee_utils::tracing_keys::{COGNEE_LLM_MODEL, COGNEE_LLM_PROVIDER};
```

at the top of the file documents the cross-reference. The
constants are not used in code — they're imported for the doc
benefit and grepability.

> **Recommendation:** include the import. It costs nothing, and it
> makes "which spans use which keys" greppable from
> `cognee_utils::tracing_keys`.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. cognee-llm compiles in isolation.
cargo check -p cognee-llm

# 3. Existing LLM tests still pass.
cargo test -p cognee-llm

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

The structured-attribute assertion lands in
[task 04-10](10-tests.md), where a mock OpenAI server fires a
chat-completion call and the test asserts the recorded
`cognee.llm.model` matches the configured model.

## 6. Files modified

- [`crates/llm/Cargo.toml`](../../crates/llm/Cargo.toml) — add
  `cognee-utils = { path = "../utils" }`.
- [`crates/llm/src/adapters/openai.rs`](../../crates/llm/src/adapters/openai.rs)
  — extend the two `#[instrument]` macros with `cognee.llm.model` and
  `cognee.llm.provider` fields; add the
  `cognee_utils::tracing_keys::{COGNEE_LLM_MODEL, COGNEE_LLM_PROVIDER}`
  import.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Field path with dots (`cognee.llm.model`) is parsed by `tracing` as a structural attribute, not a Rust expression | None — already used in adapter sub-docs (04-04, 04-05) and in `cognee-search/observability` examples in production. | n/a |
| `self.transcription_model` field name has changed | Low — verify at task time. | Sub-agent A re-greps. |
| The OpenAI adapter is used with a non-OpenAI base_url (Ollama, vLLM) and the literal `"openai"` is misleading | Real — many users point this adapter at Ollama. The Python equivalent records `name` (the litellm-resolved provider). | Acceptable for now: the adapter is named `OpenAIAdapter` and the spec is "API shape", not provider truth. A future enhancement could parse `base_url` and record `"ollama"` etc., but that goes beyond this task's scope. Document the limitation in the test comments. |
| `level = "info"` collides with an EnvFilter that suppresses info | Possible — deployments may run with `RUST_LOG=warn`. | Mirrors decision 5 (INFO for all adapter spans) and Python parity; no change. Operators tune verbosity. |

## 8. Out of scope

- LiteRT adapter — separate task ([04-07](07-litert-llm-fields.md)).
- Recording `cognee.llm.input_tokens` / `cognee.llm.output_tokens`.
  Python doesn't, and the OTEL semantic conventions for LLM tokens
  are still in flux. Future gap.
- Recording `cognee.llm.temperature` or `cognee.llm.max_tokens`.
  Same reasoning.
- Recording the request_id / response_id headers from the OpenAI
  response. Useful for debugging but not part of Python's surface.
