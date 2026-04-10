# Task 17: Add `node_type` / `node_name` / `node_name_filter_operator` Filtering to Graph Retrieval

## Summary

Add `node_type`, `node_name`, and `node_name_filter_operator` parameters to the graph retrieval pipeline so that search can filter results to a specific subset of the knowledge graph. In Python, when both `node_type` and `node_name` are provided, the graph projection calls `get_nodeset_subgraph()` instead of `get_graph_data()`, returning only the matching nodes and their neighbors. The `node_name_filter_operator` controls whether neighbors must be connected to ALL named nodes ("AND") or ANY of them ("OR", the default).

## Current State

### Python State

In Python, `node_type` and `node_name` flow from `search()` through the retriever to `brute_force_triplet_search()` and into `CogneeGraph.project_graph_from_db()`.

**Key decision logic** (in `CogneeGraph.project_graph_from_db()`):

```python
if node_type is not None and node_name not in [None, [], ""]:
    nodes_data, edges_data = await self._get_nodeset_subgraph(
        adapter, node_type, node_name, node_name_filter_operator
    )
elif len(memory_fragment_filter) == 0:
    nodes_data, edges_data = await self._get_full_or_id_filtered_graph(
        adapter, relevant_ids_to_filter
    )
else:
    nodes_data, edges_data = await self._get_filtered_graph(
        adapter, memory_fragment_filter
    )
```

When `node_type` and `node_name` are both provided:
1. The graph adapter's `get_nodeset_subgraph()` is called with `(node_type, node_name, node_name_filter_operator)`.
2. This queries for nodes matching the type and any of the names (OR mode) or all of the names (AND mode).
3. It returns those nodes PLUS their neighbors and all edges between them.

**The `node_name` filtering also affects vector search.** In `NodeEdgeVectorSearch._search_single_collection()`, `node_name` and `node_name_filter_operator` are passed to `vector_engine.search()`, which applies metadata filtering at the vector DB level to constrain which vector results are returned.

**Additionally**, in `brute_force_triplet_search()`, when `node_name` is provided, `wide_search_limit` is set to `None` (full graph projection instead of ID-filtered):

```python
wide_search_limit = (
    None if query_list_length else (wide_search_top_k if node_name is None else None)
)
```

**Parameters in Python:**
- `node_type: Optional[Type]` -- a Python class (e.g., `NodeSet`); its `__name__` is used as the graph label
- `node_name: Optional[List[str]]` -- list of node name strings to filter by
- `node_name_filter_operator: str` -- "OR" (default) or "AND"

**Search types that use these parameters:** `GraphCompletion`, `GraphCompletionCot`, `GraphCompletionContextExtension`, `GraphSummaryCompletion`, `Temporal`

### Rust State

- **`SearchRequest`** has `node_type: Option<String>` and `node_name: Option<String>` (singular string, not a list -- see Task 18).
- These fields exist but are **never used** in any retriever or in `brute_force_triplet_search()`.
- There is no `node_name_filter_operator` field anywhere in the Rust codebase.
- `GraphDBTrait::get_nodeset_subgraph()` already exists with signature `(node_type: &str, node_names: &[String])` but does not accept a `node_name_filter_operator` parameter.
- The Ladybug adapter implements `get_nodeset_subgraph()` and has tests for it.
- `brute_force_triplet_search()` always calls `graph_db.get_graph_data()` -- there is no conditional path for node filtering.

## Step-by-Step Changes

### Step 1: Add `node_name_filter_operator` to `SearchRequest`

**File:** `crates/search/src/types/search_request.rs`

```rust
// Before:
pub node_type: Option<String>,
pub node_name: Option<String>,

// After:
pub node_type: Option<String>,
pub node_name: Option<Vec<String>>,            // Task 18 dependency
pub node_name_filter_operator: Option<String>,
```

Add a helper:

```rust
pub fn node_name_filter_operator_or_default(&self) -> &str {
    self.node_name_filter_operator
        .as_deref()
        .unwrap_or("OR")
}
```

