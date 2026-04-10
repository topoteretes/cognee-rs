# Task 02: Fix Vector Collections in `brute_force_triplet_search`

## Summary

**Status as of task-01 completion:** Most of this task was implemented as part of task-01. The remaining work is:

1. Add `("EntityType", "name")` to `SEARCH_COLLECTIONS` in `brute_force_triplet_search.rs` — still missing
2. Add `EntityType_name` vector indexing to `crates/cognify/src/tasks.rs` — still missing
3. (Optional cleanup) Remove `entity_description_count()` from `IndexedFieldsStats` in `crates/cognify/src/pipeline.rs`

### What task-01 already implemented (DO NOT redo)

- `("Entity", "description")` removed from `SEARCH_COLLECTIONS` ✓
- `("Triplet", "embeddable_text")` removed from `SEARCH_COLLECTIONS` ✓
- `("EdgeType", "relationship_name")` added to `SEARCH_COLLECTIONS` ✓
- `edge_type_distances` map added, separate from `node_distances` ✓
- Distance semantics (`1.0 - similarity`), `min` merge ✓
- 3-component scoring `source_dist + target_dist + edge_dist` ✓
- `DEFAULT_TRIPLET_DISTANCE_PENALTY = 3.5` ✓

### Python reference collections (verified)

File: `/home/dmytro/dev/cognee/cognee/cognee/modules/retrieval/utils/brute_force_triplet_search.py` lines 188-194:
```python
collections = [
    "Entity_name",
    "TextSummary_text",
    "EntityType_name",      # <-- missing from Rust SEARCH_COLLECTIONS
    "DocumentChunk_text",
]
# EdgeType_relationship_name always appended (already done in task-01)
```

---

## Remaining Changes

### Change 1: Add `("EntityType", "name")` to `SEARCH_COLLECTIONS`

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current (lines 24-29):**
```rust
const SEARCH_COLLECTIONS: [(&str, &str); 4] = [
    ("Entity", "name"),
    ("TextSummary", "text"),
    ("DocumentChunk", "text"),
    ("EdgeType", "relationship_name"),
];
```

**Change to:**
```rust
const SEARCH_COLLECTIONS: [(&str, &str); 5] = [
    ("Entity", "name"),
    ("TextSummary", "text"),
    ("EntityType", "name"),           // matches Python default
    ("DocumentChunk", "text"),
    ("EdgeType", "relationship_name"),
];
```

The existing `if data_type == "EdgeType" && field_name == "relationship_name"` branch at line ~106 already handles routing `EdgeType` to `edge_type_distances` and everything else to `node_distances`. So `("EntityType", "name")` will be automatically treated as a node collection without any further code changes.

### Change 2: Add `EntityType_name` vector indexing in cognify

**File:** `crates/cognify/src/tasks.rs`

The Rust cognify pipeline creates `EdgeType_relationship_name` but does NOT create `EntityType_name`. Read the existing `Entity_name` indexing block (around line 1626-1668) and add an analogous block for `EntityType_name` immediately after it.

The `entity_types` variable is already available in the `add_data_points` function — it is computed for graph storage (confirmed around line 614-620 for `EntityType` node creation). You may need to collect `entity_types` into a deduplicated `Vec<EntityType>` in the same function where vector indexing happens, following the same pattern used for `Entity`.

The indexing block should:
1. Check `vector_db.has_collection("EntityType", "name")` — create if missing
2. Embed `entity_type.name` values in batch (size `config.embedding_batch_size`)
3. Create `VectorPoint`s with metadata: `type=EntityType`, `field=name`, `dataset_id`, `user_id`, `tenant_id`
4. Call `vector_db.index_points("EntityType", "name", &points)`
5. Record stats: `stats.record("EntityType", "name", count)`

Look at the Entity block pattern carefully — it handles batching via `chunks(config.embedding_batch_size)`, flattening, and metadata assignment. Follow the same structure.

### Change 3 (optional cleanup): Update `IndexedFieldsStats`

**File:** `crates/cognify/src/pipeline.rs`

Remove the `entity_description_count()` helper (lines ~95-98) which references a collection that is never created:
```rust
pub fn entity_description_count(&self) -> usize {
    self.get("Entity", "description")
}
```

Add instead:
```rust
pub fn entity_type_name_count(&self) -> usize {
    self.get("EntityType", "name")
}
```

Check all callers of `entity_description_count()` before removing — if it's unused, simply remove it. If callers exist, update them too.

---

## Test Verification

After changes, verify:

1. **Unit test:** The existing `ranks_edges_by_candidate_node_scores` test still passes (it doesn't test `EntityType` specifically, just overall behavior).

2. **Compilation:** `cargo check --all-targets` passes.

3. **All tests:** `cargo test -p cognee-search && cargo test -p cognee-cognify` pass.

4. **check_all:** `scripts/check_all.sh` passes.

---

## Files to Modify

| File | Change |
|---|---|
| `crates/search/src/graph_retrieval/brute_force_triplet_search.rs` | Add `("EntityType", "name")` to `SEARCH_COLLECTIONS` (array size 4→5) |
| `crates/cognify/src/tasks.rs` | Add `EntityType_name` vector indexing block after Entity indexing |
| `crates/cognify/src/pipeline.rs` | Optional: replace `entity_description_count()` with `entity_type_name_count()` |
