# Gap 07 — POST /remember/entry feedback LLM path

## Source / current state

The handler `post_remember_entry` lives in [crates/http-server/src/routers/remember.rs](../../../../crates/http-server/src/routers/remember.rs). The `MemoryEntry::Trace` arm at lines 714–750 currently contains a live TODO marker:

```rust
// crates/http-server/src/routers/remember.rs:724-732
// TODO(LIB-01-followup): generate_feedback_with_llm requires
// wiring an `Arc<dyn Llm>` + prompt template through
// `SessionManager`. For now we always pass `session_feedback = ""`.
if generate_feedback_with_llm {
    tracing::debug!(
        session_id = %payload.session_id,
        "post_remember_entry: generate_feedback_with_llm=true \
         ignored (LIB-01-followup; passing empty session_feedback)"
    );
}

session_manager
    .add_agent_trace_step(
        &user_id_str,
        Some(&payload.session_id),
        &origin_function,
        &trace_status,
        &memory_query,
        &memory_context,
        method_params.unwrap_or(serde_json::Value::Null),
        method_return_value,
        &error_message,
        "",                       // <-- empty session_feedback
    )
    .await
    .map_err(map_session_err)?
```

Confirmed live behavior: when the client sends `generateFeedbackWithLlm: true`, the handler logs at `debug!`, **silently drops the request**, and passes an empty string to `SessionManager::add_agent_trace_step`. The trace step is still written; only the `session_feedback` column is empty.