> **Note:** Task 18 changes `node_name` from `Option<String>` to `Option<Vec<String>>`. If doing Task 17 before Task 18, use `Option<String>` temporarily and convert in the retriever. If doing Task 18 first (recommended), use `Option<Vec<String>>` directly.

### Step 2: Add `node_name_filter_operator` to `GraphDBTrait::get_nodeset_subgraph`

**File:** `crates/graph/src/traits.rs`

```rust
// Before:
async fn get_nodeset_subgraph(
    &self,
    node_type: &str,
    node_names: &[String],
) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)>;

// After:
async fn get_nodeset_subgraph(
    &self,
    node_type: &str,
    node_names: &[String],
    node_name_filter_operator: &str,
) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)>;
```

Update all implementations:
- `crates/graph/src/ladybug.rs` -- add the parameter, implement AND/OR logic
- `crates/graph/src/mock.rs` -- add the parameter
- All test implementations across the search crate

**Ladybug AND vs OR logic:**
- **OR (default):** Find nodes matching any name in the list, then include their neighbors and edges between all of them. This is the current behavior.
- **AND:** Find nodes matching each name, then only include neighbors that are connected to ALL named nodes (intersection of neighbor sets).

### Step 3: Add node filtering parameters to `GraphRetrievalConfig`

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

```rust
pub struct GraphRetrievalConfig {
    pub top_k: usize,
    pub wide_search_top_k: usize,
    pub triplet_distance_penalty: f32,
    pub feedback_influence: f32,              // from Task 16
    pub node_type: Option<String>,
    pub node_name: Option<Vec<String>>,
    pub node_name_filter_operator: String,
}
```

Update the `Default` impl:

```rust
impl Default for GraphRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            wide_search_top_k: DEFAULT_WIDE_SEARCH_TOP_K,
            triplet_distance_penalty: 0.0,
            feedback_influence: 0.0,
            node_type: None,
            node_name: None,
            node_name_filter_operator: "OR".to_string(),
        }
    }
}
```

### Step 4: Add conditional graph retrieval in `brute_force_triplet_search`

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

Replace the unconditional `get_graph_data()` call with Python-compatible logic:

```rust
// Before:
let (graph_nodes, graph_edges) = graph_db.get_graph_data().await?;

// After:
let has_node_filter = config.node_type.is_some()
    && config.node_name.as_ref().is_some_and(|names| !names.is_empty());

let (graph_nodes, graph_edges) = if has_node_filter {
    let node_type = config.node_type.as_deref()
        .expect("node_type checked above in has_node_filter");
    let node_names = config.node_name.as_deref()
        .expect("node_name checked above in has_node_filter");
    graph_db
        .get_nodeset_subgraph(
            node_type,
            node_names,
            &config.node_name_filter_operator,
        )
        .await?
} else {
    graph_db.get_graph_data().await?
};
```

### Step 5: Thread parameters through retrievers

