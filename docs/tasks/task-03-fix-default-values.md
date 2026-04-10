# Task 3: Fix Default Values for Python Compatibility

## Summary

Align default values for `top_k`, `wide_search_top_k`, `triplet_distance_penalty`, iteration counts, and context join separators across all Rust retrievers with the Python reference implementation. Currently, the Rust defaults diverge in multiple places, which leads to different search behavior even when no explicit overrides are provided.

## Discrepancy Table

| Parameter | Python Default | Rust Default | Affected Rust Files |
|---|---|---|---|
| `top_k` (ChunksRetriever) | 5 | 10 | `chunks_retriever.rs:14` |
| `top_k` (SummariesRetriever) | 5 | 10 | `summaries_retriever.rs:14` |
| `top_k` (CompletionRetriever) | 1 | 10 | `completion_retriever.rs:18` |
| `top_k` (TripletRetriever) | 5 | 10 | `triplet_retriever.rs:19` |
| `top_k` (GraphCompletionRetriever) | 5 | 10 | `graph_completion_retriever.rs:20` |
| `top_k` (TemporalRetriever) | 5 | 10 | `temporal_retriever.rs:21` |
| `top_k` (advanced graph retrievers) | 5 | 10 | `advanced_graph_retrievers.rs:20` |
| `wide_search_top_k` (GraphCompletionRetriever) | 100 | 20 | `graph_completion_retriever.rs:21` |
| `wide_search_top_k` (TemporalRetriever) | 100 | 20 | `temporal_retriever.rs:22` |
| `wide_search_top_k` (advanced graph retrievers) | 100 | 20 | `advanced_graph_retrievers.rs:21` |
| `wide_search_top_k` (brute_force_triplet_search) | 100 | 20 | `brute_force_triplet_search.rs:11` |
| `triplet_distance_penalty` (GraphCompletionRetriever) | 6.5 | 0.0 | `graph_completion_retriever.rs:59` |
| `triplet_distance_penalty` (TemporalRetriever) | 6.5 | 0.0 | `temporal_retriever.rs:106` |
| `triplet_distance_penalty` (advanced graph retrievers) | 6.5 | 0.0 | `advanced_graph_retrievers.rs:66` |
| `triplet_distance_penalty` (brute_force_triplet_search default) | 6.5 | 0.0 | `brute_force_triplet_search.rs:32` |
| `max_iter` (GraphCompletionCotRetriever) | 4 | 2 | `advanced_graph_retrievers.rs:23` |
| `context_extension_rounds` (ContextExtension) | 4 | 2 | `advanced_graph_retrievers.rs:22` |
| Context join separator (CompletionRetriever) | `"\n"` | `"\n\n"` | `completion_retriever.rs:106` |
| Context join separator (TripletRetriever) | `"\n"` | `"\n\n"` | `triplet_retriever.rs:92` |

---

## Current Rust Behavior

### 1. `top_k` defaults (all retrievers)

All Rust retrievers use `const DEFAULT_TOP_K: usize = 10`.

**File:** `crates/search/src/retrievers/chunks_retriever.rs`, line 14
```rust
const DEFAULT_TOP_K: usize = 10;
```

**File:** `crates/search/src/retrievers/summaries_retriever.rs`, line 14
```rust
const DEFAULT_TOP_K: usize = 10;
```

**File:** `crates/search/src/retrievers/completion_retriever.rs`, line 18
```rust
const DEFAULT_TOP_K: usize = 10;
```

**File:** `crates/search/src/retrievers/triplet_retriever.rs`, line 19
```rust
const DEFAULT_TOP_K: usize = 10;
```

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`, line 20
```rust
const DEFAULT_TOP_K: usize = 10;
```

**File:** `crates/search/src/retrievers/temporal_retriever.rs`, line 21
```rust
const DEFAULT_TOP_K: usize = 10;
```

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`, line 20
```rust
const DEFAULT_TOP_K: usize = 10;
```

### 2. `wide_search_top_k` defaults

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`, line 21
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
```

