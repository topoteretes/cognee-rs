# Gap 6: Session Management

**Status: Implemented**

This document details the session management capabilities present in the Python SDK that are absent or incomplete in the Rust implementation.

> Implementation plan: [`impl/06-session-management-plan.md`](impl/06-session-management-plan.md)

---

## Python Session Architecture

### Public API

**File:** `cognee/api/v1/session/session.py`

| Function | Signature | Purpose |
|----------|-----------|---------|
| `get_session` | `(session_id="default_session", last_n=None, user=None) -> List[SessionQAEntry]` | Retrieve Q&A history from session cache |
| `add_feedback` | `(session_id, qa_id, feedback_text=None, feedback_score=None, user=None) -> bool` | Add feedback (text and/or score) to a Q&A entry |
| `delete_feedback` | `(session_id, qa_id, user=None) -> bool` | Clear feedback from a Q&A entry |

### SessionManager

**File:** `cognee/infrastructure/session/session_manager.py`

Key methods:
- `add_qa(*, user_id, question, context, answer, session_id=None, feedback_text=None, feedback_score=None, used_graph_element_ids=None) -> Optional[str]` -- saves Q&A entry, returns qa_id
- `update_qa(*, user_id, qa_id, question=None, context=None, answer=None, feedback_text=None, feedback_score=None, memify_metadata=None, session_id=None) -> bool` -- partial update of Q&A entry fields
- `delete_qa(*, user_id, qa_id, session_id=None) -> bool` -- deletes a Q&A entry
- `get_session(*, user_id, last_n=None, formatted=False, session_id=None, include_context=True)` -- retrieves entries (list or formatted string)
- `delete_session(*, user_id, session_id=None) -> bool` -- deletes entire session and its graph context
- `add_feedback(*, user_id, qa_id, feedback_text=None, feedback_score=None, session_id=None) -> bool` -- convenience over `update_qa` that also resets `memify_metadata.feedback_weights_applied`
- `delete_feedback(*, user_id, qa_id, session_id=None) -> bool` -- clears feedback via cache `delete_feedback` method
- `generate_completion_with_session(*, session_id, query, context, ...) -> Any` -- LLM completion with session history, auto-feedback detection, and graph context prepending
- `get_graph_context(*, user_id, session_id=None) -> str` -- retrieves graph knowledge snapshot (stored as a plain key in Redis or FS cache)
- `set_graph_context(*, user_id, session_id=None, context) -> None` -- stores graph knowledge snapshot

### SessionQAEntry Fields (Python)

**File:** `cognee/infrastructure/databases/cache/models.py`

```python
class SessionQAEntry(BaseModel):
    time: str                                              # ISO timestamp
    question: str
    context: str
    answer: str
    qa_id: Optional[str] = None                            # UUID4 string
    feedback_text: Optional[str] = None                    # User feedback text
    feedback_score: Optional[int] = None                   # Rating 1-5 (validated)
    used_graph_element_ids: Optional[Dict[str, List[str]]] = None  # {"node_ids": [...], "edge_ids": [...]}
    memify_metadata: Optional[Dict[str, bool]] = None      # e.g. {"feedback_weights_applied": true}
```

Note: Python model has `time` (string) and `qa_id` fields; Rust uses `id` (Uuid) and `created_at` (DateTime).

### Cache Backend Interface

**File:** `cognee/infrastructure/databases/cache/cache_db_interface.py`

Abstract methods: `create_qa_entry`, `get_latest_qa_entries`, `get_all_qa_entries`, `update_qa_entry`, `delete_feedback`, `delete_qa_entry`, `delete_session`, `prune`, `close`, `log_usage`, `get_usage_logs`.

Implementations: `FsCacheAdapter` (diskcache), `RedisAdapter`.

### Feedback Flow

1. User calls `search()` or `recall()` with `session_id` -- Q&A entry saved with `used_graph_element_ids`
2. User calls `add_feedback(session_id, qa_id, score=5)` -- feedback stored on entry, `memify_metadata.feedback_weights_applied` reset to `false`
3. User calls `improve(session_ids=[session_id])` -- feedback weights applied to graph edges

### Graph Context

- `get_graph_context()`: Returns stored knowledge snapshot for session (plain string in Redis/FS cache)
- `set_graph_context()`: Called by `improve()` Stage 4 (`sync_graph_to_session`) to store graph knowledge back to session
- Used by `generate_completion_with_session()` to prepend graph-derived knowledge to conversation history

---

## Rust Session Architecture

### SessionQAEntry Fields (Rust)

**File:** `crates/session/src/types.rs`

```rust
pub struct SessionQAEntry {
    pub id: Uuid,                    // (Python: qa_id as String)
    pub session_id: String,
    pub user_id: Option<String>,     // (Python: not in model, passed separately)
    pub question: String,
    pub answer: String,
    pub context: Option<String>,     // (Python: required str)
    pub created_at: DateTime<Utc>,   // (Python: time as ISO string)
}
```

### SessionStore Trait

