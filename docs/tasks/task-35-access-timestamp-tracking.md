# Task 35: Add access timestamp tracking (`update_node_access_timestamps`)

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python's search pipeline calls `update_node_access_timestamps(retrieved_objects)` after every retrieval to track when data was last accessed. This updates a `last_accessed` timestamp on the origin `Data` records in the relational database by traversing the graph from retrieved nodes back to their source documents. The Rust search pipeline does not track access timestamps. This task ports the feature.

## Current Rust State

The `SearchOrchestrator` in `crates/search/src/orchestration/search_orchestrator.rs` calls retrievers and returns results but does not update any access timestamps.

The `Data` model in `crates/models/` and the database schema in `crates/database/` do not have a `last_accessed` field (the Python schema does).

The graph DB has `get_connections` and `get_edges` which can be used to traverse from entity nodes to `DocumentChunk` to `Document`/`Data`.

## Python Reference

In `/tmp/cognee-python/cognee/modules/retrieval/utils/access_tracking.py`:

```python
async def update_node_access_timestamps(items: List[Edge]):
    if os.getenv("ENABLE_LAST_ACCESSED", "false").lower() != "true":
        return
    # Extract node IDs from items
    # Use graph projection to find origin Documents connected to those nodes
    # Update Data.last_accessed in SQL via SQLAlchemy
```

Called from `/tmp/cognee-python/cognee/modules/search/methods/get_retriever_output.py`:

```python
if retrieved_objects:
    await update_node_access_timestamps(retrieved_objects)
```

Key behavior:
1. Feature is opt-in via `ENABLE_LAST_ACCESSED` env var.
2. Extracts node IDs from retrieved graph edges.
3. Traverses graph to find `DocumentChunk` -> `Document`/`TextDocument` connections.
4. Updates `last_accessed` column on the `Data` SQL table.

## Step-by-Step Changes

### Step 1: Add `last_accessed` column to Data model

In `crates/database/`, add a `last_accessed: Option<chrono::DateTime<Utc>>` column to the Data schema. Create a migration.

**Files to modify:**
- `crates/database/src/migrations/` (new migration)
- `crates/database/src/models/` (add field to Data entity)

### Step 2: Add `update_last_accessed` to `DatabaseTrait`

In `crates/database/src/lib.rs` (or trait file), add:

```rust
async fn update_last_accessed(
    &self,
    data_ids: &[Uuid],
    timestamp: DateTime<Utc>,
) -> DatabaseResult<()>;
```

Implement in `SqliteDatabase` with a single `UPDATE data SET last_accessed = ? WHERE id IN (...)` query.

### Step 3: Create `update_node_access_timestamps` utility

Create `crates/search/src/utils/access_tracking.rs`:

```rust
pub async fn update_node_access_timestamps(
    graph_db: &dyn GraphDBTrait,
    database: &dyn DatabaseTrait,
    retrieved_items: &SearchContext,
) -> Result<(), SearchError> {
    // Extract node IDs from SearchItems
    // Traverse graph to find connected Document nodes
    // Collect document/data IDs
    // Call database.update_last_accessed(data_ids, Utc::now())
}
```

### Step 4: Integrate into `SearchOrchestrator`

In `crates/search/src/orchestration/search_orchestrator.rs`, after the retriever returns context:

```rust
if self.enable_access_tracking {
    if let Some(ref context) = base_context {
        update_node_access_timestamps(
            self.graph_db.as_ref(),
            self.database.as_ref(),
            context,
        ).await.ok(); // Log but don't fail the search
    }
}
```

### Step 5: Add configuration

Add `enable_access_tracking: bool` field to `SearchOrchestrator` (default `false`), configurable via builder method or environment variable.

**Files to modify/create:**
- `crates/database/src/migrations/` (new migration)
- `crates/database/src/models/` (Data field)
- `crates/database/src/lib.rs` (trait method)
- `crates/search/src/utils/access_tracking.rs` (new file)
- `crates/search/src/utils/mod.rs` (register module)
- `crates/search/src/orchestration/search_orchestrator.rs` (integration)

## Test Verification

1. **Unit test:** `update_node_access_timestamps` with empty context is a no-op.
2. **Unit test:** With mock graph and database, verify `last_accessed` is updated for the correct Data IDs.
3. **Unit test:** Errors in access tracking do not propagate to the search caller.
4. **Migration test:** New column is nullable and existing data is unaffected.

## Dependencies

- `chrono` (already a workspace dependency).
- Database migration infrastructure (already in place).
- No blocking dependencies from other tasks.
