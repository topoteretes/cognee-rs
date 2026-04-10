# Task 22: Add `user` Parameter for Authorization Context to SearchRequest

## Summary

The Python `search()` API accepts an optional `user: User` parameter that carries the caller's identity (`user.id`, `user.tenant_id`) through the entire search pipeline. This user identity is used for three purposes: (1) authorization -- scoping searches to datasets the user has read permission on, (2) query/result logging -- every logged query and result row includes `user_id`, and (3) telemetry -- user and tenant IDs are sent with search telemetry events. The Rust `SearchRequest` currently has no user field. This task adds `user_id: Option<Uuid>` to `SearchRequest` and threads it through the orchestrator so that query logging, result logging, session management, and future authorization checks all receive the caller's identity.

## How Python Uses the `user` Parameter

### 1. Resolution at the API layer

**File:** `cognee/api/v1/search/search.py` (lines 27-46, 206-220)

The top-level `search()` function accepts `user: Optional[User] = None`. When `None`, it resolves to the default user via `get_default_user()`. The resolved user is then set as a session-scoped context variable via `set_session_user_context_variable(user)` so that downstream database engines can read it.

### 2. Dataset authorization

**File:** `cognee/api/v1/search/search.py` (lines 223-227)

When dataset names are provided as strings, they are resolved to dataset objects through `get_authorized_existing_datasets(datasets, "read", user)`, which returns only datasets the user has `read` permission on. If no matching datasets are found, `DatasetNotFoundError` is raised.

**File:** `cognee/modules/search/methods/search.py` (lines 130-176)

The inner `authorized_search()` function takes the `User` object and calls `get_authorized_existing_datasets(dataset_ids, "read", user)` to verify/filter datasets. This ensures the user can only search datasets they are authorized to read.

### 3. Query logging

**File:** `cognee/modules/search/methods/search.py` (line 72)

```python
query = await log_query(query_text, query_type.value, user.id)
```

Every search invocation logs a `Query` record (query text, query type, user ID) to the relational database before executing the search. The returned `query.id` is later used to associate the result.

**File:** `cognee/modules/search/operations/log_query.py`

```python
async def log_query(query_text: str, query_type: str, user_id: UUID) -> Query:
    # Creates Query(text=query_text, query_type=query_type, user_id=user_id)
```

### 4. Result logging

**File:** `cognee/modules/search/methods/search.py` (lines 121-125)

```python
await log_result(query.id, json.dumps(jsonable_encoder(search_results)), user.id)
```

After the search completes, the serialized result is logged with both the `query_id` and `user.id`.

### 5. Telemetry

**File:** `cognee/modules/search/methods/search.py` (lines 73-79, 112-119)

Two telemetry events (`EXECUTION STARTED`, `EXECUTION COMPLETED`) include `user.id` and `user.tenant_id`.

### Summary of Python `user` flow

```
search(user=None)
  -> resolve default user
  -> set_session_user_context_variable(user)
  -> get_authorized_existing_datasets(..., user)
  -> log_query(query_text, query_type, user.id)
  -> authorized_search(user=user, ...)
       -> get_authorized_existing_datasets(..., user)
       -> search_in_datasets_context(...)
  -> log_result(query.id, results, user.id)
  -> send_telemetry(..., user.id)
```

## Current Rust State

### SearchRequest (no user field)

**File:** `crates/search/src/types/search_request.rs`

```rust
pub struct SearchRequest {
    pub query_text: String,
    pub search_type: SearchType,
    pub top_k: Option<usize>,
    pub datasets: Option<Vec<String>>,
    pub dataset_ids: Option<Vec<Uuid>>,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub only_context: Option<bool>,
    pub use_combined_context: Option<bool>,
    pub session_id: Option<String>,
    pub node_type: Option<String>,
    pub node_name: Option<String>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    pub save_interaction: Option<bool>,
    // No user_id field
}
```

### SearchOrchestrator passes `None` for user_id everywhere

**File:** `crates/search/src/orchestration/search_orchestrator.rs`

The orchestrator calls `database.log_query(...)` and `database.log_result(...)` with `None` as the `user_id`:

- Line 63: `database.log_query(&request.query_text, &query_type, None)` -- hardcoded `None`
- Line 183: `database.log_result(query_id, &serialized_response, None)` -- hardcoded `None`

The `SearchHistoryDb` trait already supports `Option<Uuid>` for `user_id` in all three methods (`log_query`, `log_result`, `get_history`), so the infrastructure is ready.

### Session manager also receives `None` for user_id

- Line 123: `sm.load_history_messages(Some(session_id), None)` -- hardcoded `None`
- Line 148: `sm.save_qa(Some(session_id), None, ...)` -- hardcoded `None`

