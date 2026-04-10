# Task 01: Fix Scoring Algorithm

## Summary

The Rust `brute_force_triplet_search` scoring algorithm diverges from the Python implementation in three fundamental ways:

1. **Score semantics are inverted.** Rust uses cosine similarity (higher = better) and sorts descending; Python uses cosine distance (lower = better) and sorts ascending via `heapq.nsmallest`.
2. **Edge distance is missing as a third scoring component.** Python scores each triplet as `node1_distance + edge_distance + node2_distance` (three components). Rust only uses `source_score + target_score` (two components, no edge distance).
3. **`triplet_distance_penalty` semantics are wrong.** In Python, it is a **default distance** assigned to graph elements that have no vector match (default `6.5`). In Rust, it is subtracted from Triplet collection scores as a penalty adjustment (default `0.0`), which is a completely different concept.
4. **Multi-collection score merging uses `max` instead of `min`.** When the same node appears in multiple vector collections, Python takes the minimum distance (best match); Rust takes the maximum similarity. With the distance-based semantics, Python's `min` is correct.

## Current Rust Behavior

### File: `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Lines 12-18: `SEARCH_COLLECTIONS` includes wrong collections**
```rust
const SEARCH_COLLECTIONS: [(&str, &str); 5] = [
    ("Entity", "name"),
    ("Entity", "description"),       // <-- does not exist, never created by cognify
    ("TextSummary", "text"),
    ("DocumentChunk", "text"),
    ("Triplet", "embeddable_text"),   // <-- optional, not a default Python collection
];
```
(Note: Collection list is addressed in Task 02. This task focuses on scoring.)

**Lines 60-61: `node_scores` accumulates similarity (higher = better)**
```rust
let mut node_scores = HashMap::<String, f32>::new();
```

**Lines 85-86: Multi-collection merge uses `max` (highest similarity wins)**
```rust
let entry = node_scores.entry(entity_id.clone()).or_insert(result.score);
*entry = entry.max(result.score);
```

**Lines 95-96: `triplet_distance_penalty` is subtracted from Triplet similarity scores**
```rust
let penalty_adjusted_score = result.score - config.triplet_distance_penalty;
```
With default `triplet_distance_penalty = 0.0`, this is effectively a no-op. The Python semantics are completely different: `triplet_distance_penalty` is the **default distance** for nodes/edges not found in vector search (default `3.5`).

**Lines 148-149: Nodes not found in vector search get score 0.0**
```rust
let source_score = node_scores.get(&source_id).copied().unwrap_or(0.0);
let target_score = node_scores.get(&target_id).copied().unwrap_or(0.0);
```
In the Python model, nodes not found in vector search get the `triplet_distance_penalty` (6.5) as their distance, making them rank lower (higher distance = worse).

**Lines 169: Edge score = source + target only (no edge component)**
```rust
score: rank_edge_score(source_score, target_score),
```

**Lines 177-182: Sort descending by score (higher similarity = better)**
```rust
ranked_edges.sort_by(|left, right| {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(std::cmp::Ordering::Equal)
});
```

### File: `crates/search/src/graph_retrieval/triplet_ranking.rs`

**Lines 1-3: Only two components (no edge distance)**
```rust
pub fn rank_edge_score(source_score: f32, target_score: f32) -> f32 {
    source_score + target_score
}
```

### File: `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Lines 21-35: `GraphRetrievalConfig` defaults**
```rust
pub struct GraphRetrievalConfig {
    pub top_k: usize,
    pub wide_search_top_k: usize,
    pub triplet_distance_penalty: f32,
}

impl Default for GraphRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            wide_search_top_k: DEFAULT_WIDE_SEARCH_TOP_K,
            triplet_distance_penalty: 0.0,   // <-- wrong default, Python uses 6.5
        }
    }
}
```

## Required Python Behavior

### File: `/home/dmytro/dev/cognee/cognee/cognee/modules/graph/cognee_graph/CogneeGraph.py`

**Lines 307-371: `_calculate_query_top_triplet_importances` scoring function**

