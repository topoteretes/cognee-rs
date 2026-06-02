# Implementation Plan: Missing API Functions (Gap 2)

> **Status (2026-06): COMPLETED.** All six functions described in this plan have
> been implemented under `crates/lib/src/api/` (`forget.rs`, `update.rs`,
> `prune.rs`, `recall.rs`, `remember.rs`, `improve.rs`). Some final signatures
> differ from the proposals below (e.g. `forget()` uses a `ForgetTarget` enum
> and `improve()` takes an `ImproveParams` struct), but the functionality is in
> place. This plan is retained as a historical record.

Detailed step-by-step plan for implementing the 6 missing high-level API functions.
Each section specifies exact files to create/modify, proposed signatures, and dependencies.

Reference gap descriptions: [`../02-missing-functions.md`](../02-missing-functions.md)

---

## Recommended Implementation Order

1. **`forget()`** -- thin wrapper over `DeleteService`; no new primitives needed
2. **`update()`** -- composition of existing delete + add + cognify
3. **`prune`** -- requires trait extensions but each is straightforward
4. **`recall()`** -- session keyword search + rule-based query router
5. **`remember()`** -- depends on `improve()` for full feature parity
6. **`improve()`** -- most complex; feedback system, session persistence, graph sync

---

## 1. `forget()` -- Unified Deletion API

**Complexity:** Low

### Files to Create

- `crates/lib/src/api/mod.rs` -- new `api` module declaration (shared by all 6 functions)
- `crates/lib/src/api/forget.rs` -- public `forget()` function

### Files to Modify

- `crates/lib/src/lib.rs` -- add `pub mod api;` and re-export `api::forget::forget`

### Proposed Signature

```rust
// crates/lib/src/api/forget.rs

/// Reference for dataset: either a name or a UUID.
pub enum DatasetRef {
    Name(String),
    Id(Uuid),
}

/// Summary returned after a forget operation.
pub struct ForgetResult {
    pub status: String,
    pub datasets_removed: usize,
    pub data_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
}

pub async fn forget(
    data_id: Option<Uuid>,
    dataset: Option<DatasetRef>,
    everything: bool,
    owner_id: Uuid,
    delete_service: &DeleteService,
    db: &DatabaseConnection,
) -> Result<ForgetResult, DeleteError>
```

### Steps

1. Create `crates/lib/src/api/mod.rs` with `pub mod forget;`.
2. Implement `forget()` that maps parameters to `DeleteScope` variants:
   - `everything=true` -> `DeleteScope::User { owner_id }` (deletes all user data)
   - `dataset=Some(_), data_id=None` -> resolve name via `IngestDb::get_dataset_by_name()`, then `DeleteScope::Dataset { owner_id, dataset_name }`
   - `dataset=Some(_), data_id=Some(_)` -> `DeleteScope::Data { owner_id, data_id, dataset_name, delete_dataset_if_empty: false }`
   - Validate: `data_id` without `dataset` is an error
3. Call `delete_service.execute(scope)` or `delete_service.preview(scope)`.
4. Add `pub mod api;` to `crates/lib/src/lib.rs`.
5. Re-export in prelude.

### Dependencies

None -- all primitives exist.

---

## 2. `update()` -- Data Replacement

**Complexity:** Low

### Files to Create

- `crates/lib/src/api/update.rs`

### Files to Modify

- `crates/lib/src/api/mod.rs` -- add `pub mod update;`

### Proposed Signature

```rust
// crates/lib/src/api/update.rs

pub struct UpdateResult {
    pub deleted_data_id: Uuid,
    pub new_data: Vec<Data>,
    pub cognify_result: CognifyResult,
}

pub async fn update(
    data_id: Uuid,
    new_data: Vec<DataInput>,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    delete_service: &DeleteService,
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    cognify_config: &CognifyConfig,
) -> Result<UpdateResult, UpdateError>
```

### Steps

1. Delete old data: `delete_service.execute(DeleteScope::Data { owner_id, data_id, dataset_name: Some(dataset_name.into()), delete_dataset_if_empty: false })`.
2. Re-add: `add_pipeline.add(new_data, dataset_name, owner_id, tenant_id)`.
3. Re-cognify: `cognify(added_data, dataset_id, ...)`.
4. Return combined result.

### Dependencies

- `forget()` only if we want to reuse it internally; otherwise direct `DeleteService` usage is fine.

---

## 3. `prune` -- System/Data Cleanup

**Complexity:** Low-Medium

### Files to Create

- `crates/lib/src/api/prune.rs`

### Files to Modify