**File:** `crates/search/src/retrievers/temporal_retriever.rs`, line 22
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
```

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`, line 21
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
```

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`, line 11
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
```

And in `GraphRetrievalConfig::default()` at line 30:
```rust
wide_search_top_k: DEFAULT_WIDE_SEARCH_TOP_K,
```

### 3. `triplet_distance_penalty` defaults

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`, line 59
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
```

**File:** `crates/search/src/retrievers/temporal_retriever.rs`, line 106
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
```

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`, line 66
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
```

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`, lines 32-33
```rust
triplet_distance_penalty: 0.0,
```

### 4. Iteration count defaults

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`, line 22
```rust
const DEFAULT_CONTEXT_EXTENSION_ROUNDS: usize = 2;
```

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`, line 23
```rust
const DEFAULT_COT_MAX_ITER: usize = 2;
```

### 5. Context join separators

**File:** `crates/search/src/retrievers/completion_retriever.rs`, lines 102-106
```rust
let context_text = completion_context
    .iter()
    .filter_map(|item| item.payload.get("text").and_then(|value| value.as_str()))
    .collect::<Vec<_>>()
    .join("\n\n");
```

**File:** `crates/search/src/retrievers/triplet_retriever.rs`, lines 79-93
```rust
.join("\n\n")
```

---

## Required Python Behavior

### 1. `top_k` defaults

**File:** `/tmp/cognee-python/cognee/modules/retrieval/chunks_retriever.py`, line 26
```python
def __init__(self, top_k: Optional[int] = 5):
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/summaries_retriever.py`, line 25
```python
def __init__(self, top_k: int = 5, session_id: Optional[str] = None):
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/completion_retriever.py`, lines 27-34
```python
def __init__(self, ..., top_k: Optional[int] = 1, ...):
    self.top_k = top_k if top_k is not None else 1
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/triplet_retriever.py`, lines 30-37
```python
def __init__(self, ..., top_k: Optional[int] = 5, ...):
    self.top_k = top_k if top_k is not None else 5
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_retriever.py`, lines 43-56
```python
def __init__(self, ..., top_k: Optional[int] = 5, ...):
    self.top_k = top_k if top_k is not None else 5
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/temporal_retriever.py`, lines 41-66
```python
def __init__(self, ..., top_k: Optional[int] = 5, ...):
    self.top_k = top_k if top_k is not None else 5
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_cot_retriever.py`, line 63
```python
def __init__(self, ..., top_k: Optional[int] = 5, ...):
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_context_extension_retriever.py`, line 28
```python
def __init__(self, ..., top_k: Optional[int] = 5, ...):
```

### 2. `wide_search_top_k` default = 100

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_retriever.py`, line 47
```python
wide_search_top_k: Optional[int] = 100,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_cot_retriever.py`, line 66
```python
wide_search_top_k: Optional[int] = 100,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_context_extension_retriever.py`, line 30
```python
wide_search_top_k: Optional[int] = 100,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/temporal_retriever.py`, line 44
```python
wide_search_top_k: Optional[int] = 100,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/utils/brute_force_triplet_search.py`, line 164
```python
wide_search_top_k: Optional[int] = 100,
```

### 3. `triplet_distance_penalty` default = 6.5

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_retriever.py`, line 48
```python
triplet_distance_penalty: Optional[float] = 6.5,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_cot_retriever.py`, line 67
```python
triplet_distance_penalty: Optional[float] = 6.5,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_context_extension_retriever.py`, line 31
```python
triplet_distance_penalty: Optional[float] = 6.5,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/temporal_retriever.py`, line 45
```python
triplet_distance_penalty: Optional[float] = 6.5,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/utils/brute_force_triplet_search.py`, line 55
```python
triplet_distance_penalty: Optional[float] = 6.5,
```

### 4. Iteration counts

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_cot_retriever.py`, line 69
```python
max_iter: int = 4,
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/graph_completion_context_extension_retriever.py`, line 33
```python
context_extension_rounds: int = 4,
```

### 5. Context join separators = `"\n"`

**File:** `/tmp/cognee-python/cognee/modules/retrieval/chunks_retriever.py`, line 74
```python
return "\n".join(chunk_payload_texts)
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/summaries_retriever.py`, line 86
```python
return "\n".join(summary_payload_texts)
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/completion_retriever.py`, line 80
```python
combined_context = "\n".join(chunks_payload)
```

