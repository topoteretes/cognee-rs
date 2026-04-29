# LIB-01 — `cognee-lib` `remember_entry()` facade + `MemoryEntry` types

| | |
|---|---|
| Scope | New library function + new shared types. |
| Status | **Not Started** |
| Blocks | E-02 (`POST /remember/entry`). |
| Depends on | **LIB-06** (adds `RememberResult.entry_type` / `entry_id` fields + the new `RememberStatus` CamelCase enum — see Decision 15 / Q-F), **LIB-02** (`SessionManager::add_agent_trace_step`). |
| Effort | ~0.5 day. |
| Owner crates | `cognee-models`, `cognee-lib`. |

## 1. Goal

Land a Rust counterpart of Python's discriminated-union dispatch in [`cognee/api/v1/remember/remember.py:262`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L262):

```python
if isinstance(entry, QAEntry):    qa_id   = await sm.add_qa(...)
if isinstance(entry, TraceEntry): trace_id = await sm.add_agent_trace_step(...)
if isinstance(entry, FeedbackEntry): ok    = await sm.add_feedback(...)
```

The HTTP endpoint at [`POST /api/v1/remember/entry`](../../http-api-v2/tasks/e-02-remember-entry.md) calls `cognee.api.v1.remember(entry, ...)` to perform this dispatch — Rust needs the same surface.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `QAEntry` | `cognee/memory/entries.py` | 18–31 |
| `TraceEntry` | `cognee/memory/entries.py` | 34–50 |
| `FeedbackEntry` | `cognee/memory/entries.py` | 53–64 |
| `MemoryEntry` (Union alias) | `cognee/memory/entries.py` | 67 |
| Dispatch logic | `cognee/api/v1/remember/remember.py` | 262–311 |
| `RememberResult.entry_type / entry_id` | `cognee/api/v1/remember/remember.py` | ~360 |

## 3. Current Rust state

- `crates/lib/src/api/remember.rs` exposes `remember(data: Vec<DataInput>, ...)` only (file/text path).
- No `MemoryEntry`, `QAEntry`, `TraceEntry`, `FeedbackEntry` types anywhere in the workspace (`grep -rn 'TraceEntry\|FeedbackEntry' crates/` returns nothing).
- `RememberResult` exists in `crates/lib/src/api/remember.rs` but lacks `entry_type` and `entry_id` fields.

## 4. Implementation steps

> **Decision (2026-04-29)**: `MemoryEntry` types live in `cognee-models`, not in a new dedicated `cognee-memory` crate. Rationale: `cognee-models` is the project's "data structures, no traits" crate per [`.claude/CLAUDE.md`](../../../.claude/CLAUDE.md), which is the right shape for these pure-data discriminated-union types; spinning up a new crate adds workspace noise without separating any meaningful concern. Investigation agent: do not re-litigate.

1. **Add `MemoryEntry` types** in a new module `crates/models/src/memory.rs` (re-exported from `cognee_models::memory`):
   ```rust
   /// Discriminator values stay snake_case ("qa", "trace", "feedback") per Python's
   /// Literal["qa"] / Literal["trace"] / Literal["feedback"]. The inner field
   /// names on each variant are camelCase on the wire per Decision 10.
   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(tag = "type", rename_all = "snake_case")]   // discriminator → snake; variant
                                                       // structs handle their own field casing
   pub enum MemoryEntry {
       Qa(QAEntry),                                    // type discriminator: "qa"
       Trace(TraceEntry),                              // "trace"
       Feedback(FeedbackEntry),                        // "feedback"
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "camelCase")]                  // wire field names per Decision 10
   pub struct QAEntry {
       pub question: String,                           // single-word
       pub answer: String,                             // single-word
       #[serde(default)]
       pub context: String,                            // single-word; default ""
       #[serde(default, alias = "feedback_text")]
       pub feedback_text: Option<String>,              // wire: "feedbackText"
       #[serde(default, alias = "feedback_score")]
       pub feedback_score: Option<i32>,                // wire: "feedbackScore"
       #[serde(default, alias = "used_graph_element_ids")]
       pub used_graph_element_ids: Option<serde_json::Value>,  // wire: "usedGraphElementIds"
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "camelCase")]
   pub struct TraceEntry {
       #[serde(alias = "origin_function")]
       pub origin_function: String,                    // wire: "originFunction"
       #[serde(default)]
       pub status: TraceStatus,                        // enum Success | Error → "success"/"error"
       #[serde(default, alias = "method_params")]
       pub method_params: Option<serde_json::Value>,   // wire: "methodParams"
       #[serde(default, alias = "method_return_value")]
       pub method_return_value: Option<serde_json::Value>,  // wire: "methodReturnValue"
       #[serde(default, alias = "memory_query")]
       pub memory_query: String,                       // wire: "memoryQuery"; default ""
       #[serde(default, alias = "memory_context")]
       pub memory_context: String,                     // wire: "memoryContext"; default ""
       #[serde(default, alias = "error_message")]
       pub error_message: String,                      // wire: "errorMessage"; default ""
       #[serde(default, alias = "generate_feedback_with_llm")]
       pub generate_feedback_with_llm: bool,           // wire: "generateFeedbackWithLlm"; default false
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   #[serde(rename_all = "camelCase")]
   pub struct FeedbackEntry {
       #[serde(alias = "qa_id")]
       pub qa_id: String,                              // wire: "qaId"; required
       #[serde(default, alias = "feedback_text")]
       pub feedback_text: Option<String>,              // wire: "feedbackText"
       #[serde(default, alias = "feedback_score")]
       pub feedback_score: Option<i32>,                // wire: "feedbackScore"
   }
   ```
   Field-for-field with Python (see `cognee/memory/entries.py`). `QAEntry::context` defaults to `""`, `TraceEntry::status` defaults to `"success"`, `TraceEntry::generate_feedback_with_llm` defaults to `false`. The `serde(alias)` on every multi-word field accepts snake_case inputs (Python `populate_by_name=True` parity).