- `crates/lib/src/api/mod.rs` -- add `pub mod prune;`
- `crates/storage/src/storage_trait.rs` (line ~53) -- add `remove_all()` method to `StorageTrait`
- `crates/storage/src/local_storage.rs` -- implement `remove_all()` for `LocalStorage`
- `crates/storage/src/mock_storage.rs` -- implement `remove_all()` for `MockStorage`
- `crates/vector/src/vector_db_trait.rs` (line ~96) -- add `prune()` method to `VectorDB` trait (default: list_collections + delete each)
- `crates/vector/src/qdrant_adapter.rs` -- implement `prune()` for `QdrantAdapter`
- `crates/vector/src/pgvector_adapter.rs` -- implement `prune()` for `PgVectorAdapter`
- `crates/vector/src/mock_vector_db.rs` -- implement `prune()` for `MockVectorDB`

### Proposed Signatures

```rust
// StorageTrait addition
async fn remove_all(&self) -> Result<(), StorageError>;

// VectorDB addition (with default impl)
async fn prune(&self) -> VectorDBResult<()> {
    let collections = self.list_collections().await?;
    for (data_type, field_name) in collections {
        self.delete_collection(&data_type, &field_name).await?;
    }
    Ok(())
}

// Public API
pub struct PruneResult {
    pub data_pruned: bool,
    pub graph_pruned: bool,
    pub vector_pruned: bool,
    pub metadata_pruned: bool,
    pub cache_pruned: bool,
}

pub async fn prune_data(
    storage: &dyn StorageTrait,
) -> Result<(), StorageError>;

pub async fn prune_system(
    graph: bool,
    vector: bool,
    metadata: bool,
    cache: bool,
    graph_db: Option<&dyn GraphDBTrait>,
    vector_db: Option<&dyn VectorDB>,
    session_store: Option<&dyn SessionStore>,
    // Note: metadata (DB drop) deferred -- see below
) -> Result<PruneResult, PruneError>;
```

### Steps

