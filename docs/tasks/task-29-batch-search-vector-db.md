# Task 29: Add `batch_search` to `VectorDb` trait

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python's `VectorDBInterface` defines `batch_search(collection_name, query_texts, limit, ...)` which performs multiple vector searches in a single call. The Rust `VectorDB` trait only has `search_similar` for a single query vector. This task adds `batch_search_similar` to enable efficient multi-query retrieval.

## Current Rust State

The `VectorDB` trait in `crates/vector/src/vector_db_trait.rs` defines:

```rust
async fn search_similar(
    &self,
    data_type: &str,
    field_name: &str,
    query_vector: &[f32],
    top_k: usize,
) -> VectorDBResult<Vec<SearchResult>>;
```

No batch search method exists. The `SearchResult` type in `crates/vector/src/models.rs` contains `id: Uuid`, `score: f32`, `metadata: HashMap<String, Value>`.

The Qdrant adapter (`crates/vector/src/qdrant_adapter.rs`) and mock (`crates/vector/src/mock_vector_db.rs`) both implement only single-query search.

## Python Reference

In `/tmp/cognee-python/cognee/infrastructure/databases/vector/vector_db_interface.py`:

```python
async def batch_search(
    self,
    collection_name: str,
    query_texts: List[str],
    limit: Optional[int],
    with_vectors: bool = False,
    include_payload: bool = False,
    node_name: Optional[List[str]] = None,
):
```

Python `batch_search` takes raw text queries (embedding happens inside the adapter). In Rust, the embedding step is separate, so the batch method should accept pre-computed vectors.

## Step-by-Step Changes

### Step 1: Add `batch_search_similar` to `VectorDB` trait

In `crates/vector/src/vector_db_trait.rs`, add a default-implemented method:

```rust
async fn batch_search_similar(
    &self,
    data_type: &str,
    field_name: &str,
    query_vectors: &[Vec<f32>],
    top_k: usize,
) -> VectorDBResult<Vec<Vec<SearchResult>>> {
    let mut results = Vec::with_capacity(query_vectors.len());
    for query_vector in query_vectors {
        results.push(self.search_similar(data_type, field_name, query_vector, top_k).await?);
    }
    Ok(results)
}
```

The default loops over `search_similar`. Backends that support native batch search can override.

### Step 2: Implement in Qdrant adapter

In `crates/vector/src/qdrant_adapter.rs`, override `batch_search_similar` to use Qdrant's batch search API if available, or fall back to the default loop.

### Step 3: Implement in MockVectorDB

In `crates/vector/src/mock_vector_db.rs`, add a trivial implementation (the default loop is fine for testing).

### Step 4: Re-export from `crates/vector/src/lib.rs`

Ensure the new method is accessible through the public API. No new types needed since the return type is `Vec<Vec<SearchResult>>`.

## Test Verification

1. **Unit test in `crates/vector/`:** Verify `batch_search_similar` with 0, 1, and N query vectors returns correct shape.
2. **Unit test:** Verify batch results match sequential `search_similar` calls for the same vectors.
3. **Mock test:** Ensure `MockVectorDB` handles batch queries correctly.

## Dependencies

- No new external crate dependencies.
- Used by Task 28 (batch query support in retrievers) for efficient batch retrieval.
