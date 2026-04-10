# Task 24: Make Query Logging Unconditional by Default

## Summary

In Python, every call to `search()` unconditionally logs the query and result to the relational database. In Rust, query logging only happens when the caller explicitly sets `save_interaction: Some(true)` in the `SearchRequest`. The default is `false`, meaning most searches are never logged. This task changes the Rust default so that query logging happens unconditionally when a database is configured, matching the Python behavior. The `save_interaction` field is repurposed as an opt-out mechanism rather than an opt-in one.

## Current Rust Behavior

**File:** `crates/search/src/orchestration/search_orchestrator.rs` (lines 56-67)

```rust
let should_save_interaction = request.save_interaction.unwrap_or(false);
// ...
if should_save_interaction
    && let Some(database) = &self.database
    && let Ok(query_id) = database
        .log_query(&request.query_text, &query_type, None)
        .await
{
    logged_query_id = Some(query_id);
}
```

- `save_interaction` defaults to `false` when `None`.
- Logging is gated behind `should_save_interaction` being `true`.
- Most callers never set `save_interaction`, so logging silently does not happen.

**File:** `crates/search/src/types/search_request.rs` (line 23)

```rust
pub save_interaction: Option<bool>,
```

No default annotation -- deserialization leaves it as `None`, which resolves to `false`.

## Required Python Behavior

**File:** `/tmp/cognee-python/cognee/modules/search/methods/search.py` (lines 72, 121-125)

```python
async def search(query_text, query_type, ...):
    query = await log_query(query_text, query_type.value, user.id)
    # ... perform search ...
    await log_result(query.id, json.dumps(jsonable_encoder(search_results)), user.id)
    return ...
```

- `log_query` is called unconditionally at the start of every search.
- `log_result` is called unconditionally after every search completes.
- There is no `save_interaction` flag in the Python API.

## Step-by-Step Changes

### Step 1: Change the default of `save_interaction` to `true`

In `crates/search/src/orchestration/search_orchestrator.rs`, change line 56:

```rust
// Before
let should_save_interaction = request.save_interaction.unwrap_or(false);

// After
let should_save_interaction = request.save_interaction.unwrap_or(true);
```

This single change makes logging the default behavior when a database is configured. Callers can still pass `save_interaction: Some(false)` to suppress logging.

### Step 2: Update the `SearchRequest` doc comment (if any)

In `crates/search/src/types/search_request.rs`, add a doc comment to clarify the new default:

```rust
/// Whether to persist this query and its result to the search history database.
/// Defaults to `true` when omitted, matching the Python SDK behavior where every
/// search is logged unconditionally.
pub save_interaction: Option<bool>,
```

### Step 3: Update test expectations

In `crates/search/src/orchestration/search_orchestrator.rs`, the tests that set `save_interaction: None` will now log by default. Update the following tests to explicitly set `save_interaction: Some(false)` where they do not expect logging:

- `routes_to_registered_retriever_for_completion` (line 232) -- no database is configured, so logging is a no-op regardless. No change needed.
- `routes_to_registered_retriever_for_context` (line 269) -- same, no database. No change needed.
- `routes_to_registered_retriever_for_default_context_label` (line 309) -- same. No change needed.
- `includes_graph_when_context_is_fetched` (line 341) -- same. No change needed.
- `fans_out_context_by_dataset_when_dataset_scope_enabled` (line 411) -- same. No change needed.
- `merges_scoped_context_when_combined_context_enabled` (line 491) -- same. No change needed.
- `persists_query_and_result_when_save_interaction_enabled` (line 574) -- this test uses `save_interaction: Some(true)`, which still works. No change needed.

Similarly, update tests in `crates/search/src/orchestration/search_execution_builder.rs` where `save_interaction: None` is used. These tests do not use a real database (they use `TestDatabase`), so the logging calls will succeed but not affect assertions. No changes needed unless a test explicitly asserts that logging did NOT happen.

### Step 4: Update integration test

In `crates/search/tests/integration_search_matrix.rs`, line 328 has `save_interaction: Some(false)` already, which will continue to work. The test at line 270 uses `save_interaction: Some(true)` via the `save` parameter. No changes needed.

### Step 5 (optional): Add a new test for default logging

Add a test that exercises the new default: create an orchestrator with a database, send a request with `save_interaction: None`, and verify that both query and result are persisted:

```rust
#[tokio::test]
async fn logs_query_and_result_by_default_when_save_interaction_is_none() {
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(FakeChunksRetriever));

    let db = connect("sqlite::memory:").await.unwrap();
    initialize(&db).await.unwrap();
    let db = Arc::new(db);
    let orchestrator = super::SearchOrchestrator::new(registry)
        .with_database(db.clone() as Arc<dyn SearchHistoryDb>);

    let request = SearchRequest {
        query_text: "default logging test".to_string(),
        search_type: SearchType::Chunks,
        save_interaction: None, // defaults to true
        // ... other fields ...
    };

    let _ = orchestrator.search(&request).await.unwrap();

    let history = orchestrator.get_history(None, Some(10)).await.unwrap();
    assert_eq!(history.len(), 2); // one Query + one Result
}
```

## Test Verification

1. **Existing test `persists_query_and_result_when_save_interaction_enabled`**: Continues to pass as-is (it uses `Some(true)`).

2. **New test `logs_query_and_result_by_default_when_save_interaction_is_none`**: Validates the new default behavior.

3. **All other orchestrator tests**: Continue to pass because they either lack a database (logging is a no-op) or explicitly set the flag.

4. Run `cargo check --all-targets` and `scripts/check_all.sh`.

## Dependencies

- No new crate dependencies.
- No migration changes.
- This is a one-line default change plus documentation and test updates.