The `SessionManager::save_qa` and `load_history_messages` methods accept `user_id: Option<&str>` as their second parameter, so they are also ready to receive a user identity.

### CLI constructs SearchRequest without user_id

**File:** `crates/cli/src/commands/search.rs` (lines 78-94)

The CLI builds a `SearchRequest` struct literal with all fields. A new `user_id` field will need to be added here.

## Step-by-Step Changes

### Step 1: Add `user_id` field to `SearchRequest`

**File:** `crates/search/src/types/search_request.rs`

Add a new `user_id: Option<Uuid>` field to the `SearchRequest` struct. Place it after `save_interaction` (or at the top alongside other identity fields like `dataset_ids`) for logical grouping with authorization-related fields. Use `#[serde(default)]` so it is backward-compatible for deserialization (existing JSON payloads without `user_id` will deserialize to `None`).

```rust
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query_text: String,
    #[serde(default)]
    pub search_type: SearchType,
    pub top_k: Option<usize>,
    pub datasets: Option<Vec<String>>,
    pub dataset_ids: Option<Vec<Uuid>>,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub only_context: Option<bool>,
    pub use_combined_context: Option<bool>,
    pub session_id: Option<String>,
    pub node_type: Option<String>,
    pub node_name: Option<String>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    pub save_interaction: Option<bool>,
    #[serde(default)]
    pub user_id: Option<Uuid>,  // <-- NEW
}
```

Note: `uuid::Uuid` is already imported in this file.

### Step 2: Thread `user_id` through the SearchOrchestrator

**File:** `crates/search/src/orchestration/search_orchestrator.rs`

#### 2a. Pass `user_id` to `log_query`

Replace the hardcoded `None` in the `log_query` call (line 63) with `request.user_id`:

```rust
// Before:
database.log_query(&request.query_text, &query_type, None).await

// After:
database.log_query(&request.query_text, &query_type, request.user_id).await
```

#### 2b. Pass `user_id` to `log_result`

In the `log_result_if_enabled` method, change its signature to accept `user_id: Option<Uuid>` and pass it through:

```rust
// Before:
async fn log_result_if_enabled(&self, query_id: Option<uuid::Uuid>, response: &SearchResponse) {
    ...
    let _ = database.log_result(query_id, &serialized_response, None).await;
}

// After:
async fn log_result_if_enabled(
    &self,
    query_id: Option<uuid::Uuid>,
    response: &SearchResponse,
    user_id: Option<uuid::Uuid>,
) {
    ...
    let _ = database.log_result(query_id, &serialized_response, user_id).await;
}
```

Update both call sites within `search()` to pass `request.user_id`:

```rust
self.log_result_if_enabled(logged_query_id, &response, request.user_id).await;
```

#### 2c. Pass `user_id` to SessionManager

Convert `request.user_id` to a string for the session manager methods:

```rust
// For load_history_messages:
let user_id_str = request.user_id.map(|id| id.to_string());
let history = sm
    .load_history_messages(Some(session_id), user_id_str.as_deref())
    .await
    .unwrap_or_default();

// For save_qa:
let user_id_str = request.user_id.map(|id| id.to_string());
let _ = sm
    .save_qa(
        Some(session_id),
        user_id_str.as_deref(),
        &request.query_text,
        answer,
        ctx_json.as_deref(),
    )
    .await;
```

#### 2d. Pass `user_id` to `get_history`

The `get_history` method already accepts `user_id: Option<Uuid>`. No signature change needed, but callers (like the CLI or a future HTTP handler) should pass the user ID through when they call `orchestrator.get_history(user_id, limit)`.

### Step 3: Update the CLI

**File:** `crates/cli/src/commands/search.rs`

Add `user_id: None` to the `SearchRequest` struct literal (line 78-94). Optionally add a `--user-id` CLI argument to `SearchArgs` so callers can pass a user identity.

```rust
let request = SearchRequest {
    query_text: args.query_text,
    search_type: mapped_query_type,
    top_k: Some(args.top_k),
    datasets,
    dataset_ids: None,
    system_prompt: None,
    system_prompt_path: Some(system_prompt),
    only_context: Some(false),
    use_combined_context: Some(false),
    session_id: args.session_id,
    node_type: None,
    node_name: None,
    wide_search_top_k: None,
    triplet_distance_penalty: None,
    save_interaction: Some(false),
    user_id: None,  // <-- NEW
};
```

### Step 4: Update all test code that constructs `SearchRequest`

Every test that builds a `SearchRequest` struct literal will fail to compile until `user_id` is added. These are found in:

1. **`crates/search/src/orchestration/search_orchestrator.rs`** -- 7 test functions, each constructing `SearchRequest { ... }`
2. **`crates/search/src/orchestration/search_execution_builder.rs`** -- 2 test functions
3. **`crates/search/tests/integration_search_matrix.rs`** -- all integration test SearchRequest constructions
4. **`crates/cognify/tests/integration_default_backend.rs`** -- SearchRequest constructions in cognify integration tests