**File:** `/tmp/cognee-python/cognee/modules/retrieval/triplet_retriever.py`, line 90
```python
combined_context = "\n".join(triplets_payload)
```

---

## Step-by-Step Changes

### Step 1: Fix `top_k` in `chunks_retriever.rs`

**File:** `crates/search/src/retrievers/chunks_retriever.rs`

**Current (line 14):**
```rust
const DEFAULT_TOP_K: usize = 10;
```

**Target:**
```rust
const DEFAULT_TOP_K: usize = 5;
```

### Step 2: Fix `top_k` in `summaries_retriever.rs`

**File:** `crates/search/src/retrievers/summaries_retriever.rs`

**Current (line 14):**
```rust
const DEFAULT_TOP_K: usize = 10;
```

**Target:**
```rust
const DEFAULT_TOP_K: usize = 5;
```

### Step 3: Fix `top_k` in `completion_retriever.rs`

**File:** `crates/search/src/retrievers/completion_retriever.rs`

**Current (line 18):**
```rust
const DEFAULT_TOP_K: usize = 10;
```

**Target:**
```rust
const DEFAULT_TOP_K: usize = 1;
```

### Step 4: Fix `top_k` in `triplet_retriever.rs`

**File:** `crates/search/src/retrievers/triplet_retriever.rs`

**Current (line 19):**
```rust
const DEFAULT_TOP_K: usize = 10;
```

**Target:**
```rust
const DEFAULT_TOP_K: usize = 5;
```

### Step 5: Fix `top_k` in `graph_completion_retriever.rs`

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`

**Current (line 20):**
```rust
const DEFAULT_TOP_K: usize = 10;
```

**Target:**
```rust
const DEFAULT_TOP_K: usize = 5;
```

### Step 6: Fix `top_k` in `temporal_retriever.rs`

**File:** `crates/search/src/retrievers/temporal_retriever.rs`

**Current (line 21):**
```rust
const DEFAULT_TOP_K: usize = 10;
```

**Target:**
```rust
const DEFAULT_TOP_K: usize = 5;
```

### Step 7: Fix `top_k` in `advanced_graph_retrievers.rs`

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

**Current (line 20):**
```rust
const DEFAULT_TOP_K: usize = 10;
```

**Target:**
```rust
const DEFAULT_TOP_K: usize = 5;
```

### Step 8: Fix `wide_search_top_k` in `graph_completion_retriever.rs`

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`

**Current (line 21):**
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
```

**Target:**
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;
```

### Step 9: Fix `wide_search_top_k` in `temporal_retriever.rs`

**File:** `crates/search/src/retrievers/temporal_retriever.rs`

**Current (line 22):**
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
```

**Target:**
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;
```

### Step 10: Fix `wide_search_top_k` in `advanced_graph_retrievers.rs`

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

**Current (line 21):**
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
```

**Target:**
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;
```

### Step 11: Fix `wide_search_top_k` in `brute_force_triplet_search.rs`

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current (line 11):**
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
```

**Target:**
```rust
const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;
```

Also fix the `GraphRetrievalConfig::default()` at line 30:
```rust
// This will automatically pick up the new DEFAULT_WIDE_SEARCH_TOP_K constant.
// No code change needed here -- it already references the const.
```

### Step 12: Fix `triplet_distance_penalty` in `graph_completion_retriever.rs`

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`

**Current (line 59):**
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
```

**Target:**
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(6.5),
```

### Step 13: Fix `triplet_distance_penalty` in `temporal_retriever.rs`

**File:** `crates/search/src/retrievers/temporal_retriever.rs`

**Current (line 106):**
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
```

**Target:**
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(6.5),
```

### Step 14: Fix `triplet_distance_penalty` in `advanced_graph_retrievers.rs`

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

In `GraphRetrieverCore::new()` at line 66:

