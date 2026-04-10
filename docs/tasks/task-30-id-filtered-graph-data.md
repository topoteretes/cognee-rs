# Task 30: Add `get_id_filtered_graph_data` to `GraphDBTrait`

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python's `GraphDBInterface` provides `get_filtered_graph_data(attribute_filters)` which filters nodes by arbitrary attribute key-value pairs. While Rust already has `get_filtered_graph_data`, Python also provides specialized methods like `get_nodeset_subgraph` for type+name filtering. This task adds a convenience method `get_id_filtered_graph_data` that retrieves a subgraph containing only the nodes with specific IDs and the edges connecting them. This is used by several retrievers to narrow graph traversal to relevant node sets obtained from vector search.

## Current Rust State

In `crates/graph/src/traits.rs`, `GraphDBTrait` has:

```rust
async fn get_filtered_graph_data(
    &self,
    attribute_filters: &HashMap<Cow<'static, str>, Vec<serde_json::Value>>,
) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)>;

async fn get_nodeset_subgraph(
    &self,
    node_type: &str,
    node_names: &[String],
) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)>;
```

There is no method to retrieve a subgraph by a set of node IDs. Currently, retrievers that need this pattern call `get_node` or `get_nodes` for nodes and then separately `get_edges` per node, which is N+1 queries.

The `MockGraphDB` in `crates/graph/src/mock.rs` delegates `get_filtered_graph_data` to `get_graph_data` (returns everything).

## Python Reference

In `/tmp/cognee-python/cognee/infrastructure/databases/graph/graph_db_interface.py`:

```python
async def get_filtered_graph_data(
    self, attribute_filters: List[Dict[str, List[Union[str, int]]]]
) -> Tuple[List[Node], List[EdgeData]]:
```

Python uses `attribute_filters` with `{"id": [list_of_ids]}` to achieve ID-based filtering. The Kuzu and Neo4j adapters translate these filters into native queries.

## Step-by-Step Changes

### Step 1: Add `get_id_filtered_graph_data` to `GraphDBTrait`

In `crates/graph/src/traits.rs`, add a default-implemented method:

```rust
async fn get_id_filtered_graph_data(
    &self,
    node_ids: &[String],
) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
    // Default: filter from full graph data
    let (all_nodes, all_edges) = self.get_graph_data().await?;
    let id_set: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
    let nodes: Vec<GraphNode> = all_nodes
        .into_iter()
        .filter(|(id, _)| id_set.contains(id.as_str()))
        .collect();
    let edges: Vec<EdgeData> = all_edges
        .into_iter()
        .filter(|(src, tgt, _, _)| id_set.contains(src.as_str()) && id_set.contains(tgt.as_str()))
        .collect();
    Ok((nodes, edges))
}
```

This default scans the full graph. Backends can override with native filtering.

### Step 2: Override in Ladybug adapter

In `crates/graph/src/ladybug.rs`, implement an efficient version that queries only the requested node IDs and their interconnecting edges, avoiding a full graph scan.

### Step 3: Implement in MockGraphDB

In `crates/graph/src/mock.rs`, add an implementation that filters from the in-memory store:

```rust
async fn get_id_filtered_graph_data(
    &self,
    node_ids: &[String],
) -> GraphDBResult<(Vec<GraphNode>, Vec<EdgeData>)> {
    let id_set: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
    let nodes = self.nodes.lock().unwrap(); // lock poison is unrecoverable
    let edges = self.edges.lock().unwrap(); // lock poison is unrecoverable
    // filter nodes and edges by id_set
}
```

### Step 4: Use in retrievers

Update graph-based retrievers to use `get_id_filtered_graph_data` where they currently do multiple `get_node`/`get_edges` calls. Primary candidate: `brute_force_triplet_search` in `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`.

**Files to modify:**
- `crates/graph/src/traits.rs`
- `crates/graph/src/mock.rs`
- `crates/graph/src/ladybug.rs`
- `crates/search/src/graph_retrieval/brute_force_triplet_search.rs` (optional optimization)

## Test Verification

1. **Unit test in `crates/graph/`:** Verify `get_id_filtered_graph_data` returns only requested nodes and their interconnecting edges.
2. **Unit test:** Empty ID list returns empty result.
3. **Unit test:** IDs not in graph are silently ignored.
4. **Integration test:** Verify retriever produces same search results before and after switching to `get_id_filtered_graph_data`.

## Dependencies

- No new external crate dependencies.
- No blocking dependencies from other tasks.
