# Task 16: Add `feedback_influence` Parameter to Search Pipeline

## Summary

Add a `feedback_influence` parameter (f32, range [0.0, 1.0], default 0.0) to `SearchRequest`, `GraphRetrievalConfig`, and the edge scoring function. When non-zero, this parameter blends per-element `feedback_weight` values stored on graph nodes/edges into the triplet ranking score, allowing user feedback to bias search results.

## Current State

### Python State

Python passes `feedback_influence` from `search()` all the way through:

1. **`search.py`** -- top-level API parameter: `feedback_influence: float = 0.0`
2. **`search_function()`** -- forwards it to dataset-level search
3. **`get_search_type_retriever_instance()`** -- passes it to `GraphCompletionRetriever`, `GraphCompletionCotRetriever`, `GraphCompletionContextExtensionRetriever`, `GraphSummaryCompletionRetriever`, and `TemporalRetriever`
4. **`GraphCompletionRetriever.__init__()`** -- stores `self.feedback_influence`
5. **`brute_force_triplet_search()`** -- validates `0.0 <= feedback_influence <= 1.0`, passes to `get_memory_fragment()` and `calculate_top_triplet_importances()`
6. **`CogneeGraph.project_graph_from_db()`** -- when `feedback_influence > 0.0`, projects `feedback_weight` property on both nodes and edges
7. **`CogneeGraph._calculate_query_top_triplet_importances()`** -- applies the blending formula via `_effective_distance()`

#### Python Feedback Blending Formula

Located in `CogneeGraph._calculate_query_top_triplet_importances()`:

```python
def _effective_distance(distance: float, feedback_weight: Any) -> float:
    if active_feedback_influence <= 0.0:
        return distance

    # Only blend real cosine distances in [0, 2].
    # Fallback penalties and out-of-range values must remain unchanged so
    # missing components stay ranked below valid matches.
    if distance >= self.triplet_distance_penalty or distance < 0.0 or distance > 2.0:
        return distance

    try:
        normalized_feedback_weight = float(feedback_weight)
    except (TypeError, ValueError):
        normalized_feedback_weight = 0.5

    normalized_feedback_weight = max(0.0, min(1.0, normalized_feedback_weight))
    # Blend in a normalized space (cosine distance in [0, 2] -> [0, 1]),
    # then project back to distance scale so score magnitudes stay consistent.
    normalized_distance = distance / 2.0
    blended_normalized = (1.0 - active_feedback_influence) * normalized_distance + (
        active_feedback_influence * (1.0 - normalized_feedback_weight)
    )
    return blended_normalized * 2.0
```

The triplet score for a single query index is the **sum** of `_effective_distance(distance, feedback_weight)` across three elements: `edge.node1`, `edge.node2`, and `edge` itself. The top-k edges are those with the **smallest** total score (via `heapq.nsmallest`).

### Rust State

- **`SearchRequest`** (`crates/search/src/types/search_request.rs`): No `feedback_influence` field.
- **`GraphRetrievalConfig`** (`crates/search/src/graph_retrieval/brute_force_triplet_search.rs`): Has `top_k`, `wide_search_top_k`, `triplet_distance_penalty` -- no `feedback_influence`.
- **`rank_edge_score()`** (`crates/search/src/graph_retrieval/triplet_ranking.rs`): Currently `source_score + target_score` with no feedback blending.
- **`brute_force_triplet_search()`**: Uses higher-is-better cosine similarity (not cosine distance). Sorts descending by score. No feedback logic.
- **Graph nodes/edges**: No `feedback_weight` property is read or used.

**Note on scoring convention:** Python uses cosine **distance** (lower is better, range [0, 2]), while Rust uses cosine **similarity** (higher is better, range [-1, 1]). The formula must be adapted accordingly. Cosine distance = 1 - cosine similarity when similarity is in [0, 1], but the Qdrant scores may be in [-1, 1]. The adaptation needs to account for this.

## Step-by-Step Changes

### Step 1: Add `feedback_influence` to `SearchRequest`

**File:** `crates/search/src/types/search_request.rs`

Add the field after `triplet_distance_penalty`:

```rust
// Before:
pub triplet_distance_penalty: Option<f32>,
pub save_interaction: Option<bool>,

// After:
pub triplet_distance_penalty: Option<f32>,
pub feedback_influence: Option<f32>,
pub save_interaction: Option<bool>,
```

Add a helper method:

```rust
pub fn feedback_influence_or_default(&self) -> f32 {
    self.feedback_influence.unwrap_or(0.0)
}
```

### Step 2: Add `feedback_influence` to `GraphRetrievalConfig`

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

```rust
// Before:
pub struct GraphRetrievalConfig {
    pub top_k: usize,
    pub wide_search_top_k: usize,
    pub triplet_distance_penalty: f32,
}

// After:
pub struct GraphRetrievalConfig {
    pub top_k: usize,
    pub wide_search_top_k: usize,
    pub triplet_distance_penalty: f32,
    pub feedback_influence: f32,
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
        }
    }
}
```

### Step 3: Update `rank_edge_score` to accept `feedback_influence`

**File:** `crates/search/src/graph_retrieval/triplet_ranking.rs`

Replace the current simple function with a feedback-aware version:

