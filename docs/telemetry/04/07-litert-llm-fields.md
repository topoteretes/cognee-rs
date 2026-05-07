# Task 04-07 — Add `cognee.llm.{model,provider}` fields to `LiteRtAdapter`

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 04-02](02-tracing-constants-dedupe.md) — `COGNEE_LLM_MODEL`/`COGNEE_LLM_PROVIDER` constants.
**Blocks**: —

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decisions**: #4 (LiteRT in its own task), #5 (INFO level), #8 (no feature gate beyond the existing `android-litert` cfg).

---

## 1. Goal

Add a `llm.litert_call` `#[tracing::instrument]` block to the two
methods on `LiteRtAdapter` that perform inference, recording the
same `cognee.llm.{model,provider}` fields as the OpenAI adapter:

| Method | File | Line | Span name | Fields |
|---|---|---|---|---|
| `Llm::generate` | [`crates/llm/src/adapters/litert.rs:691`](../../crates/llm/src/adapters/litert.rs#L691) | 691 | `llm.litert_call` | `cognee.llm.model = self.model`, `cognee.llm.provider = "litert"` |
| `Llm::create_structured_output_with_messages_raw` | [`crates/llm/src/adapters/litert.rs`](../../crates/llm/src/adapters/litert.rs) (around line 720+) | tbd | `llm.litert_structured_call` | same |

Confirm exact method positions at task time — the LiteRT adapter
is feature-gated behind `android-litert` and may move between
ports.

The span complements the OpenAI adapter spans from [task 04-06](06-openai-llm-fields.md)
so traces from Android builds carry the same `cognee.llm.*` attributes
as cloud builds.

## 2. Rationale

LiteRT is a local Android-only inference engine; it does not make
HTTP calls and therefore does not have the `url` field that the
OpenAI spans carry. We use a different span name to signal that
distinction (`llm.litert_call` vs `llm.api_call`), but the
*payload-relevant* fields (`cognee.llm.model`,
`cognee.llm.provider`) match so OTLP consumers can group both
backends by model.

The adapter exists at
[`crates/llm/src/adapters/litert.rs`](../../crates/llm/src/adapters/litert.rs)
and is feature-gated behind `android-litert`. The `#[instrument]`
attribute itself is unconditional (locked decision 8); the file
already lives behind `#[cfg(feature = "android-litert")]` at the
crate level (or via a `#[cfg]`-gated `mod.rs` line — verify at task
time), so the code path is only compiled on Android builds.

This task is split out from 04-06 because:

1. The adapter has a different signature (no HTTP, no `request_body`,
   no `form`) and a different span name.
2. Bundling would force `#[cfg(feature = "android-litert")]` blocks
   into the OpenAI task's diff, making it harder to review.
3. The adapter is locked behind a platform-specific feature; testing
   it requires Android tooling that the host CI lane doesn't run.

## 3. Pre-conditions

- Task 04-02 (constants dedupe) is complete (or, equivalently, the
  pre-existing `cognee_utils::tracing_keys::COGNEE_LLM_MODEL` /
  `COGNEE_LLM_PROVIDER` constants are still present).
- `cognee-llm` already depends on `cognee-utils` (added by [task 04-06](06-openai-llm-fields.md)).
  If 04-07 lands before 04-06, this task adds the dep itself.
- LiteRT adapter file exists at
  [`crates/llm/src/adapters/litert.rs`](../../crates/llm/src/adapters/litert.rs)
  and exposes `LiteRtAdapter` with a `model: String` field.

## 4. Step-by-step

### 4.1 Confirm the call sites

```bash
grep -n "fn generate\|fn create_structured_output_with_messages_raw" \
    crates/llm/src/adapters/litert.rs
```

Both methods live inside `impl Llm for LiteRtAdapter`. Confirm
exact lines and field name (`self.model`) before editing.

### 4.2 Add `#[instrument]` to `generate`

The current method (truncated):

```rust
async fn generate(
    &self,
    messages: Vec<Message>,
    options: Option<GenerationOptions>,
) -> LlmResult<GenerationResponse> {
    // ... uses self.model.clone() inside spawn_blocking ...
}
```

Add the annotation:

```rust
#[instrument(
    name = "llm.litert_call",
    level = "info",
    skip_all,
    fields(
        cognee.llm.model = self.model.as_str(),
        cognee.llm.provider = "litert",
    ),
)]
async fn generate(
    &self,
    messages: Vec<Message>,
    options: Option<GenerationOptions>,
) -> LlmResult<GenerationResponse> {
    // body unchanged
}
```

`skip_all` keeps `messages` (potentially long, possibly containing
PII) and `options` out of the span. The model name is the only
identifying attribute we want to attach; the prompt itself is never
recorded.

### 4.3 Add `#[instrument]` to `create_structured_output_with_messages_raw`

The exact line is around 740 in the current source (confirm at task
time). Add:

```rust
#[instrument(
    name = "llm.litert_structured_call",
    level = "info",
    skip_all,
    fields(
        cognee.llm.model = self.model.as_str(),
        cognee.llm.provider = "litert",
    ),
)]
async fn create_structured_output_with_messages_raw(
    &self,
    mut messages: Vec<Message>,
    json_schema: &Value,
    options: Option<GenerationOptions>,
) -> LlmResult<Value> {
    // body unchanged
}
```

The schema is small (compact JSON) but treating it as opaque is
safer; `skip_all` keeps it out of the span.

### 4.4 Imports

Confirm `use tracing::{debug, instrument, warn};` (or similar) is
already imported at the top of the file. The current source uses
`use tracing::{debug, warn};` — the implementor needs to add
`instrument` to that import group:

```rust
use tracing::{debug, instrument, warn};
```

Documentation-only:

```rust
use cognee_utils::tracing_keys::{COGNEE_LLM_MODEL, COGNEE_LLM_PROVIDER};
```

The constants are not used in code (the field-path syntax in the
`#[instrument]` macro requires literal identifier paths), but the
import documents the cross-reference.

### 4.5 (Optional) Cover the lower-level `run_prompt` helper

`Self::run_prompt(engine, prompt, options)` is the actual blocking
call inside `spawn_blocking`. Wrapping it would add a child span
inside `llm.litert_call`. **Do not do this** in this task — Python's
litellm adapter wraps the outer async surface, not the lower-level
inference call. Match Python's granularity.

## 5. Verification

```bash
# 1. Compile (host, no feature flag — verifies the file still parses).
cargo check --all-targets

# 2. Compile with the android-litert feature for the LLM crate.
cargo check -p cognee-llm --features android-litert

# 3. Clippy on both lanes.
cargo clippy --all-targets -- -D warnings
cargo clippy -p cognee-llm --features android-litert -- -D warnings

# 4. Full check.
scripts/check_all.sh
```

Real test coverage of the LiteRT span requires Android tooling; the
host-side smoke is sufficient for this task. A device-side test
would belong in `android/` and is out of scope.

## 6. Files modified

- [`crates/llm/Cargo.toml`](../../crates/llm/Cargo.toml) — add
  `cognee-utils = { path = "../utils" }` if not already present
  (likely already added by [task 04-06](06-openai-llm-fields.md)).
- [`crates/llm/src/adapters/litert.rs`](../../crates/llm/src/adapters/litert.rs)
  — add `#[instrument]` to `generate` and
  `create_structured_output_with_messages_raw`; extend the `tracing`
  import with `instrument`; add the (optional, documentation-only)
  `cognee_utils::tracing_keys::*` import.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| The LiteRT adapter file is not compiled on host CI; clippy on the host lane misses any errors in this code | Real. | Sub-agent C runs `cargo check -p cognee-llm --features android-litert` and `cargo clippy -p cognee-llm --features android-litert`. CI has an Android lane in `lib-tests.yml` already. |
| `spawn_blocking` runs the actual inference on a different thread, and the span context does not automatically follow | Yes — `tracing` spans are entered by `#[instrument]` for the duration of the wrapping `async fn`. The `spawn_blocking` task does not inherit the span, but the parent span (`llm.litert_call`) stays open until `await` resolves, so the span attribute *recording* still works correctly because `cognee.llm.{model,provider}` are set at entry. | n/a — we only need entry-time attributes; no `record(...)` calls inside the blocking task. |
| `self.model.as_str()` evaluation order vs. `&self` borrow | None — `fields()` evaluates Rust expressions in scope at function entry; `&self` is in scope. | n/a |
| Future LiteRT adapter signature changes | Low — the adapter is stable; new methods would inherit the pattern. | n/a |

## 8. Out of scope

- Instrumenting `Self::run_prompt` (the blocking inference helper).
  Mirror Python's outer-only granularity.
- Recording token counts, latency, or finish reason as span attributes.
  Future telemetry gap.
- Wiring an Android device-side test. The smoke check builds the
  feature; real coverage requires hardware-in-loop.
- A `transcribe_audio` span — LiteRT is text-only and the trait
  method on `LiteRtAdapter` returns "unsupported" today.
