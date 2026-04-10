# Task 34: Add dynamic vector collection discovery from data point metadata

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python's `index_data_points` function dynamically discovers which vector collections to create based on each data point's `metadata["index_fields"]` list. The type name and field name together form the collection key (e.g., `DocumentChunk:text`, `Entity:name`). In Rust, the collection names are hardcoded constants in each retriever (`CHUNKS_DATA_TYPE = "DocumentChunk"`, `CHUNKS_FIELD_NAME = "text"`, etc.). This task adds a mechanism to discover available vector collections from stored metadata, enabling retrievers to adapt to custom data point types without code changes.

## Current Rust State

Collections are referenced by hardcoded constants:

- `crates/search/src/retrievers/chunks_retriever.rs`: `CHUNKS_DATA_TYPE = "DocumentChunk"`, `CHUNKS_FIELD_NAME = "text"`
- `crates/search/src/retrievers/summaries_retriever.rs`: `"TextSummary"`, `"text"`
- `crates/search/src/retrievers/triplet_retriever.rs`: `TRIPLET_DATA_TYPE = "Triplet"`, `TRIPLET_PRIMARY_FIELD = "text"`
- `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`: `"Entity"`, `"name"`

The `VectorDB` trait's `create_collection` takes `(data_type, field_name, dimension)`. There is no method to list existing collections.

The cognify pipeline (`crates/cognify/`) creates collections for 5 hardcoded types during `add_data_points`.

## Python Reference

In `/tmp/cognee-python/cognee/tasks/storage/index_data_points.py`:

```python
for data_point in data_points:
    type_name = type(data_point).__name__
    for field_name in data_point.metadata["index_fields"]:
        if type_name not in data_points_by_type:
            data_points_by_type[type_name] = {}
        if field_name not in data_points_by_type[type_name]:
            await vector_engine.create_vector_index(type_name, field_name)
            data_points_by_type[type_name][field_name] = []
```

The Python approach is fully dynamic -- any `DataPoint` subclass with `index_fields` metadata gets indexed.

## Step-by-Step Changes

### Step 1: Add `list_collections` to `VectorDB` trait

In `crates/vector/src/vector_db_trait.rs`, add:

```rust
async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
    Ok(vec![])  // Default: no discovery
}
```

Returns `Vec<(data_type, field_name)>` pairs for all known collections.

### Step 2: Implement in Qdrant adapter

In `crates/vector/src/qdrant_adapter.rs`, parse collection names (which use `{data_type}_{field_name}` format) to return the list.

### Step 3: Implement in MockVectorDB

Track created collections in the mock and return them from `list_collections`.

### Step 4: Add `CollectionRegistry` utility

Create `crates/search/src/utils/collection_registry.rs`:

```rust
pub struct CollectionRegistry {
    vector_db: Arc<dyn VectorDB>,
    known_collections: OnceCell<Vec<(String, String)>>,
}

impl CollectionRegistry {
    pub async fn has_collection(&self, data_type: &str, field_name: &str) -> bool { ... }
    pub async fn collections_for_type(&self, data_type: &str) -> Vec<String> { ... }
}
```

### Step 5: Use in retrievers for graceful fallback

Update retrievers like `TripletRetriever` to check if the collection exists before searching, falling back gracefully if the collection was not created during cognify:

```rust
if !self.vector_db.has_collection(TRIPLET_DATA_TYPE, TRIPLET_PRIMARY_FIELD).await? {
    // Try fallback field or return empty context
}
```

**Files to modify/create:**
- `crates/vector/src/vector_db_trait.rs` (new method)
- `crates/vector/src/qdrant_adapter.rs` (implementation)
- `crates/vector/src/mock_vector_db.rs` (implementation)
- `crates/search/src/utils/collection_registry.rs` (new file)
- `crates/search/src/retrievers/triplet_retriever.rs` (graceful fallback)

## Test Verification

1. **Unit test:** `list_collections` returns empty for fresh database, populated after `create_collection`.
2. **Unit test:** `CollectionRegistry` caches the list and serves subsequent queries without re-querying.
3. **Unit test:** Retriever with missing collection returns empty context instead of error.

## Dependencies

- `tokio::sync::OnceCell` for caching (already a workspace dependency via `tokio`).
- No blocking dependencies from other tasks.