```rust
/// Compute effective score for a single element, blending feedback weight
/// when `feedback_influence > 0.0`.
///
/// Rust uses cosine similarity (higher is better), unlike Python which uses
/// cosine distance (lower is better). The blending formula is adapted:
///
///   normalized_sim = (score + 1.0) / 2.0        -- map [-1,1] to [0,1]
///   blended = (1 - fi) * normalized_sim + fi * feedback_weight
///   result  = blended * 2.0 - 1.0               -- map back to [-1,1]
///
/// Elements with no valid similarity score (e.g. score == 0.0 when the node
/// was not found in vector search) are left unchanged.
fn effective_score(score: f32, feedback_weight: f32, feedback_influence: f32) -> f32 {
    if feedback_influence <= 0.0 {
        return score;
    }

    // Clamp feedback_weight to [0, 1]
    let fw = feedback_weight.clamp(0.0, 1.0);

    // Normalize similarity from [-1, 1] to [0, 1]
    let normalized_sim = (score + 1.0) / 2.0;

    // Blend: higher feedback_weight -> higher effective score
    let blended = (1.0 - feedback_influence) * normalized_sim + feedback_influence * fw;

    // Map back to [-1, 1]
    blended * 2.0 - 1.0
}

/// Rank an edge by combining source and target node scores with optional feedback blending.
pub fn rank_edge_score(
    source_score: f32,
    target_score: f32,
    feedback_influence: f32,
    source_feedback_weight: f32,
    target_feedback_weight: f32,
) -> f32 {
    let s = effective_score(source_score, source_feedback_weight, feedback_influence);
    let t = effective_score(target_score, target_feedback_weight, feedback_influence);
    s + t
}
```

### Step 4: Read `feedback_weight` from graph node properties in `brute_force_triplet_search`

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

After building `node_names`, also extract `feedback_weight` per node:

```rust
let mut node_feedback_weights: HashMap<String, f32> = HashMap::new();

let node_names: HashMap<String, String> = graph_nodes
    .into_iter()
    .map(|(node_id, properties)| {
        let name = properties
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or(node_id.as_str())
            .to_string();

        if config.feedback_influence > 0.0 {
            let fw = properties
                .get("feedback_weight")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.5) as f32;
            node_feedback_weights.insert(node_id.clone(), fw);
        }

        (node_id, name)
    })
    .collect();
```

Then update the edge ranking call:

```rust
let source_fw = node_feedback_weights.get(&source_id).copied().unwrap_or(0.5);
let target_fw = node_feedback_weights.get(&target_id).copied().unwrap_or(0.5);

Some(RankedGraphEdge {
    source_id,
    target_id,
    relationship_name,
    score: rank_edge_score(
        source_score,
        target_score,
        config.feedback_influence,
        source_fw,
        target_fw,
    ),
    source_name,
    target_name,
    dataset_id,
})
```

### Step 5: Thread `feedback_influence` through all retriever constructors

Update every place that creates a `GraphRetrievalConfig` to include the new field.

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`

Add `feedback_influence: f32` to the struct and constructor, then include it in config creation:

```rust
let config = GraphRetrievalConfig {
    top_k: self.top_k,
    wide_search_top_k: self.wide_search_top_k,
    triplet_distance_penalty: self.triplet_distance_penalty,
    feedback_influence: self.feedback_influence,
};
```

Apply the same pattern to:
- `crates/search/src/retrievers/advanced_graph_retrievers.rs` -- `GraphRetrieverCore`, `GraphSummaryCompletionRetriever`, `GraphCompletionContextExtensionRetriever`, `GraphCompletionCotRetriever`
- `crates/search/src/retrievers/temporal_retriever.rs` -- `TemporalRetriever`

### Step 6: Thread through builder and orchestrator

**File:** `crates/search/src/orchestration/search_execution_builder.rs`

The standard retrievers are created with `None` defaults. When `feedback_influence` is added to each retriever's constructor, pass `None` (or `Some(0.0)`) in the builder. The parameter can be overridden per-request if the architecture supports it, or the builder can accept a default config.

**File:** `crates/search/src/orchestration/search_orchestrator.rs`

All test `SearchRequest` literals must include the new field:

```rust
feedback_influence: None,
```

### Step 7: Validate the input range

In `brute_force_triplet_search()`, add a validation at the top (matching Python):

```rust
if config.feedback_influence < 0.0 || config.feedback_influence > 1.0 {
    return Err(SearchError::InvalidInput(
        "feedback_influence must be in range [0.0, 1.0]".to_string(),
    ));
}
```

### Step 8: Update all `SearchRequest` construction sites

Every place that constructs a `SearchRequest` must include `feedback_influence`. This includes:

- `crates/search/src/orchestration/search_orchestrator.rs` -- all test `SearchRequest` literals (approximately 8 instances)
- `crates/search/src/orchestration/search_execution_builder.rs` -- all test `SearchRequest` literals (approximately 3 instances)
- `crates/search/tests/integration_search_matrix.rs` -- the `make_request()` helper and any inline construction
- `crates/cli/` -- if the CLI constructs `SearchRequest` directly

## Test Verification

1. **Unit test for `effective_score`**: Verify that with `feedback_influence = 0.0`, the output equals the input score. With `feedback_influence = 1.0`, the output is purely based on `feedback_weight`. With `feedback_influence = 0.5`, verify the blended result.

2. **Unit test for `rank_edge_score`**: Verify the updated signature produces correct sums with feedback blending.

3. **Existing tests**: All existing tests should continue passing with `feedback_influence: 0.0` (the default), which produces identical behavior to the old `rank_edge_score(source_score, target_score)`.

4. **Integration**: The `make_request()` helper in `integration_search_matrix.rs` must include `feedback_influence: None`.

## Dependencies

- No external crate dependencies.
- This task is independent of Tasks 17 and 18 and can be done in any order.
- The `feedback_weight` property on graph nodes must be stored during cognify for the feature to have an effect. When absent, the default of 0.5 is used (matching Python behavior).
