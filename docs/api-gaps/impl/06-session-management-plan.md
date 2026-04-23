# Implementation Plan: Gap 6 -- Session Management

> Reference: [`docs/api-gaps/06-session-management.md`](../06-session-management.md)

---

## Overview

The Python session subsystem has four major capabilities that the Rust side lacks:

1. **Feedback fields and methods** -- `feedback_text`, `feedback_score`, `add_feedback()`, `delete_feedback()`
2. **Graph element tracking** -- `used_graph_element_ids` dict on each QA entry
3. **Memify metadata** -- `memify_metadata` dict on each QA entry
4. **Update QA entry** -- generic `update_qa()` method
5. **Graph context storage** -- `get_graph_context()` / `set_graph_context()` per-session knowledge snapshot
6. **LLM completion with session** -- `generate_completion_with_session()` (session-aware search)
7. **Auto-feedback detection** -- LLM-based feedback detection during completion
8. **Session keyword search** -- keyword overlap search over QA entries (used by `recall()`)

This plan covers phases 1-5 (data model + CRUD + graph context). Phases 6-8 (LLM completion, auto-feedback, keyword search) are deferred as they depend on retriever and search pipeline integration.

---

## Phase 1: Extend `SessionQAEntry` struct

**File:** `crates/session/src/types.rs`

Add the four missing fields to the domain struct with serde defaults for backward compatibility:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionQAEntry {
    pub id: Uuid,
    pub session_id: String,
    pub user_id: Option<String>,
    pub question: String,
    pub answer: String,
    pub context: Option<String>,
    pub created_at: DateTime<Utc>,
    // --- NEW fields ---
    #[serde(default)]
    pub feedback_text: Option<String>,
    #[serde(default)]
    pub feedback_score: Option<i32>,         // 1-5 rating, validated on write
    #[serde(default)]
    pub used_graph_element_ids: Option<UsedGraphElementIds>,
    #[serde(default)]
    pub memify_metadata: Option<HashMap<String, bool>>,
}
```

Also add a typed struct for graph element IDs (matching Python's `{"node_ids": [...], "edge_ids": [...]}`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsedGraphElementIds {
    #[serde(default)]
    pub node_ids: Vec<String>,
    #[serde(default)]
    pub edge_ids: Vec<String>,
}
```

**Impact on existing code:**
- All constructors of `SessionQAEntry` in tests and store implementations must add the new fields (set to `None`/default).
- The `FsQAEntry` and `RedisQAEntry` internal structs already have `feedback_text`, `feedback_score`, `used_graph_element_ids`, `memify_metadata` -- they just need to be mapped into the domain struct.
- The SeaORM entity needs new columns (Phase 3).

---

## Phase 2: Add `SessionQAUpdate` type and extend `SessionStore` trait

**File:** `crates/session/src/session_store.rs`

Add an update DTO:

```rust
/// Partial update for a QA entry. `None` fields are left unchanged;
/// `Some(None)` clears the field; `Some(Some(v))` sets it.
#[derive(Debug, Clone, Default)]
pub struct SessionQAUpdate {
    pub question: Option<String>,
    pub answer: Option<String>,
    pub context: Option<Option<String>>,
    pub feedback_text: Option<Option<String>>,
    pub feedback_score: Option<Option<i32>>,
    pub used_graph_element_ids: Option<Option<UsedGraphElementIds>>,
    pub memify_metadata: Option<Option<HashMap<String, bool>>>,
}
```

Extend the `SessionStore` trait with three new methods:

```rust
#[async_trait]
pub trait SessionStore: Send + Sync {
    // ... existing methods ...

    /// Update fields on a QA entry. Only non-None fields in `updates` are applied.
    /// Returns true if the entry was found and updated.
    async fn update_qa_entry(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError>;

    /// Retrieve the graph knowledge snapshot for a session, or None.
    async fn get_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError>;

    /// Store (or overwrite) the graph knowledge snapshot for a session.
    async fn set_graph_context(
        &self,
        session_id: &str,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), SessionError>;
}
```