**File:** `crates/session/src/session_store.rs`

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_qa_entry(&self, session_id: &str, user_id: Option<&str>,
        question: &str, answer: &str, context: Option<&str>) -> Result<String, SessionError>;
    async fn get_latest_qa_entries(&self, session_id: &str, user_id: Option<&str>,
        last_n: usize) -> Result<Vec<SessionQAEntry>, SessionError>;
    async fn get_all_qa_entries(&self, session_id: &str,
        user_id: Option<&str>) -> Result<Vec<SessionQAEntry>, SessionError>;
    async fn delete_session(&self, session_id: &str,
        user_id: Option<&str>) -> Result<bool, SessionError>;
    async fn delete_qa_entry(&self, session_id: &str, user_id: Option<&str>,
        qa_id: &str) -> Result<bool, SessionError>;
    async fn prune(&self) -> Result<(), SessionError>;
}
```

### SessionManager

**File:** `crates/session/src/session_manager.rs`

```rust
impl SessionManager {
    pub fn new(store: Arc<dyn SessionStore>) -> Self
    pub fn with_default_session_id(mut self, id: impl Into<String>) -> Self
    pub fn with_history_limit(mut self, limit: usize) -> Self
    pub async fn load_history_messages(&self, session_id: Option<&str>,
        user_id: Option<&str>) -> Result<Vec<Message>, SessionError>
    pub async fn load_history_both(&self, session_id: Option<&str>,
        user_id: Option<&str>) -> Result<(Vec<Message>, String), SessionError>
    pub async fn save_qa(&self, session_id: Option<&str>, user_id: Option<&str>,
        question: &str, answer: &str, context: Option<&str>) -> Result<String, SessionError>
    pub async fn delete_session(&self, session_id: Option<&str>,
        user_id: Option<&str>) -> Result<bool, SessionError>
    pub fn format_entries(entries: &[SessionQAEntry]) -> String  // static method
}
```

### Store Implementations

- `FsSessionStore` -- filesystem-based (feature: `fs`). Internal `FsQAEntry` already has `feedback_text`, `feedback_score`, `used_graph_element_ids`, `memify_metadata` fields but they are **not mapped** into the domain `SessionQAEntry`.
- `RedisSessionStore` -- Redis-based (feature: `redis`). Internal `RedisQAEntry` also has the four feedback fields but they are **not mapped** into `SessionQAEntry`.
- `SeaOrmSessionStore` -- database-based (feature: `sea-orm-store`). DB schema has **no feedback columns** -- only `id`, `session_id`, `user_id`, `question`, `answer`, `context`, `created_at`.

---

## Gap Analysis

| # | Feature | Python | Rust | Status |
|---|---------|--------|------|--------|
| 1 | **Create Q&A entry** | `add_qa()` | `save_qa()` / `create_qa_entry()` | Implemented |
| 2 | **Get session entries** | `get_session()` with `last_n` + `formatted` | `load_history_messages()`, `get_latest_qa_entries()`, `get_all_qa_entries()` | Implemented |
| 3 | **Delete session** | `delete_session()` | `delete_session()` | Implemented |
| 4 | **Delete Q&A entry** | `delete_qa()` | `delete_qa_entry()` | Implemented |
| 5 | **Prune all sessions** | `prune()` | `prune()` | Implemented |
| 6 | **Format entries** | `format_entries()` (with `include_context` param) | `format_entries()` (no `include_context`) | Partial -- missing `include_context` parameter |
| 7 | **Feedback text field** | `feedback_text` on `SessionQAEntry` | Missing from domain struct | Not implemented |
| 8 | **Feedback score field** | `feedback_score` on `SessionQAEntry` | Missing from domain struct | Not implemented |
| 9 | **Graph element tracking** | `used_graph_element_ids` dict | Missing from domain struct | Not implemented |
| 10 | **Memify metadata** | `memify_metadata` dict | Missing from domain struct | Not implemented |
| 11 | **Update Q&A entry** | `update_qa()` | Missing | Not implemented |
| 12 | **Add feedback** | `add_feedback()` (API + SessionManager) | Missing | Not implemented |
| 13 | **Delete feedback** | `delete_feedback()` (API + SessionManager + CacheDBInterface) | Missing | Not implemented |
| 14 | **Graph context get** | `get_graph_context()` | Missing | Not implemented |
| 15 | **Graph context set** | `set_graph_context()` | Missing | Not implemented |
| 16 | **LLM completion with session** | `generate_completion_with_session()` | Missing | Not implemented |
| 17 | **Auto-feedback detection** | Via `CacheConfig.auto_feedback` flag in completion | Missing | Not implemented |
| 18 | **FS/Redis field mapping** | N/A | `FsQAEntry`/`RedisQAEntry` have feedback fields but `*_entry_to_domain()` discards them | Not implemented |
| 19 | **SeaORM schema** | N/A | DB table missing feedback/tracking columns | Not implemented |
| 20 | **Public session API** | `cognee/api/v1/session/session.py` (`get_session`, `add_feedback`, `delete_feedback`) | No equivalent module in `cognee-lib` | Not implemented |

### Notes on existing partial work

The FS and Redis store implementations already serialize/deserialize `feedback_text`, `feedback_score`, `used_graph_element_ids`, and `memify_metadata` in their internal entry structs (`FsQAEntry`, `RedisQAEntry`). However, the conversion functions (`fs_entry_to_domain`, `redis_entry_to_domain`) discard these fields because the domain `SessionQAEntry` struct does not have them. This means cross-SDK reads of Python-written entries will lose feedback data.