Parallel implementation `cognee_lib::api::remember::remember_entry` in [crates/lib/src/api/remember.rs:792-828](../../../../crates/lib/src/api/remember.rs#L792-L828) has the same TODO and must remain byte-for-byte consistent (see handler-doc note at `remember.rs:567-569`).

Related types and APIs:

- `TraceEntry` DTO at [crates/models/src/memory.rs:85-115](../../../../crates/models/src/memory.rs#L85-L115). Fields: `origin_function: String`, `status: String` (default `"success"`), `method_params: Option<Value>`, `method_return_value: Option<Value>`, `memory_query: String`, `memory_context: String`, `error_message: String`, `generate_feedback_with_llm: bool`. All optionals default; aliases support both `camelCase` (primary) and `snake_case`.
- `SessionManager::add_agent_trace_step` at [crates/session/src/session_manager.rs:264-299](../../../../crates/session/src/session_manager.rs#L264-L299). Takes `session_feedback: &str` as the last positional argument. The struct holds only `store: Arc<dyn SessionStore>`, `default_session_id`, `history_limit` — it does NOT currently hold an `Arc<dyn Llm>`.
- `ComponentHandles.llm: Option<Arc<dyn Llm>>` is already wired ([crates/http-server/src/components.rs:54-55](../../../../crates/http-server/src/components.rs#L54-L55)).
- `Llm::generate(messages, options) -> LlmResult<GenerationResponse>` at [crates/llm/src/llm_trait.rs:17-23](../../../../crates/llm/src/llm_trait.rs#L17-L23). `GenerationResponse.content: String`. `GenerationOptions` carries `temperature`, `max_tokens`, etc. ([crates/llm/src/types.rs:44-70](../../../../crates/llm/src/types.rs#L44-L70)).
- `MockLlm` exists at [crates/test-utils/src/mock_llm.rs](../../../../crates/test-utils/src/mock_llm.rs) and pops canned responses from a queue — perfect for the deterministic-feedback test.

Python reference (canonical behavior):

- `cognee/infrastructure/session/session_manager.py:214-294` implements `_generate_agent_trace_feedback`. It uses `LLMGateway.acreate_structured_output` with the prompt at `cognee/infrastructure/llm/prompts/agent_trace_feedback_summary_system.txt` and the `AgentTraceFeedbackSummary` Pydantic model (single field `session_feedback: str`). On `None` return value or any exception → deterministic fallback `"<origin> succeeded."` / `"<origin> failed. Reason: <err>."` / `"<origin> failed."` (see lines 200–212).
- The full system prompt (5 short rules) is at `cognee/infrastructure/llm/prompts/agent_trace_feedback_summary_system.txt` — must be reproduced verbatim.
- `sanitize_value` at `cognee/modules/agent_memory/sanitization.py` truncates strings to `MAX_SERIALIZED_VALUE_LENGTH = 1000` and caps containers at `MAX_TRACE_CONTAINER_ITEMS = 20`. The serialized JSON of `method_return_value` is the LLM `text_input`.

## Strategy

Two viable approaches:

**A. Carry the `Arc<dyn Llm>` into `SessionManager`** — clean per-component composition but widens the session crate's surface (requires `cognee-llm` dep in `cognee-session` and a `with_llm(...)` builder).

**B. Generate the feedback string in the HTTP handler before calling `add_agent_trace_step`** — keeps the session crate untouched and matches the existing call shape (`session_feedback: &str` already takes any externally produced string).

**Chosen: B.** Rationale:

1. `SessionManager` lives below the HTTP layer and is shared with non-LLM call sites (cache backends, lib facade). Forcing an `Arc<dyn Llm>` through its constructor would either make it mandatory (breaking session-only callers) or `Option`, replicating the same conditional logic at a deeper level with no payoff.
2. The handler already owns `components: Arc<ComponentHandles>` (already carries `Option<Arc<dyn Llm>>`) and is the only call site that needs LLM-backed feedback today.
3. Building the prompt + serializing `method_return_value` are short pure operations; isolating them in a `feedback` helper module keeps reuse simple.

## Implementation steps

### a. Extract the prompt template and helpers

Create `crates/http-server/src/routers/feedback.rs` (or extend `remember.rs` with a private `mod feedback`) and add:

```rust
const AGENT_TRACE_FEEDBACK_SYSTEM_PROMPT: &str = "\
Summarize the provided method return value as one short human-readable sentence.\n\
\n\
Rules:\n\
- Focus only on the meaning of the return value.\n\
- Keep it to a single concise sentence.\n\
- Do not mention JSON, serialization, or that this is a summary.\n\
- Do not invent details that are not present in the input.\n\
- If the return value is already short, rewrite it as a clear sentence.\n";

const FEEDBACK_LLM_TIMEOUT: Duration = Duration::from_secs(8);
const FEEDBACK_MAX_LEN: usize = 500;            // post-scrub cap
const SERIALIZED_RETURN_MAX_LEN: usize = 1000;  // Python parity
```

Helper signatures:

```rust
fn fallback_feedback(origin_function: &str, status: &str, error_message: &str) -> String;
fn truncate(s: &str, limit: usize) -> String;     // suffix "..."
fn scrub_feedback(raw: &str) -> String;           // strip control/ANSI; cap len
async fn generate_session_feedback(
    llm: &dyn Llm,
    origin_function: &str,
    status: &str,
    method_return_value: Option<&Value>,
    error_message: &str,
) -> String;
```

`fallback_feedback` mirrors Python `_fallback_agent_trace_feedback` byte-for-byte:

- status normalized to lower case;
- `error` + non-empty error_message → `"<origin> failed. Reason: <err>."`
- `error` + empty error_message → `"<origin> failed."`
- else → `"<origin> succeeded."`

### b. Where the LLM call lives

In the handler. `generate_session_feedback` takes `&dyn Llm` and pure-data inputs, building the messages and calling `Llm::generate`. The session crate stays unchanged.

### c. LLM call with timeout + graceful degradation

Inside `generate_session_feedback`:

1. If `method_return_value.is_none()` → return `fallback_feedback(...)` immediately (Python parity, `session_manager.py:229-230`).
2. Serialize the return value with `serde_json::to_string`; truncate to `SERIALIZED_RETURN_MAX_LEN` characters.
3. Build `messages = [Message::system(SYSTEM_PROMPT), Message::user(serialized)]`.
4. Build `GenerationOptions { temperature: Some(0.0), max_tokens: Some(120), ..Default::default() }` — feedback is one sentence; small budget keeps latency low.
5. Wrap the `llm.generate(...)` future with `tokio::time::timeout(FEEDBACK_LLM_TIMEOUT, ...)`.
6. Match outcomes:
   - `Ok(Ok(resp))` → `scrub_feedback(&resp.content)`; if empty after scrub → fallback.
   - `Ok(Err(e))` → `tracing::warn!(error = %e, ...)`; fallback. Do **not** include `resp.content` in the log if any.
   - `Err(_)` (elapsed) → `tracing::warn!(timeout_ms = ?, ...)`; fallback.
7. No `.unwrap()` / `.expect()`; all branches return a `String`.

### d. Scrubbing the LLM response

`scrub_feedback`:

- `trim()` whitespace.
- Reject empty → return empty string (caller falls back).
- Remove ASCII control chars except `\n` and space; collapse to spaces. Strip ANSI CSI sequences (`\x1b\[[0-9;]*[a-zA-Z]`) — use a const regex via `once_cell::sync::Lazy<Regex>` (already a dep). If `regex` is not on the http-server crate, do a small hand-rolled scan: `for ch in s.chars() { if ch.is_control() && ch != '\n' { skip } }` — equivalent for the ANSI ESC byte.
- Truncate to `FEEDBACK_MAX_LEN` (suffix `"..."`).
- Return the result; the caller is responsible for the fallback when the result is empty.

The full raw response is never logged. If we must log diagnostics, only log: model name, byte length, finish_reason — never `content`.

### e. Wire the handler

Edit [crates/http-server/src/routers/remember.rs](../../../../crates/http-server/src/routers/remember.rs) `MemoryEntry::Trace` arm:

```rust
let llm_opt = components.llm.clone();

let session_feedback: String = if generate_feedback_with_llm {
    if let Some(llm) = llm_opt.as_ref() {
        feedback::generate_session_feedback(
            llm.as_ref(),
            &origin_function,
            &trace_status,
            method_return_value.as_ref(),
            &error_message,
        )
        .await
    } else {
        tracing::warn!(
            session_id = %payload.session_id,
            "post_remember_entry: generate_feedback_with_llm=true but \
             ComponentHandles.llm is None — falling back to deterministic feedback"
        );
        feedback::fallback_feedback(&origin_function, &trace_status, &error_message)
    }
} else {
    // Python parity: when generate_feedback_with_llm=false the deterministic
    // fallback is still recorded (session_manager.py:289-294).
    feedback::fallback_feedback(&origin_function, &trace_status, &error_message)
};

session_manager
    .add_agent_trace_step(
        &user_id_str,
        Some(&payload.session_id),
        &origin_function,
        &trace_status,
        &memory_query,
        &memory_context,
        method_params.unwrap_or(serde_json::Value::Null),
        method_return_value,
        &error_message,
        &session_feedback,
    )
    .await
    .map_err(map_session_err)?
```

Important parity note: today, when `generate_feedback_with_llm` is `false`, the Rust handler **also** passes `""`, but Python's `add_agent_trace_step` writes the deterministic fallback string regardless (`session_manager.py:289-294`). This gap should be closed in the same change — the fallback is cheap and matches Python — but flag it as an intentional behavior bump. (If parity-strictness is preferred for the false branch in this gap, gate that with a feature flag and follow up.)

Mirror the same code path in [crates/lib/src/api/remember.rs:792-828](../../../../crates/lib/src/api/remember.rs#L792-L828) so `cognee_lib::api::remember::remember_entry` accepts an optional `Arc<dyn Llm>` and uses the same `feedback` helpers (extract them into a small shared module, e.g. `crates/session/src/feedback.rs` reachable from both crates, or duplicate inside the handler if cycle constraints bite — [crates/http-server/Cargo.toml:35-37](../../../../crates/http-server/Cargo.toml#L35-L37) already documents the cycle).

Remove the TODO comments in both files once the wiring is live.

## Tests

All tests go in [crates/http-server/tests/test_remember_entry.rs](../../../../crates/http-server/tests/test_remember_entry.rs) (extend the existing file). Use the existing `build_handles_with_session` scaffold + a new `build_handles_with_session_and_llm(llm: Arc<dyn Llm>)` variant that injects `MockLlm` into the `llm` field of `ComponentHandles`.

**Test A — `trace_entry_with_generate_feedback_llm_uses_mock_llm_text`**
- Build app with `MockLlm::new(vec![r#"This is a deterministic feedback."#.to_string()])`.
- POST `/api/v1/remember/entry` with `{type: "trace", originFunction: "f", status: "success", methodReturnValue: {"x": 1}, generateFeedbackWithLlm: true, sessionId: "s"}`.
- Assert response is 200, `entry_type == "trace"`, `entry_id` non-empty.
- Then call `session_manager.get_agent_trace_session(user_id, Some("s"), None)` (expose via a test helper or hit a read API) and assert the saved `session_feedback == "This is a deterministic feedback."`.

**Test B — `trace_entry_llm_error_falls_back_to_deterministic_feedback`**
- Use a custom test LLM (small `struct ErrLlm; impl Llm for ErrLlm { async fn generate(..) { Err(LlmError::Internal(...)) } ... }`) declared in the test module.
- POST the same payload as above, with `originFunction = "search"`, `status = "success"`.
- Assert 200, entry saved, and the persisted `session_feedback == "search succeeded."` (Python deterministic fallback).
- Use `tracing-test` or `tracing_subscriber::fmt` capture to assert a `WARN` event was emitted; if capture is heavy, alternatively rely on the fallback value to prove the error branch was taken.

**Test C — `trace_entry_llm_timeout_falls_back`**
- Define a `SlowLlm` that `tokio::time::sleep(Duration::from_secs(30))` inside `generate`. Lower `FEEDBACK_LLM_TIMEOUT` for tests via `cfg(test)` constant or expose via env override (`COGNEE_FEEDBACK_LLM_TIMEOUT_MS`).
- Assert the handler returns 200 within (say) 2 seconds; the saved `session_feedback` matches the deterministic fallback.

**Test D — preserve existing assertions in `test_remember_entry.rs`**
- Existing `trace_entry_returns_session_stored_with_entry_type_trace` (sends no `generateFeedbackWithLlm` ⇒ defaults to `false`) must still pass. With the parity-bump described above, this test should additionally assert `session_feedback == "search succeeded."` once a read path is available; otherwise just keep current behavior assertions intact.

(Optional, recommended) **Unit tests for the helpers** in `feedback.rs` itself: fallback messages for the three branches, scrub truncation, control-char removal, empty-after-scrub returns empty.

## Acceptance criteria

- [ ] `POST /api/v1/remember/entry` with a `trace` entry and `generateFeedbackWithLlm: true` invokes the configured LLM and persists its output (scrubbed) in the trace step's `session_feedback`.
- [ ] LLM errors and timeouts do not break the trace write; instead a deterministic fallback (`"<origin> succeeded."` / `"<origin> failed. Reason: ..."` / `"<origin> failed."`) is written and a single `WARN` log is emitted (no LLM content in the log).
- [ ] If `ComponentHandles.llm` is `None`, the handler logs a `WARN` once and writes the deterministic fallback.
- [ ] The LLM call is bounded by `FEEDBACK_LLM_TIMEOUT` (default 8s).
- [ ] The LLM response is scrubbed for ANSI/control characters and truncated to `FEEDBACK_MAX_LEN` (default 500 chars).
- [ ] No `.unwrap()` / `.expect()` introduced outside `#[cfg(test)]`.
- [ ] The TODO marker at [crates/http-server/src/routers/remember.rs:724-732](../../../../crates/http-server/src/routers/remember.rs#L724-L732) is removed.
- [ ] The mirror TODO at [crates/lib/src/api/remember.rs:804-813](../../../../crates/lib/src/api/remember.rs#L804-L813) is removed and `remember_entry` accepts/uses an optional `Arc<dyn Llm>`.
- [ ] `cargo test -p cognee-http-server --test test_remember_entry` passes (existing 7 tests + new Tests A/B/C).
- [ ] `cargo clippy -p cognee-http-server -- -D warnings` clean.

## Status

**landed** — merged into main as `492df0f`, gap commit `3479f33`. Deviations: (1) helpers duplicated between `crates/http-server/src/routers/feedback.rs` and `crates/lib/src/api/remember_feedback.rs` (the cycle constraint prevents sharing); functional surface is byte-identical, drift would be a latent bug. (2) Timeout uses `COGNEE_FEEDBACK_LLM_TIMEOUT_MS` env override (default 8 s) rather than a `cfg(test)` const, so the timeout-failure test can run integration-mode; the test is `#[serial_test::serial]` with an RAII `EnvGuard` to prevent leakage. (3) Parity bump locked: when `generate_feedback_with_llm=false`, the handler now writes the deterministic fallback (Python parity) instead of the previous Rust empty string.