1. **`StorageTrait::remove_all()`**: List all files in the base directory, delete each. For `LocalStorage`, use `tokio::fs::read_dir` + `tokio::fs::remove_file`.
2. **`VectorDB::prune()`**: Add default implementation that calls `list_collections()` then `delete_collection()` for each. Backends can override with more efficient bulk ops.
3. **`GraphDBTrait::delete_graph()`**: Already exists! No change needed.
4. **Metadata (DB drop)**: The Python version calls `db_engine.delete_database()` which drops and recreates the entire relational database. In Rust, `DatabaseConnection` (SeaORM) does not have a `delete_database()` method. Two options:
   - Option A: Skip for initial implementation (mark as `metadata=false` default, same as Python's default).
   - Option B: For SQLite, delete the DB file and reinitialize. For PostgreSQL, drop all tables via migration rollback.
   - **Recommendation**: Option A for initial release; file an issue for Option B.
5. **Cache/session prune**: `SessionStore::prune()` already exists.
6. Wire up in `prune_data()` and `prune_system()`.

### Dependencies

- Trait extensions must land before the API functions.

---

## 4. `recall()` -- Smart Search with Session Routing

**Complexity:** Medium

### Files to Create

- `crates/lib/src/api/recall.rs`
- `crates/search/src/query_router.rs` -- rule-based query type classifier

### Files to Modify

- `crates/lib/src/api/mod.rs` -- add `pub mod recall;`
- `crates/search/src/lib.rs` -- add `pub mod query_router;`

### Proposed Signatures

```rust
// crates/search/src/query_router.rs

pub struct RouteResult {
    pub search_type: SearchType,
    pub confidence: f32,
    pub runner_up: SearchType,
    pub runner_up_score: f32,
}

pub fn route_query(query: &str) -> RouteResult;

// crates/lib/src/api/recall.rs

pub async fn recall(
    query_text: &str,
    query_type: Option<SearchType>,
    datasets: Option<Vec<String>>,
    top_k: usize,
    auto_route: bool,
    session_id: Option<&str>,
    user_id: Option<&str>,
    // Component references
    search_orchestrator: &SearchOrchestrator,
    session_store: Option<&dyn SessionStore>,
) -> Result<Vec<serde_json::Value>, RecallError>
```

### Steps

1. **Implement session keyword search** in `recall.rs`:
   - Tokenize query: split on word boundaries (`\b\w+\b`), filter `len >= 2`, lowercase.
   - Load session Q&A entries via `SessionStore::get_all_qa_entries(session_id, user_id)`.
   - Score each entry: count of `query_tokens intersection entry_tokens` (union of question, answer, context text).
   - Return top_k sorted by score, tagged with `_source: "session"`.
2. **Implement `route_query()`** in `crates/search/src/query_router.rs`:
   - Port the Python weighted-scoring heuristic: each regex pattern adds weight to a `SearchType`.
   - Rules to port (from Python `query_router.py`):
     - Cypher syntax detection (MATCH, RETURN, CREATE) -> `Cypher` (weight 10.0)
     - Coding rules keywords -> `CodingRules` (weight 5.0)
     - Quoted exact phrases -> `ChunksLexical` (weight 8.0)
     - Summary keywords -> `GraphSummaryCompletion` (weight 5.0)
     - Reasoning/CoT keywords -> `GraphCompletionCot` (weight 4.0)
     - Relationship keywords -> `GraphCompletionContextExtension` (weight 5.0)
     - Temporal keywords/years -> `Temporal` (weight 3.0-6.0)
     - Default fallback -> `GraphCompletion` (base score 2.0)
   - Include negation detection (suppress match if negation word within 20 chars before).
3. **Wire up `recall()`**:
   - If `session_id` provided and no `datasets`/`query_type`: try session search first, fall through to graph on empty results.
   - If `auto_route=true` and no `query_type`: call `route_query()`.
   - If `auto_route=false` and no `query_type`: default to `GraphCompletion`.
   - Tag all graph results with `_source: "graph"`.
4. Add exports and re-exports.

### Dependencies

- `SearchOrchestrator` and `SessionStore` already exist.
- No dependency on other gap functions.

---

## 5. `remember()` -- Smart Ingestion with Session Bridging

**Complexity:** Medium

### Files to Create

- `crates/lib/src/api/remember.rs`

### Files to Modify

- `crates/lib/src/api/mod.rs` -- add `pub mod remember;`

### Proposed Signatures

```rust
// crates/lib/src/api/remember.rs

pub struct RememberResult {
    pub status: RememberStatus,
    pub dataset_name: String,
    pub dataset_id: Option<Uuid>,
    pub session_ids: Option<Vec<String>>,
    pub elapsed_seconds: Option<f64>,
    pub content_hash: Option<String>,
    pub items_processed: usize,
    pub items: Vec<RememberItemInfo>,
    pub error: Option<String>,
}

pub struct RememberItemInfo {
    pub id: Option<Uuid>,
    pub name: Option<String>,
    pub content_hash: Option<String>,
    pub mime_type: Option<String>,
}

pub enum RememberStatus {
    Running,
    Completed,
    Errored,
    SessionStored,
}

pub async fn remember(
    data: Vec<DataInput>,
    dataset_name: &str,
    session_id: Option<&str>,
    self_improvement: bool,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    // Components
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    cognify_config: &CognifyConfig,
) -> Result<RememberResult, RememberError>
```

### Steps

1. **Define `RememberResult`** and related types.
2. **Permanent mode (no `session_id`)**:
   - Call `add_pipeline.add(data, dataset_name, owner_id, tenant_id)`.
   - Call `cognify(data_items, dataset_id, ...)`.
   - If `self_improvement=true`, call `memify(graph_db, vector_db, embedding_engine, ...)`.
   - Populate `RememberResult` from results.
3. **Session mode (with `session_id`)**:
   - Convert data inputs to text representation.
   - Store via `SessionStore::create_qa_entry(session_id, user_id, question="", answer=text, context=None)`.
   - If `self_improvement=true`, call `improve()` (when available) or just `memify()`.
   - Return `RememberResult` with status `SessionStored`.
4. Note: background execution mode can be deferred (Rust callers can use `tokio::spawn` directly).

### Dependencies

- `improve()` for full `self_improvement` support with sessions.
- Without `improve()`, session mode can still store to session cache and run `memify()`.

---

## 6. `improve()` -- Bidirectional Session-Graph Bridge

**Complexity:** High

### Files to Create

- `crates/lib/src/api/improve.rs`

### Files to Modify

- `crates/lib/src/api/mod.rs` -- add `pub mod improve;`
- `crates/session/src/types.rs` -- add `feedback_score` and `used_graph_element_ids` fields to `SessionQAEntry`
- `crates/session/src/session_store.rs` -- add `update_qa_feedback()` method to trait
- `crates/session/src/sea_orm_store.rs` -- implement `update_qa_feedback()`
- `crates/session/src/fs_store.rs` -- implement `update_qa_feedback()`
- `crates/session/src/redis_store.rs` -- implement `update_qa_feedback()`
- `crates/graph/src/traits.rs` -- add `update_node_property()` and `update_edge_property()` methods to `GraphDBTrait`
- `crates/graph/src/ladybug.rs` -- implement property update methods
- `crates/graph/src/mock.rs` -- implement property update methods

### Proposed Signature

```rust
// crates/lib/src/api/improve.rs

pub struct ImproveResult {
    pub stages_run: Vec<String>,
    pub memify_result: Option<MemifyResult>,
    pub feedback_entries_processed: usize,
    pub sessions_persisted: usize,
    pub edges_synced: usize,
}

pub async fn improve(
    dataset_name: &str,
    session_ids: Option<Vec<String>>,
    node_name: Option<Vec<String>>,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    feedback_alpha: f64,
    // Components
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    cognify_config: &CognifyConfig,
) -> Result<ImproveResult, ImproveError>
```

### Steps

#### Stage 1: Apply Feedback Weights (only when `session_ids` provided)

1. Add to `SessionQAEntry`:
   ```rust
   pub feedback_score: Option<f64>,
   pub used_graph_element_ids: Vec<Uuid>,
   ```
   Note: `used_graph_element_ids` already exists as `Option<serde_json::Value>` in the FS/Redis store internal types. Need to expose it on the public `SessionQAEntry` type.
2. Add to `GraphDBTrait`:
   ```rust
   async fn update_node_property(
       &self,
       node_id: &str,
       key: &str,
       value: serde_json::Value,
   ) -> GraphDBResult<()>;
   ```
3. For each session entry with feedback:
   - Load entries via `SessionStore::get_all_qa_entries()`.
   - Filter entries that have `feedback_score` and `used_graph_element_ids`.
   - For each referenced graph element ID, call `update_node_property(id, "feedback_weight", current + alpha * feedback_score)`.

#### Stage 2: Persist Session Q&A to Graph (only when `session_ids` provided)

1. Load Q&A entries from session store.
2. Concatenate question + answer text.
3. Call `cognify()` on the text, tagged with metadata `node_set="user_sessions_from_cache"`.

#### Stage 3: Default Enrichment (always runs)

1. Call existing `memify()` / `run_memify()`.

#### Stage 4: Sync Graph to Session Cache (only when `session_ids` provided)

1. Read recent graph edges (from relational DB or graph DB).
2. Format as JSON-lines or structured summaries.
3. Store in session store as a special "graph_context" entry.
4. Requires adding a `set_graph_context()` / `get_graph_context()` method pair to `SessionStore` trait.

### Dependencies

- Trait extensions on `GraphDBTrait` (property updates).
- `SessionQAEntry` field additions.
- `SessionStore` trait additions (feedback update, graph context).
- `memify()` (already exists).
- `cognify()` (already exists).

---

## Cross-Cutting Concerns

### New `crates/lib/src/api/` Module Structure

```
crates/lib/src/api/
  mod.rs          -- pub mod forget; pub mod update; ... etc.
  forget.rs       -- forget()
  update.rs       -- update()
  prune.rs        -- prune_data(), prune_system()
  recall.rs       -- recall()
  remember.rs     -- remember()
  improve.rs      -- improve()
```

### Error Types

Each function should define its own error enum (or a shared `ApiError`) in `crates/lib/src/api/`:

```rust
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Delete error: {0}")]
    Delete(#[from] DeleteError),
    #[error("Ingestion error: {0}")]
    Ingestion(String),
    #[error("Cognify error: {0}")]
    Cognify(String),
    #[error("Search error: {0}")]
    Search(String),
    #[error("Session error: {0}")]
    Session(#[from] SessionError),
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("Component error: {0}")]
    Component(#[from] ComponentError),
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
}
```

### CLI Integration

After implementing each function, add corresponding subcommands to `crates/cli/`:

- `forget` subcommand (maps to `forget()`)
- `update` subcommand (maps to `update()`)
- `prune-data` / `prune-system` subcommands (maps to `prune_data()` / `prune_system()`)
- `recall` subcommand (maps to `recall()`)
- `remember` subcommand (maps to `remember()`)
- `improve` subcommand (maps to `improve()`)

### Language Binding Integration

After Rust implementation, update:
- `capi/` -- C FFI wrappers
- `python/` -- PyO3 wrappers
- `js/` -- Neon wrappers

### Testing Strategy

Each function should have:
1. Unit tests with mock backends (in-module `#[cfg(test)]` block)
2. Integration test in `crates/lib/tests/` or relevant crate's `tests/`
3. CLI E2E test in `crates/cli/tests/cli_e2e.rs`
4. Cross-SDK E2E test in `e2e-cross-sdk/` (parity with Python)