Each retriever that uses graph retrieval must accept and forward the new parameters.

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`

Add fields to the struct:

```rust
pub struct GraphCompletionRetriever {
    // ... existing fields ...
    node_type: Option<String>,
    node_name: Option<Vec<String>>,
    node_name_filter_operator: String,
}
```

Update the constructor to accept `Option<String>` for node_type, `Option<Vec<String>>` for node_name, and `Option<String>` for node_name_filter_operator (defaulting to "OR").

Update config creation:

```rust
let config = GraphRetrievalConfig {
    top_k: self.top_k,
    wide_search_top_k: self.wide_search_top_k,
    triplet_distance_penalty: self.triplet_distance_penalty,
    feedback_influence: self.feedback_influence,
    node_type: self.node_type.clone(),
    node_name: self.node_name.clone(),
    node_name_filter_operator: self.node_name_filter_operator.clone(),
};
```

Apply the same pattern to:
- `GraphRetrieverCore` in `advanced_graph_retrievers.rs`
- `TemporalRetriever` in `temporal_retriever.rs`

### Step 6: Validate `node_name_filter_operator`

In `brute_force_triplet_search()`, validate the operator early:

```rust
let op = config.node_name_filter_operator.to_uppercase();
if op != "AND" && op != "OR" {
    return Err(SearchError::InvalidInput(format!(
        "Invalid node_name_filter_operator: {:?}. Must be AND or OR.",
        config.node_name_filter_operator
    )));
}
```

### Step 7: Update all `SearchRequest` construction sites

Add `node_name_filter_operator: None` to every `SearchRequest` literal in:
- `crates/search/src/orchestration/search_orchestrator.rs` -- all test instances
- `crates/search/src/orchestration/search_execution_builder.rs` -- all test instances
- `crates/search/tests/integration_search_matrix.rs` -- the `make_request()` helper
- `crates/cli/` -- CLI request construction

### Step 8: Update the `SearchBuilder` to forward per-request parameters

Currently, `SearchBuilder` creates retrievers once with `None` defaults. For per-request `node_type`/`node_name` filtering to work, one of two approaches is needed:

**Option A (simpler):** Have the orchestrator extract `node_type`/`node_name`/`node_name_filter_operator` from `SearchRequest` and pass them when calling `get_context()`. This requires changing the `SearchRetriever` trait's `get_context` signature.

**Option B (recommended, matching current architecture):** Create retrievers per-request with the request's parameters. The `SearchBuilder` can provide a factory method instead of pre-built retrievers for types that need per-request configuration.

The simplest interim approach: make `get_context` accept an optional config, or have the orchestrator inject these values into the retriever before calling `get_context`. This is an architectural decision that should match the existing Rust pattern.

## How Node Filtering Works in Python's Graph Projection

1. **OR mode** (`node_name_filter_operator = "OR"`, default):
   - Find all nodes in the graph whose `type` matches `node_type.__name__` and whose `name` is in the `node_name` list.
   - Find ALL neighbors of those matched nodes (one hop).
   - Return the union of matched nodes + their neighbors, plus all edges between them.
   - **Effect:** Each named node contributes its local subgraph. The result is the union of all their neighborhoods.

2. **AND mode** (`node_name_filter_operator = "AND"`):
   - Find all nodes matching the type and any name (same initial step).
   - Find neighbors, but only keep neighbors that are connected to ALL of the named nodes (intersection).
   - Return the matched nodes + filtered neighbors + edges between them.
   - **Effect:** Only entities that are related to every named node survive. This is a more restrictive filter.

3. **When `node_name` is set, `wide_search_limit` is forced to `None`:**
   - This means the full graph is projected (no ID-based filtering from vector search).
   - The rationale is that node_name filtering replaces the role of wide_search_top_k filtering.

4. **Vector search also respects `node_name`:**
   - `node_name` and `node_name_filter_operator` are passed to the vector engine's `search()` method.
   - The vector DB applies metadata filtering so only vectors associated with the named nodes are returned.
   - In Rust, this would require extending `VectorDB::search_similar()` to accept optional metadata filters. This can be deferred to a follow-up task if needed.

## Test Verification

1. **Unit test for node filtering in `brute_force_triplet_search`:**
   - Set up a graph with typed nodes (e.g., "Person" type with names "Alice", "Bob", "Charlie").
   - Search with `node_type = Some("Person")` and `node_name = Some(vec!["Alice"])`.
   - Verify only edges involving Alice and her neighbors are returned.

2. **Unit test for AND vs OR:**
   - Set up a graph where Alice and Bob each have neighbors, and Charlie is connected to both.
   - OR mode: returns Alice's neighbors + Bob's neighbors.
   - AND mode: returns only Charlie (connected to both).

3. **Existing tests must pass** with `node_type: None`, `node_name: None`, `node_name_filter_operator: None` (the defaults), which produce the same behavior as current code (full graph retrieval).

4. **`get_nodeset_subgraph` tests** in `crates/graph/src/ladybug.rs` should be updated to test the new `node_name_filter_operator` parameter.

## Dependencies

- **Task 18** (change `node_name` type) should ideally be done first or simultaneously, since this task introduces `node_name` into `GraphRetrievalConfig` and it should be `Option<Vec<String>>` from the start.
- **Task 16** is independent but both tasks modify `GraphRetrievalConfig`, so they should be coordinated.
- Extending `VectorDB::search_similar()` for metadata filtering is a separate follow-up concern.