Python scores each edge (triplet) as a sum of **three** distance components:
```python
def score(edge: Edge) -> float:
    elements = (
        (edge.node1, f"node {edge.node1.id}"),       # source node distance
        (edge.node2, f"node {edge.node2.id}"),       # target node distance
        (edge, f"edge {edge.node1.id}->{edge.node2.id}"),  # edge distance
    )

    importances = []
    for element, label in elements:
        distances = element.attributes.get("vector_distance")
        # ... extracts distance[query_index] for each element ...
        importances.append(_effective_distance(distance, feedback_weight))

    return sum(importances)  # SUM of 3 distances
```

**Line 371: Sort ascending (lowest total distance = best match)**
```python
return heapq.nsmallest(k, self.edges, key=score)
```

### File: `/home/dmytro/dev/cognee/cognee/cognee/modules/graph/cognee_graph/CogneeGraphElements.py`

**Lines 57-58: Default vector distance is `triplet_distance_penalty` (3.5)**
```python
def reset_vector_distances(self, query_count: int, default_penalty: float) -> None:
    self.attributes["vector_distance"] = [default_penalty] * query_count
```

This means: any node or edge that is NOT matched by the vector search retains the penalty value (3.5) as its distance. Since the final score is `node1_dist + edge_dist + node2_dist`, unmatched components add 6.5 each to the total, pushing them down in the ranking.

**Lines 67-75: `update_distance_for_query` replaces the distance (not max/min)**
```python
def update_distance_for_query(self, query_index, score, query_count, default_penalty):
    distances = self.ensure_vector_distance_list(query_count, default_penalty)
    distances[query_index] = score
```

### File: `/home/dmytro/dev/cognee/cognee/cognee/infrastructure/databases/vector/models/ScoredResult.py`

**Lines 13-20: Score is cosine distance (lower = better)**
```python
class ScoredResult(BaseModel):
    id: UUID
    score: float  # Lower score is better
```

### File: `/home/dmytro/dev/cognee/cognee/cognee/modules/graph/cognee_graph/CogneeGraph.py`

**Lines 242-275: `map_vector_distances_to_graph_nodes` - maps per-collection results to nodes**

For node collections, each vector result updates the corresponding node's distance. When the same node appears in multiple collections, the **last write wins** (each collection overwrites). However, in practice, different collections reference different IDs (Entity_name has Entity IDs, DocumentChunk_text has chunk IDs, etc.), so collisions are rare. The important point is that there is no `max`/`min` merge; the score is simply assigned.

**Lines 277-305: `map_vector_distances_to_graph_edges` - maps edge collection results**

Edge distances come from the `EdgeType_relationship_name` collection. Multiple edges can share the same `edge_type_id` (same relationship name), so all matching edges get the same distance.

### File: `/home/dmytro/dev/cognee/cognee/cognee/modules/retrieval/utils/brute_force_triplet_search.py`

**Line 143: Default `triplet_distance_penalty = 3.5`** (verified in actual Python source)
```python
async def get_memory_fragment(
    ...
    triplet_distance_penalty: Optional[float] = 3.5,
    ...
```

**Lines 216-225: Default collections**
```python
if collections is None:
    collections = [
        "Entity_name",
        "TextSummary_text",
        "EntityType_name",
        "DocumentChunk_text",
    ]

if "EdgeType_relationship_name" not in collections:
    collections.append("EdgeType_relationship_name")
```

## Step-by-Step Changes

### Step 1: Change `triplet_distance_penalty` default from 0.0 to 3.5

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**What it currently does (lines 27-35):**
```rust
impl Default for GraphRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            wide_search_top_k: DEFAULT_WIDE_SEARCH_TOP_K,
            triplet_distance_penalty: 0.0,
        }
    }
}
```

**What it should do:**
```rust
const DEFAULT_TRIPLET_DISTANCE_PENALTY: f32 = 3.5;

impl Default for GraphRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            wide_search_top_k: DEFAULT_WIDE_SEARCH_TOP_K,
            triplet_distance_penalty: DEFAULT_TRIPLET_DISTANCE_PENALTY,
        }
    }
}
```