2. **`RememberResult.entry_type` / `entry_id` fields** — already present after LIB-06 lands (Q-F / Decision 15). LIB-01 just populates them in step 3 below; no struct extension is needed in this task. If for any reason LIB-06 was skipped or partially landed, the implementation agent must report **BLOCKED** and stop — do not re-add the fields here.

3. **Add `remember_entry()` facade** alongside the existing `remember()`:
   ```rust
   pub async fn remember_entry(
       entry: MemoryEntry,
       dataset_name: &str,
       session_id: &str,
       user: &User,
       components: &ComponentHandles,
   ) -> Result<RememberResult, RememberError>;
   ```
   - Returns `Err(RememberError::MissingSessionId)` when `session_id` is empty (Python raises `ValueError`).
   - Pre-upserts the `session_records` row via `ensure_and_touch_session` (LIB-03 dependency — accept `None` if dataset lookup fails, matching Python's `try/except → resolved_dataset = None`).
   - Match-dispatches:
     - `MemoryEntry::Qa(q)` → `SessionManager::add_qa(...)` → returns `qa_id`.
     - `MemoryEntry::Trace(t)` → `SessionManager::add_agent_trace_step(...)` → returns `trace_id` (LIB-02).
     - `MemoryEntry::Feedback(f)` → `SessionManager::add_feedback(...)` → returns `f.qa_id` on success.
   - Sets `result.entry_type` from the discriminator string (`"qa" | "trace" | "feedback"`).
   - Sets `result.entry_id` to the cache-returned id.
   - Sets `result.status = RememberStatus::SessionStored` on success, `RememberStatus::Errored` + populated `error` on failure. The library `RememberStatus` enum (CamelCase serde, owned by LIB-06 / Decision 15) serializes the variants as `"SessionStored"` / `"PipelineRunErrored"`. The HTTP DTO translates to Python's lowercase `"session_stored"` / `"errored"` at the wire boundary (E-01).

4. **Re-export from `cognee-lib`**:
   - `pub use cognee_models::memory::{MemoryEntry, QAEntry, TraceEntry, FeedbackEntry};`
   - `pub use api::remember::{remember, remember_entry, RememberResult};`

5. **No new error variants** beyond `RememberError::MissingSessionId` and `RememberError::SessionCacheUnavailable`. The HTTP layer maps these to 400 / 503 (E-02).

## 5. Tests

- `crates/lib/tests/test_remember_entry.rs` (new):
  - `test_qa_entry_dispatch_returns_qa_id` — uses `MockSessionStore`, asserts `entry_type=="qa"`, `entry_id` matches.
  - `test_trace_entry_dispatch` — same pattern via `add_agent_trace_step` (depends on LIB-02 mock).
  - `test_feedback_entry_dispatch_returns_qa_id_on_success`.
  - `test_feedback_entry_returns_errored_when_qa_missing`.
  - `test_missing_session_id_returns_error`.

## 6. Acceptance criteria

- [ ] `MemoryEntry` discriminated enum lives in `cognee-models` with `serde(tag="type")` and round-trips to the Python wire shape (verified by a JSON fixture test).
- [ ] `remember_entry()` exists in `cognee_lib::api::remember` and dispatches to the three `SessionManager` methods.
- [ ] `RememberResult.entry_type` and `entry_id` populated for all three branches.
- [ ] No `unwrap()` in non-test code.
- [ ] `cargo check --all-targets` clean; new tests pass.

## 7. References

- [Python `MemoryEntry`](https://github.com/topoteretes/cognee/blob/main/cognee/memory/entries.py)
- [Python remember dispatch](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L262)
- [E-02 — `POST /remember/entry`](e-02-remember-entry.md)
- [LIB-02 — `add_agent_trace_step`](lib-02-session-manager-trace-step.md)
