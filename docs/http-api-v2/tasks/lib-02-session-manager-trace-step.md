# LIB-02 — `cognee-session` `add_agent_trace_step()` parity

| | |
|---|---|
| Scope | Add agent-trace-step storage to `SessionManager` + `SessionStore`. |
| Status | **Not Started** |
| Blocks | LIB-01, E-02, E-12 (sessions detail returns trace tail). |
| Effort | ~1 day. |
| Owner crate | `cognee-session`. |

## 1. Goal

`SessionManager` needs a typed equivalent of Python's [`add_agent_trace_step`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/session/session_manager.py) so that `TraceEntry` payloads from `remember_entry()` and the cognee-mcp tracing wrapper can persist into the session cache. The detail endpoint (E-12) reads them back via `get_agent_trace_session()`.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `add_agent_trace_step` | `cognee/infrastructure/session/session_manager.py` | grep `def add_agent_trace_step` |
| `get_agent_trace_session` | same file | grep `def get_agent_trace_session` |
| `TraceEntry` shape | `cognee/memory/entries.py` | 34–50 |

Trace step payload fields (mirror these as columns / cache keys):

```
origin_function: str
status: "success" | "error"
method_params: Optional[dict]
method_return_value: Optional[Any]
memory_query: str
memory_context: str
error_message: str
generate_feedback_with_llm: bool
created_at: datetime  (server-set)
trace_id: uuid       (server-set, returned)
```

## 3. Current Rust state

- `crates/session/src/session_store.rs` defines `SessionStore` with `save_qa`, `read_history`, `add_feedback`, etc. — **no** trace step methods.
- `crates/session/src/session_manager.rs` exposes Q/A operations but nothing trace-related (`grep TraceEntry crates/session/` returns nothing).
- `crates/session/src/types.rs` defines `SessionQAEntry` only.
- Three backends exist (`fs_store.rs`, `redis_store.rs`, `sea_orm_store.rs`); all three need a parallel implementation.

## 4. Implementation steps

1. **Add `SessionTraceStep` type** in `crates/session/src/types.rs`:
   ```rust
   pub struct SessionTraceStep {
       pub trace_id: String,
       pub session_id: String,
       pub user_id: String,
       pub origin_function: String,
       pub status: TraceStatus,            // enum { Success, Error }
       pub method_params: Option<serde_json::Value>,
       pub method_return_value: Option<serde_json::Value>,
       pub memory_query: String,
       pub memory_context: String,
       pub error_message: String,
       pub generate_feedback_with_llm: bool,
       pub created_at: chrono::DateTime<chrono::Utc>,
   }
   ```
   `serde` + Python-compatible field naming (`snake_case`).

2. **Extend `SessionStore` trait**:
   ```rust
   async fn save_trace_step(&self, step: SessionTraceStep) -> Result<String, SessionError>;
   async fn read_trace_steps(
       &self, user_id: &str, session_id: &str,
   ) -> Result<Vec<SessionTraceStep>, SessionError>;
   ```
   Default impls return `SessionError::Unsupported` so existing backends compile until each is implemented.

3. **Implement on each backend**:
   - `fs_store.rs` — append-only `traces.jsonl` next to `qas.jsonl` per session dir.
   - `redis_store.rs` — `LPUSH cognee:trace:{user_id}:{session_id}` with JSON body; `LRANGE` on read.
   - `sea_orm_store.rs` — new SeaORM entity `SessionTraceStepEntity` (table `session_trace_steps`, PK `(user_id, session_id, trace_id)`, indexed on `created_at`); add a migration in `crates/session/src/migrator/`.

4. **Add `SessionManager` wrappers**:
   ```rust
   pub async fn add_agent_trace_step(&self, …all TraceEntry fields…) -> Result<String, SessionError>;
   pub async fn get_agent_trace_session(&self, user_id: &str, session_id: &str)
       -> Result<Vec<SessionTraceStep>, SessionError>;
   ```
   - `add_agent_trace_step`: server-generates `trace_id` = `uuid::Uuid::new_v4().to_string()` (Python uses UUID4); fills `created_at = Utc::now()`; calls `store.save_trace_step`.
   - `get_agent_trace_session`: forwards to `store.read_trace_steps`. Caller (`improve()` Stage 2 + E-12) tail-truncates as needed.

5. **Update `MockSessionStore` in `cognee-test-utils`** to match.

## 5. Tests

- `crates/session/tests/test_trace_steps.rs` (new):
  - `test_add_and_read_trace_step` — round-trip on each backend (parameterized).
  - `test_trace_step_uuid_uniqueness` — 100 inserts, 100 distinct `trace_id`s.
  - `test_get_agent_trace_session_orders_by_created_at`.
  - `test_unimplemented_backend_returns_unsupported` — sanity for the trait default impl.

## 6. Acceptance criteria

- [ ] `SessionTraceStep` type added to `cognee_session::types`.
- [ ] `SessionStore::save_trace_step` + `read_trace_steps` implemented on all three backends.
- [ ] `SessionManager::add_agent_trace_step` returns the generated `trace_id`.
- [ ] SeaORM migration lands; `cargo run --bin cognee-cli -- run-sequence` smoke test still passes.
- [ ] All existing session tests still pass.

## 7. References

- [Python session_manager.py](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/session/session_manager.py)
- [LIB-01 — remember_entry facade](lib-01-remember-entry-facade.md)
- [E-12 — sessions detail](e-12-sessions-detail.md)