Also update the callers that construct this config with `Some(0.0)`:

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs` **line 59:**
```rust
// Change from:
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
// Change to:
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(DEFAULT_TRIPLET_DISTANCE_PENALTY),
```

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs` **line 66:**
```rust
// Change from:
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
// Change to:
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(DEFAULT_TRIPLET_DISTANCE_PENALTY),
```

Import or define `DEFAULT_TRIPLET_DISTANCE_PENALTY` (3.5) in both retriever files, or re-export it from the `graph_retrieval` module.

### Step 2: Rewrite the brute-force search to use distance-based scoring with 3 components

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current algorithm (lines 60-119):** Accumulates per-node *similarity* scores from vector search; uses `max` to merge across collections; subtracts penalty from Triplet scores; assigns 0.0 to unmatched nodes.

**New algorithm:**

The Rust approach is architecturally different from Python's full in-memory graph model. The Python approach loads the entire graph, assigns default penalties to all nodes/edges, then overwrites with vector distances, and finally scores all edges. The Rust approach only loads candidate nodes from vector search, then filters graph edges. We can preserve the Rust architectural approach but fix the scoring semantics.

Key changes:
1. Convert similarity scores to cosine distance: `distance = 1.0 - similarity`
2. Track per-node *distances* (lower = better), using `min` to merge across collections
3. Assign `triplet_distance_penalty` as default distance for unmatched nodes
4. Track per-edge-type distances from `EdgeType_relationship_name` collection
5. Score each triplet as `node1_dist + edge_dist + node2_dist`
6. Sort ascending (lowest distance = best)

```rust
pub async fn brute_force_triplet_search(
    query: &str,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    graph_db: &dyn GraphDBTrait,
    config: &GraphRetrievalConfig,
) -> Result<Vec<RankedGraphEdge>, SearchError> {
    let query_vectors = embedding_engine.embed(&[query]).await?;
    let query_vector = query_vectors.into_iter().next().ok_or_else(|| {
        SearchError::InvalidInput("embedding engine returned no vectors".to_string())
    })?;

    // node_id -> cosine distance (lower = better)
    let mut node_distances = HashMap::<String, f32>::new();
    let mut candidate_node_ids = HashSet::<String>::new();
    let mut node_dataset_ids = HashMap::<String, String>::new();

    // edge_type_id -> cosine distance (lower = better)
    let mut edge_type_distances = HashMap::<String, f32>::new();

    for (data_type, field_name) in SEARCH_COLLECTIONS {
        if !vector_db.has_collection(data_type, field_name).await? {
            debug!("vector collection {data_type}/{field_name} does not exist -- skipping");
            continue;
        }

        let results = vector_db
            .search_similar(data_type, field_name, &query_vector, config.wide_search_top_k)
            .await?;

        for result in results {
            // Convert Qdrant cosine similarity to cosine distance
            let distance = 1.0 - result.score;

            if data_type == "EdgeType" && field_name == "relationship_name" {
                // Edge distances keyed by relationship_name (from vector point metadata).
                // edge_type_id is NOT stored in graph edge properties, so we must key
                // by relationship_name to match graph edges at scoring time.
                if let Some(rel_name) = result
                    .metadata
                    .get("relationship_name")
                    .and_then(|v| v.as_str())
                {
                    let entry = edge_type_distances
                        .entry(rel_name.to_string())
                        .or_insert(distance);
                    if distance < *entry {
                        *entry = distance;
                    }
                }
            } else {
                // Node distances keyed by point ID
                let node_id = result.id.to_string();
                candidate_node_ids.insert(node_id.clone());
                let entry = node_distances
                    .entry(node_id.clone())
                    .or_insert(distance);
                if distance < *entry {
                    *entry = distance;
                }
                if let Some(dataset_id) =
                    result.metadata.get("dataset_id").and_then(|v| v.as_str())
                {
                    node_dataset_ids
                        .entry(node_id)
                        .or_insert_with(|| dataset_id.to_string());
                }
            }
        }
    }

    if candidate_node_ids.is_empty() {
        debug!("no candidate nodes found from vector search -- returning empty");
        return Ok(vec![]);
    }

    let (graph_nodes, graph_edges) = graph_db.get_graph_data().await?;

    let node_names: HashMap<String, String> = graph_nodes
        .into_iter()
        .map(|(node_id, properties)| {
            let name = properties
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or(node_id.as_str())
                .to_string();
            (node_id, name)
        })
        .collect();

    let default_penalty = config.triplet_distance_penalty;

    let mut ranked_edges = graph_edges
        .into_iter()
        .filter_map(|(source_id, target_id, relationship_name, properties)| {
            if !candidate_node_ids.contains(&source_id)
                && !candidate_node_ids.contains(&target_id)
            {
                return None;
            }

            let source_dist = node_distances
                .get(&source_id)
                .copied()
                .unwrap_or(default_penalty);
            let target_dist = node_distances
                .get(&target_id)
                .copied()
                .unwrap_or(default_penalty);

            // Look up edge distance by relationship_name.
            // NOTE: edge_type_id is NOT stored in graph edge properties by cognify,
            // so we key by relationship_name instead. EdgeType vector points include
            // relationship_name in their metadata (confirmed in tasks.rs line 1802).
            let edge_dist = edge_type_distances
                .get(&relationship_name)
                .copied()
                .unwrap_or(default_penalty);

            let source_name = node_names
                .get(&source_id)
                .cloned()
                .unwrap_or(source_id.clone());
            let target_name = node_names
                .get(&target_id)
                .cloned()
                .unwrap_or(target_id.clone());

            let dataset_id = node_dataset_ids
                .get(&source_id)
                .or_else(|| node_dataset_ids.get(&target_id))
                .cloned();

            Some(RankedGraphEdge {
                source_id,
                target_id,
                relationship_name,
                score: rank_edge_score(source_dist, target_dist, edge_dist),
                source_name,
                target_name,
                dataset_id,
            })
        })
        .collect::<Vec<_>>();

    // Sort ascending: lowest total distance = best match
    ranked_edges.sort_by(|left, right| {
        left.score
            .partial_cmp(&right.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(ranked_edges.into_iter().take(config.top_k).collect())
}
```

