# Task 28: Add batch query support (`query_batch`) to graph retrievers

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python's `BaseRetriever` accepts both `query: Optional[str]` and `query_batch: Optional[List[str]]` across all three pipeline methods (`get_retrieved_objects`, `get_context_from_objects`, `get_completion_from_context`). The Rust `SearchRetriever` trait currently only accepts a single `query: &str`. This task adds batch query support so that multiple queries can be processed in a single retriever call, matching the Python interface.

## Current Rust State

The Rust `SearchRetriever` trait in `crates/search/src/retrievers/base_retriever.rs` defines:

```rust
#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;
    async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError>;
    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
    ) -> Result<SearchOutput, SearchError>;
}
```

No batch query parameter exists. Each retriever implementation (`GraphCompletionRetriever`, `ChunksRetriever`, `TripletRetriever`, etc.) only handles a single query string.

## Python Reference

In `/tmp/cognee-python/cognee/modules/retrieval/base_retriever.py`:

- `get_retrieved_objects(query: Optional[str], query_batch: Optional[str])` -- note: type annotation says `str` but semantically it is `List[str]`
- `get_context_from_objects(query, query_batch, retrieved_objects)`
- `get_completion_from_context(query, query_batch, retrieved_objects, context)`

The batch parameter allows callers to send multiple queries that the retriever may process more efficiently (e.g., a single batch vector search instead of N individual searches).

## Step-by-Step Changes

### Step 1: Add `QueryInput` enum to `crates/search/src/types/`

Create a new type that represents either a single query or a batch:

```rust
pub enum QueryInput<'a> {
    Single(&'a str),
    Batch(&'a [String]),
}
```

This avoids breaking the existing API by keeping the trait method signature accepting `QueryInput` or by adding optional batch methods.

**File:** `crates/search/src/types/mod.rs` -- add the enum and re-export it.

### Step 2: Extend `SearchRetriever` trait with default batch methods

Add default-implemented batch methods to `SearchRetriever` in `crates/search/src/retrievers/base_retriever.rs`:

```rust
async fn get_context_batch(&self, queries: &[String]) -> Result<Vec<SearchContext>, SearchError> {
    let mut results = Vec::with_capacity(queries.len());
    for query in queries {
        results.push(self.get_context(query).await?);
    }
    Ok(results)
}

async fn get_completion_batch(
    &self,
    queries: &[String],
    contexts: Option<Vec<SearchContext>>,
    session: &SessionContext,
) -> Result<Vec<SearchOutput>, SearchError> {
    // Default: sequential delegation to single-query method
}
```

The default implementation loops sequentially. Retrievers that benefit from batching (e.g., vector-search-based) can override for efficiency.

### Step 3: Add batch support to `SearchOrchestrator`

In `crates/search/src/orchestration/search_orchestrator.rs`, add a `search_batch` method that accepts `Vec<SearchRequest>` and delegates to `get_context_batch` / `get_completion_batch`.

### Step 4: Override in vector-based retrievers (optional optimization)

For `ChunksRetriever`, `TripletRetriever`, and `GraphCompletionRetriever`, override `get_context_batch` to call `VectorDB::batch_search` (Task 29) instead of looping.

**Files to modify:**
- `crates/search/src/retrievers/chunks_retriever.rs`
- `crates/search/src/retrievers/triplet_retriever.rs`
- `crates/search/src/retrievers/graph_completion_retriever.rs`

## Test Verification

1. **Unit test:** Verify default `get_context_batch` returns the same results as calling `get_context` in a loop.
2. **Unit test:** Verify `search_batch` on `SearchOrchestrator` returns one `SearchResponse` per query.
3. **Unit test:** Empty batch returns empty results, single-element batch matches single-query result.

## Dependencies

- **Task 29** (batch_search on VectorDB) -- needed for efficient batch override in vector-based retrievers, but the default sequential implementation works without it.
- No external crate dependencies.