---

## Phase 3: Update store implementations

### 3a. `FsSessionStore` (`crates/session/src/fs_store.rs`)

**`update_qa_entry`:** Load JSON array, find entry by `qa_id`, apply non-None fields, save.

**`get_graph_context` / `set_graph_context`:** Store as a separate file `{base_dir}/{user_id}/_graph_context_{session_id}.json` containing a plain string. This mirrors Python's per-key approach.

**`fs_entry_to_domain`:** Map the existing `FsQAEntry` fields (`feedback_text`, `feedback_score`, `used_graph_element_ids`, `memify_metadata`) into the new `SessionQAEntry` fields. Currently these are deserialized but discarded during conversion.

### 3b. `RedisSessionStore` (`crates/session/src/redis_store.rs`)

**`update_qa_entry`:** Load all entries from Redis list, find by `qa_id`, apply updates, rewrite the list. (Same pattern as `delete_qa_entry`.)

**`get_graph_context` / `set_graph_context`:** Use Redis key `graph_knowledge:{user_id}:{session_id}` (matching Python's `SessionManager._graph_context_key()`).

**`redis_entry_to_domain`:** Same as FS -- map the four fields that are already deserialized into the domain struct.

### 3c. `SeaOrmSessionStore` (`crates/session/src/sea_orm_store.rs`)

**Schema migration:** Add a new migration (`m20250423_000002_session_qa_feedback_fields.rs`) that ALTERs the `session_qa_entries` table to add:
- `feedback_text TEXT NULL`
- `feedback_score INTEGER NULL`
- `used_graph_element_ids TEXT NULL` (JSON string)
- `memify_metadata TEXT NULL` (JSON string)

Also create a `session_graph_context` table:
- `id TEXT PRIMARY KEY` (composite key as `{user_id}:{session_id}`)
- `user_id TEXT`
- `session_id TEXT NOT NULL`
- `context TEXT NOT NULL`
- `updated_at TIMESTAMP WITH TIME ZONE NOT NULL`

**Entity update:** Add the four new columns to `entity::Model`. Add a new entity for graph context.

**Ops update:** Add `update_qa_entry`, `get_graph_context`, `set_graph_context` operations.

**`model_to_entry`:** Map the new columns into `SessionQAEntry`.

### 3d. `MockSessionStore` (if used in test-utils)

Check if `cognee-test-utils` has a mock session store and update accordingly.

---

## Phase 4: Add feedback + graph context methods to `SessionManager`

**File:** `crates/session/src/session_manager.rs`

```rust
impl SessionManager {
    /// Update arbitrary fields on a QA entry.
    pub async fn update_qa(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        qa_id: &str,
        updates: SessionQAUpdate,
    ) -> Result<bool, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store.update_qa_entry(resolved_id, user_id, qa_id, updates).await
    }

    /// Add or update feedback on a QA entry (convenience over update_qa).
    /// Resets memify_metadata.feedback_weights_applied to false.
    pub async fn add_feedback(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        qa_id: &str,
        feedback_text: Option<&str>,
        feedback_score: Option<i32>,
    ) -> Result<bool, SessionError> {
        let mut memify = HashMap::new();
        memify.insert("feedback_weights_applied".to_string(), false);

        self.update_qa(session_id, user_id, qa_id, SessionQAUpdate {
            feedback_text: Some(feedback_text.map(|s| s.to_string())),
            feedback_score: Some(feedback_score),
            memify_metadata: Some(Some(memify)),
            ..Default::default()
        }).await
    }

    /// Clear feedback from a QA entry.
    pub async fn delete_feedback(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        qa_id: &str,
    ) -> Result<bool, SessionError> {
        self.update_qa(session_id, user_id, qa_id, SessionQAUpdate {
            feedback_text: Some(None),
            feedback_score: Some(None),
            ..Default::default()
        }).await
    }

    /// Retrieve graph knowledge snapshot for a session.
    pub async fn get_graph_context(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store.get_graph_context(resolved_id, user_id).await
    }

    /// Store graph knowledge snapshot for a session.
    pub async fn set_graph_context(
        &self,
        session_id: Option<&str>,
        user_id: Option<&str>,
        context: &str,
    ) -> Result<(), SessionError> {
        let resolved_id = self.resolve_session_id(session_id);
        self.store.set_graph_context(resolved_id, user_id, context).await
    }
}
```

---

## Phase 5: Public API functions

**File:** `crates/lib/src/api/session.rs` (new)

Thin wrappers that mirror the Python `cognee/api/v1/session/session.py` functions:

```rust
pub async fn get_session(
    manager: &SessionManager,
    session_id: &str,
    user_id: Option<&str>,
    last_n: Option<usize>,
) -> Result<Vec<SessionQAEntry>, SessionError>;

pub async fn add_feedback(
    manager: &SessionManager,
    session_id: &str,
    qa_id: &str,
    user_id: Option<&str>,
    feedback_text: Option<&str>,
    feedback_score: Option<i32>,
) -> Result<bool, SessionError>;

pub async fn delete_feedback(
    manager: &SessionManager,
    session_id: &str,
    qa_id: &str,
    user_id: Option<&str>,
) -> Result<bool, SessionError>;
```

Re-export from `crates/lib/src/lib.rs`.

---

## Phase 6 (deferred): Session keyword search

Add to `SessionManager`:

```rust
pub fn search_entries(
    entries: &[SessionQAEntry],
    query: &str,
    top_k: usize,
) -> Vec<&SessionQAEntry>;
```

Simple keyword overlap (tokenize, intersect, rank). Used by `recall()` in the search pipeline.

---

## Phase 7 (deferred): LLM completion with session history

Port `generate_completion_with_session()` -- depends on search pipeline integration, prompt templates, and the `recall()` function.

---

## Phase 8 (deferred): Auto-feedback detection

Port `auto_feedback` config flag and LLM-based feedback detection during completion. Depends on Phase 7.

---

## Files to create/modify (Phases 1-5)

| File | Action |
|------|--------|
| `crates/session/src/types.rs` | **Modify** -- add feedback/tracking fields + `UsedGraphElementIds` struct |
| `crates/session/src/session_store.rs` | **Modify** -- add `SessionQAUpdate`, `update_qa_entry()`, graph context methods |
| `crates/session/src/session_manager.rs` | **Modify** -- add `update_qa`, `add_feedback`, `delete_feedback`, graph context methods |
| `crates/session/src/fs_store.rs` | **Modify** -- implement new trait methods, map fields in `fs_entry_to_domain` |
| `crates/session/src/redis_store.rs` | **Modify** -- implement new trait methods, map fields in `redis_entry_to_domain` |
| `crates/session/src/sea_orm_store.rs` | **Modify** -- implement new trait methods |
| `crates/session/src/sea_orm_backend/entity.rs` | **Modify** -- add four new columns |
| `crates/session/src/sea_orm_backend/ops.rs` | **Modify** -- add update + graph context ops |
| `crates/session/src/migrator/` | **Create** -- new migration for feedback columns + graph context table |
| `crates/lib/src/api/session.rs` | **Create** -- public API wrappers |
| `crates/lib/src/lib.rs` | **Modify** -- re-export session API |

---

## Test plan

- Unit tests for `SessionQAUpdate` application logic
- Integration tests for each store: create entry with feedback -> read back -> verify fields
- Integration tests for `update_qa_entry` (set, clear, partial update)
- Integration tests for `add_feedback` / `delete_feedback` round-trip
- Integration tests for `get_graph_context` / `set_graph_context` round-trip
- Backward compatibility: read entries saved without feedback fields (serde default)
- SeaORM migration: verify ALTER TABLE on existing DB with data
- Cross-SDK: verify JSON format matches Python's `FsCacheAdapter` and `RedisAdapter`