**Important:** The `edge_type_id` is NOT stored in graph edge properties by cognify (confirmed by reading `crates/cognify/src/tasks.rs`). Instead, edge distances are keyed by **`relationship_name`** string, which is always present on every graph edge. The `EdgeType_relationship_name` vector points store `relationship_name` in their metadata (confirmed at `tasks.rs` line 1802), so the lookup is:
1. Search `EdgeType_relationship_name` vector collection → get back results with `metadata["relationship_name"]`
2. Build `HashMap<String, f32>` keyed by that relationship_name string
3. When scoring each graph edge, look up `edge_type_distances.get(&relationship_name)`

No changes to cognify are needed.

### Step 3: Update `rank_edge_score` to accept three components

**File:** `crates/search/src/graph_retrieval/triplet_ranking.rs`

**Current code (lines 1-3):**
```rust
pub fn rank_edge_score(source_score: f32, target_score: f32) -> f32 {
    source_score + target_score
}
```

**New code:**
```rust
/// Computes the total distance for a triplet (source_node, edge, target_node).
///
/// Each component is a cosine distance (lower = better). The total distance is
/// the sum of all three, matching Python's `_calculate_query_top_triplet_importances`.
pub fn rank_edge_score(source_distance: f32, target_distance: f32, edge_distance: f32) -> f32 {
    source_distance + target_distance + edge_distance
}
```

### Step 4: Update `RankedGraphEdge.score` documentation

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current struct (lines 37-47):**
```rust
pub struct RankedGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relationship_name: String,
    pub score: f32,
    pub source_name: String,
    pub target_name: String,
    pub dataset_id: Option<String>,
}
```

**New struct:**
```rust
pub struct RankedGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relationship_name: String,
    /// Total triplet distance (lower = better match).
    /// Sum of source_node_distance + edge_distance + target_node_distance.
    pub score: f32,
    pub source_name: String,
    pub target_name: String,
    /// Dataset ID of the source or target entity, for context scoping.
    pub dataset_id: Option<String>,
}
```

### Step 5: Remove the old Triplet-specific scoring path

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current code (lines 93-115):** The `"Triplet"` match arm applies `triplet_distance_penalty` as a *subtraction* from the similarity score and injects source/target scores based on Triplet metadata.

