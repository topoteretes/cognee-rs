# Task 36: Add community retriever plugin mechanism

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python has a `registered_community_retrievers` dictionary (`cognee/modules/retrieval/registered_community_retrievers.py`) that allows external plugins to register custom retriever classes by name. The Rust search system uses a `SearchTypeRegistry` that maps `SearchType` enum variants to retriever instances, but there is no mechanism for dynamically registering community/custom retrievers without modifying the `SearchType` enum. This task adds a plugin mechanism for community retrievers.

## Current Rust State

The `SearchTypeRegistry` in `crates/search/src/orchestration/search_type_tools.rs` (or similar) maps `SearchType` enum variants to `SearchRetrieverRef` instances:

```rust
pub struct SearchTypeRegistry {
    retrievers: HashMap<SearchType, SearchRetrieverRef>,
}

impl SearchTypeRegistry {
    pub fn register(&mut self, retriever: SearchRetrieverRef) { ... }
    pub fn get(&self, search_type: SearchType) -> Result<SearchRetrieverRef, SearchError> { ... }
}
```

The `SearchType` enum has 15 fixed variants. Adding a custom retriever requires modifying the enum (a code change).

The `SearchRetriever` trait is already public and well-defined, so custom retrievers can implement it. The gap is in routing.

## Python Reference

In `/tmp/cognee-python/cognee/modules/retrieval/registered_community_retrievers.py`:

```python
registered_community_retrievers = {}
```

This is a module-level dictionary where community plugins can register their retriever classes by name. The search router checks this dictionary for unknown search types before returning an error.

## Step-by-Step Changes

### Step 1: Add string-based routing to `SearchTypeRegistry`

In the registry, add a secondary lookup for string-named retrievers:

```rust
pub struct SearchTypeRegistry {
    typed_retrievers: HashMap<SearchType, SearchRetrieverRef>,
    named_retrievers: HashMap<String, SearchRetrieverRef>,
}

impl SearchTypeRegistry {
    pub fn register(&mut self, retriever: SearchRetrieverRef) {
        self.typed_retrievers.insert(retriever.search_type(), retriever);
    }

    pub fn register_named(&mut self, name: String, retriever: SearchRetrieverRef) {
        self.named_retrievers.insert(name, retriever);
    }

    pub fn get_by_name(&self, name: &str) -> Option<SearchRetrieverRef> {
        self.named_retrievers.get(name).cloned()
    }
}
```

### Step 2: Extend `SearchRequest` with optional string type

In `crates/search/src/types/search_request.rs`, add:

```rust
pub struct SearchRequest {
    // ... existing fields ...
    pub custom_search_type: Option<String>,  // For community retrievers
}
```

### Step 3: Update orchestrator routing

In `crates/search/src/orchestration/search_orchestrator.rs`, modify `search()` to check `custom_search_type` first:

```rust
let retriever = if let Some(ref custom_type) = request.custom_search_type {
    self.registry.get_by_name(custom_type)
        .ok_or_else(|| SearchError::UnsupportedSearchType(/* ... */))?
} else {
    self.registry.get(request.search_type)?
};
```

### Step 4: Add a registration builder

Provide a fluent API for registering community retrievers:

```rust
impl SearchOrchestrator {
    pub fn with_community_retriever(mut self, name: String, retriever: SearchRetrieverRef) -> Self {
        self.registry.register_named(name, retriever);
        self
    }
}
```

### Step 5: Document the plugin pattern

Add a doc comment on `SearchRetriever` trait explaining how to implement a custom retriever and register it with the orchestrator. Include an example in `examples/`.

**Files to modify:**
- `crates/search/src/orchestration/search_type_tools.rs` (or wherever `SearchTypeRegistry` lives)
- `crates/search/src/types/search_request.rs` (optional field)
- `crates/search/src/orchestration/search_orchestrator.rs` (routing logic)
- `crates/search/src/retrievers/base_retriever.rs` (doc comments)

## Test Verification

1. **Unit test:** Register a custom retriever by name, route a request with `custom_search_type`, verify it reaches the custom retriever.
2. **Unit test:** Request with both `search_type` and `custom_search_type` prefers `custom_search_type`.
3. **Unit test:** Unknown custom type returns `UnsupportedSearchType` error.
4. **Existing tests:** All existing registry tests pass unchanged.

## Dependencies

- No new external crate dependencies.
- No blocking dependencies from other tasks.