Add `user_id: None` to each struct literal in test code (or use `Some(Uuid::new_v4())` for tests that verify user-scoped logging).

### Step 5: Add a targeted test for user_id flowing to query logging

**File:** `crates/search/src/orchestration/search_orchestrator.rs` (in the `#[cfg(test)] mod tests` block)

Add a test that verifies `user_id` is correctly passed through to the database logging:

```rust
#[tokio::test]
async fn passes_user_id_to_query_and_result_logging() {
    let user_id = uuid::Uuid::new_v4();

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(FakeChunksRetriever));

    let db = connect("sqlite::memory:").await.unwrap();
    initialize(&db).await.unwrap();
    let db = Arc::new(db);
    let orchestrator = super::SearchOrchestrator::new(registry)
        .with_database(db.clone() as Arc<dyn SearchHistoryDb>);

    let request = SearchRequest {
        query_text: "test query".to_string(),
        search_type: SearchType::Chunks,
        top_k: Some(3),
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(false),
        use_combined_context: Some(false),
        session_id: None,
        node_type: None,
        node_name: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: Some(true),
        user_id: Some(user_id),
    };

    let _ = orchestrator.search(&request).await.unwrap();

    let history = orchestrator.get_history(Some(user_id), Some(10)).await.unwrap();
    // Both query and result should be logged with the user_id
    assert_eq!(history.len(), 2);
    for entry in &history {
        assert_eq!(entry.user_id, Some(user_id));
    }
}
```

Note: This test assumes `SearchHistoryEntry` exposes `user_id`. Verify the `SearchHistoryEntry` struct has this field; if not, it may need to be checked via a direct DB query.

### Step 6 (Future, out of scope): Dataset authorization scoped by user

Python uses `user` to call `get_authorized_existing_datasets(dataset_ids, "read", user)`. The Rust codebase does not yet have an authorization/ACL system. When that is added, the `user_id` on `SearchRequest` will be the hook point for dataset-level authorization. This task only adds the field and threads it through logging/sessions -- authorization enforcement is deferred.

## Files to Modify

| File | Change |
|---|---|
| `crates/search/src/types/search_request.rs` | Add `user_id: Option<Uuid>` field |
| `crates/search/src/orchestration/search_orchestrator.rs` | Thread `request.user_id` to `log_query`, `log_result`, and session manager; update `log_result_if_enabled` signature |
| `crates/cli/src/commands/search.rs` | Add `user_id: None` to SearchRequest literal |
| `crates/search/src/orchestration/search_orchestrator.rs` (tests) | Add `user_id: None` to all test SearchRequest literals; add new test for user_id logging |
| `crates/search/src/orchestration/search_execution_builder.rs` (tests) | Add `user_id: None` to all test SearchRequest literals |
| `crates/search/tests/integration_search_matrix.rs` | Add `user_id: None` (or `Some(owner_id)`) to SearchRequest literals |
| `crates/cognify/tests/integration_default_backend.rs` | Add `user_id: None` to SearchRequest literals |

## Test Verification

1. **Compilation check:** `cargo check --all-targets` must pass after all changes.
2. **Existing tests pass:** `cargo test` -- all existing tests must still pass with the new `user_id: None` field.
3. **New test:** The `passes_user_id_to_query_and_result_logging` test verifies that:
   - When `user_id: Some(uuid)` is set on `SearchRequest` and `save_interaction: Some(true)`, both the query and result log entries in the database are associated with that user ID.
   - When `user_id: None`, the existing behavior (log with `None`) is preserved.
4. **Serde compatibility:** Verify that deserializing a JSON payload without `user_id` succeeds (defaults to `None`) by confirming existing deserialization tests still pass.
5. **Full suite:** Run `scripts/check_all.sh` to verify formatting, clippy, and all binding checks.

## Dependencies

- **No new crate dependencies.** `uuid::Uuid` is already available in `cognee-search` via the `uuid` dependency.
- **SearchHistoryDb trait already supports `user_id`.** The `log_query`, `log_result`, and `get_history` methods all accept `Option<Uuid>` for user_id -- no trait changes needed.
- **SessionManager already supports `user_id`.** Both `load_history_messages` and `save_qa` accept `Option<&str>` for user_id -- no signature changes needed.
- **No dependency on authorization/ACL system.** This task only adds the identity plumbing. Authorization enforcement (dataset scoping by user permissions) is a separate future task.
- **Blocking:** None. This task is self-contained.
- **Blocked by:** Nothing. The infrastructure (`SearchHistoryDb`, `SessionManager`) already accepts user_id.