**Remove this entirely.** The `Triplet` collection is not part of the default Python collection set for `brute_force_triplet_search`. It is handled separately by `TripletRetriever`/`TripletCompletionRetriever`. If `Triplet_embeddable_text` is removed from `SEARCH_COLLECTIONS` (Task 02), this code path becomes unreachable.

If we want to keep the Triplet collection as an optional node-level collection (for backward compat), then treat it like any other node collection: convert similarity to distance, update `node_distances` with `min`. But do NOT extract `source_id`/`target_id` from metadata -- the Python code does not do this in `brute_force_triplet_search`.

### Step 6: Update all callers that pass `triplet_distance_penalty`

Search for all places that construct `GraphRetrievalConfig` or pass `triplet_distance_penalty`:

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs` **line 59:**
Change default from `0.0` to `3.5` (see Step 1).

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs` **line 66:**
Change default from `0.0` to `3.5` (see Step 1).

### Step 7: Update existing tests

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs` -- tests starting at line 469

The test `ranks_edges_by_candidate_node_scores` creates entity hits with similarity scores `0.95`, `0.80`, `0.40`. After the fix:
- Distances: `0.05`, `0.20`, `0.60`
- Edge `Alice->Bob`: `0.05 + 0.20 + 3.5 = 3.75` (no edge distance match, uses penalty `3.5`)
- Edge `Bob->Charlie`: `0.20 + 0.60 + 3.5 = 4.30`
- Sort ascending: `Alice->Bob` first, `Bob->Charlie` second

The test currently asserts:
```rust
assert_eq!(context[0].payload["relationship"], "KNOWS");
assert_eq!(context[1].payload["relationship"], "WORKS_WITH");
```

This assertion order would remain the same (Alice->Bob still ranks first), but the scores themselves change. The test needs to be updated to pass `triplet_distance_penalty: Some(3.5)` and verify the new scoring.

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs` -- tests starting at line 491

Similarly, update the test helper `build_vector_db()` and retriever constructors. The tests create hits with scores `0.9` and `0.8`. After conversion to distances: `0.1` and `0.2`. With edge penalty `3.5`, the single edge `KNOWS` gets distance `0.1 + 0.2 + 3.5 = 3.8`.

## Test Verification

### Unit tests to update

1. **`crates/search/src/retrievers/graph_completion_retriever.rs::ranks_edges_by_candidate_node_scores`** -- Update assertions for distance-based scoring and ascending sort order.

2. **`crates/search/src/retrievers/advanced_graph_retrievers.rs::graph_summary_completion_uses_two_generation_steps`** -- Verify the retriever still works end-to-end with distance scoring.

### New unit tests to write

3. **`crates/search/src/graph_retrieval/triplet_ranking.rs`** -- Test `rank_edge_score` with 3 arguments.

4. **`crates/search/src/graph_retrieval/brute_force_triplet_search.rs`** -- Add test that verifies:
   - Similarity-to-distance conversion: score `0.9` becomes distance `0.1`
   - Unmatched nodes get `triplet_distance_penalty` as distance
   - Edge distance from `EdgeType_relationship_name` collection is included
   - Sorting is ascending (lowest distance first)
   - `triplet_distance_penalty` defaults to `3.5`

5. **Integration test** -- Run the full E2E search matrix test (`crates/search/tests/` if one exists) to verify the scoring pipeline end-to-end.

### How to run

```bash
cargo test -p cognee-search
cargo test -p cognee-search -- brute_force
cargo test -p cognee-search -- graph_completion
cargo test -p cognee-search -- triplet_ranking
```

## Dependencies

- **Task 02 (Fix Vector Collections)** should be done in parallel or before this task, since it changes `SEARCH_COLLECTIONS` to include `EdgeType_relationship_name` and `EntityType_name`. The scoring algorithm in this task relies on `EdgeType_relationship_name` being present in the search collections.
- **No cognify changes needed.** Edge distances are keyed by `relationship_name` (always present on graph edges). The `EdgeType_relationship_name` vector points store `relationship_name` in their metadata. `edge_type_id` is NOT stored in graph edge properties, so the earlier plan to key by `edge_type_id` would not work.