**Current:**
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(0.0),
```

**Target:**
```rust
triplet_distance_penalty: triplet_distance_penalty.unwrap_or(6.5),
```

### Step 15: Fix `triplet_distance_penalty` in `GraphRetrievalConfig::default()`

**File:** `crates/search/src/graph_retrieval/brute_force_triplet_search.rs`

**Current (lines 32-33):**
```rust
triplet_distance_penalty: 0.0,
```

**Target:**
```rust
triplet_distance_penalty: 6.5,
```

### Step 16: Fix `DEFAULT_CONTEXT_EXTENSION_ROUNDS`

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

**Current (line 22):**
```rust
const DEFAULT_CONTEXT_EXTENSION_ROUNDS: usize = 2;
```

**Target:**
```rust
const DEFAULT_CONTEXT_EXTENSION_ROUNDS: usize = 4;
```

### Step 17: Fix `DEFAULT_COT_MAX_ITER`

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

**Current (line 23):**
```rust
const DEFAULT_COT_MAX_ITER: usize = 2;
```

**Target:**
```rust
const DEFAULT_COT_MAX_ITER: usize = 4;
```

### Step 18: Fix context join separator in `completion_retriever.rs`

**File:** `crates/search/src/retrievers/completion_retriever.rs`

**Current (lines 102-106):**
```rust
let context_text = completion_context
    .iter()
    .filter_map(|item| item.payload.get("text").and_then(|value| value.as_str()))
    .collect::<Vec<_>>()
    .join("\n\n");
```

**Target:**
```rust
let context_text = completion_context
    .iter()
    .filter_map(|item| item.payload.get("text").and_then(|value| value.as_str()))
    .collect::<Vec<_>>()
    .join("\n");
```

### Step 19: Fix context join separator in `triplet_retriever.rs`

**File:** `crates/search/src/retrievers/triplet_retriever.rs`

**Current (line 92, inside `context_to_text`):**
```rust
.join("\n\n")
```

**Target:**
```rust
.join("\n")
```

### Step 20: Update tests that assert hardcoded `top_k` or penalty values

Several existing tests pass explicit values (e.g. `Some(2)`, `Some(5)`, `Some(0.0)`) to bypass defaults, so they will remain unaffected. However, review tests that rely on the default path (`None` for top_k) to confirm they still pass:

- `chunks_retriever.rs` tests: all pass explicit `Some(2)` -- no change needed.
- `summaries_retriever.rs` tests: all pass explicit `Some(2)` -- no change needed.
- `completion_retriever.rs` tests: all pass explicit `Some(2)` -- no change needed.
- `triplet_retriever.rs` tests: all pass explicit `Some(2)` -- no change needed.
- `graph_completion_retriever.rs` tests: pass explicit `Some(2)` and `Some(5)` -- no change needed.
- `advanced_graph_retrievers.rs` tests: pass explicit `Some(5)`, `Some(0.0)`, `Some(2)`, `Some(1)` -- no change needed.
- `temporal_retriever.rs` tests: pass explicit `Some(5)`, `Some(10)`, `Some(3)`, `Some(0.0)` -- no change needed.

Tests that assert on the `"\n\n"` join separator in `completion_retriever.rs` and `triplet_retriever.rs` should not be broken because those tests check for the presence of text content (e.g. `contains("chunk one")`), not the exact separator.

---

## Test Verification

1. Run `cargo check --all-targets` to verify compilation.
2. Run `cargo test -p cognee-search` to verify all existing tests pass.
3. Run `scripts/check_all.sh` for full CI validation.
4. Manually verify that the search E2E tests still produce correct results (the default behavior will now match Python more closely, so result counts may change in integration tests).

**Suggested new test:** Add a unit test that constructs each retriever with `None` for all optional params and asserts the internal field values match the Python defaults:
```rust
#[test]
fn default_values_match_python() {
    let retriever = ChunksRetriever::new(
        /* vector_db, embedding_engine */ ...,
        None, // top_k
    );
    assert_eq!(retriever.top_k, 5); // requires making top_k pub(crate) or adding a getter
}
```

---

## Dependencies on Other Tasks

- None. This task is self-contained and can be implemented independently.
- Other tasks that modify retriever construction (such as adding new parameters) should be aware of the updated defaults.
