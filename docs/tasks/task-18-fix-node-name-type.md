# Task 18: Change `node_name` Type from `Option<String>` to `Option<Vec<String>>` in SearchRequest

## Summary

The `node_name` field in the Rust `SearchRequest` struct is typed as `Option<String>` (a single optional name), but the Python SDK uses `Optional[List[str]]` (an optional list of names). This mismatch means Rust callers can only filter by a single node name, whereas Python supports filtering by multiple names simultaneously with AND/OR semantics. This task corrects the type to `Option<Vec<String>>` and updates all construction sites and downstream consumers.

## Current Rust Type vs Python Type

### Rust (incorrect)

**File:** `crates/search/src/types/search_request.rs`, line 20

```rust
pub node_name: Option<String>,
```

### Python (reference)

**File:** `/tmp/cognee-python/cognee/api/v1/search/search.py`, line 37

```python
node_name: Optional[List[str]] = None,
```

The Python type flows unchanged through the entire search pipeline:
- `cognee/api/v1/search/search.py` line 37 -- top-level API
- `cognee/modules/search/methods/search.py` lines 47, 139, 187, 209 -- internal search functions
- `cognee/modules/search/methods/get_search_type_retriever_instance.py` line 58 -- retriever instantiation

In Python, `node_name` is always treated as a list of strings and is paired with `node_name_filter_operator` (AND/OR) to determine whether results must match ALL names or ANY name.

## All Rust Code Locations That Reference `node_name` and Need Updating

### 1. Struct definition (type change)

| File | Line | Current Code |
|---|---|---|
| `crates/search/src/types/search_request.rs` | 20 | `pub node_name: Option<String>,` |

### 2. SearchRequest construction sites (value `None` -- no code change needed, but must verify they still compile)

| File | Line(s) | Context |
|---|---|---|
| `crates/search/src/orchestration/search_orchestrator.rs` | 251, 288, 329, 391, 477, 557, 597 | Test `SearchRequest` literals, all set `node_name: None` |
| `crates/search/src/orchestration/search_execution_builder.rs` | 605, 669 | Test `SearchRequest` literals, all set `node_name: None` |
| `crates/search/tests/integration_search_matrix.rs` | 80, 325 | `make_request()` helper and inline literal, all set `node_name: None` |
| `crates/cognify/tests/integration_default_backend.rs` | 75 | `make_request()` helper, sets `node_name: None` |
| `crates/cli/src/commands/search.rs` | 89 | CLI request construction, sets `node_name: None` |

Since all existing construction sites set `node_name: None`, the type change from `Option<String>` to `Option<Vec<String>>` is fully backward-compatible at these sites -- `None` is valid for both types.

### 3. GraphDb trait and implementations (already correct -- use `&[String]`)

These already use `node_names: &[String]` (a slice of strings) and do NOT need changing for this task:

| File | Line | Signature |
|---|---|---|
| `crates/graph/src/traits.rs` | 213 | `async fn get_nodeset_subgraph(&self, node_type: &str, node_names: &[String])` |
| `crates/graph/src/ladybug.rs` | 1120 | Implementation of `get_nodeset_subgraph` |
| `crates/graph/src/mock.rs` | 294 | Mock implementation of `get_nodeset_subgraph` |
| `crates/search/src/orchestration/search_execution_builder.rs` | 487 | Test mock `get_nodeset_subgraph` |
| `crates/search/src/retrievers/graph_completion_retriever.rs` | 449 | Test mock `get_nodeset_subgraph` |
| `crates/search/src/retrievers/temporal_retriever.rs` | 814 | Test mock `get_nodeset_subgraph` |
| `crates/search/src/retrievers/cypher_nl_retrievers.rs` | 373 | Test mock `get_nodeset_subgraph` |

The graph layer already expects a slice of names, which aligns perfectly with the `Vec<String>` type. When Task 17 wires up node filtering, it will call `get_nodeset_subgraph(node_type, &node_names)` where `node_names` comes from `SearchRequest.node_name`.

### 4. Other `node_name` references (NOT affected by this task)

These use `node_name` in unrelated contexts (entity models, ontology, id generation) and are not affected:

- `crates/models/src/entity.rs` lines 67, 73, 79 -- `Entity::new()` constructor parameter
- `crates/ontology/src/traits.rs` line 123 -- `OntologyResolver` trait parameter (`&str`)
- `crates/ontology/src/noop.rs` line 43, `crates/ontology/src/rdflib.rs` lines 124, 134, 139, 159 -- ontology implementations
- `crates/utils/src/id_generation.rs` line 98 -- `generate_node_name()` utility function
- `crates/search/src/graph_retrieval/brute_force_triplet_search.rs` lines 128, 151, 155 -- local `node_names` HashMap for display names
- `crates/search/src/retrievers/temporal_retriever.rs` lines 368, 416 -- `extract_node_name()` helper
- `crates/cognify/src/fact_extraction/extractor.rs` line 344 -- test for empty node names
- `crates/cognify/tests/integration_fact_extraction.rs` lines 49, 53, 59, 65 -- test assertions on extracted node names

## Step-by-Step Changes

### Step 1: Change the type in `SearchRequest`

**File:** `crates/search/src/types/search_request.rs`

```rust
// Before (line 20):
pub node_name: Option<String>,

// After:
pub node_name: Option<Vec<String>>,
```

This is the only code change required. The `Serialize`/`Deserialize` derives handle `Vec<String>` automatically -- JSON callers will send `"node_name": ["Alice", "Bob"]` instead of `"node_name": "Alice"`.

### Step 2: Verify all construction sites compile

Every existing `SearchRequest` literal in the codebase sets `node_name: None`. Since `None` is valid for both `Option<String>` and `Option<Vec<String>>`, no changes are needed at these sites. However, verify compilation:

```bash
cargo check --all-targets
```

The following files contain `SearchRequest` literals that must compile without changes:
1. `crates/search/src/orchestration/search_orchestrator.rs` -- 7 instances
2. `crates/search/src/orchestration/search_execution_builder.rs` -- 2 instances
3. `crates/search/tests/integration_search_matrix.rs` -- 2 instances
4. `crates/cognify/tests/integration_default_backend.rs` -- 1 instance
5. `crates/cli/src/commands/search.rs` -- 1 instance

### Step 3: Update serde deserialization behavior (if needed)

Consider whether the API should accept both a single string and a list for backward compatibility. The simplest approach is to NOT add backward compatibility -- just require a list. If backward compat is needed, add a custom deserializer:

```rust
use serde::Deserializer;

fn deserialize_node_name<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    // Accept either a single string or a list of strings
    // e.g., "Alice" -> Some(vec!["Alice"]) or ["Alice", "Bob"] -> Some(vec!["Alice", "Bob"])
}
```

**Recommendation:** Skip the custom deserializer for now. The Rust API has not been released, so there are no backward compatibility concerns. Use the standard `Option<Vec<String>>` deserialization.

### Step 4: Update the CLI `SearchArgs` (future consideration)

**File:** `crates/cli/src/cli.rs`

Currently the CLI does not expose a `--node-name` argument (it always sets `node_name: None`). When the CLI does add support (likely as part of Task 17), it should accept multiple values:

```rust
/// Filter results to specific named entities
#[arg(long, num_args = 1..)]
node_name: Option<Vec<String>>,
```

This is NOT part of this task but is noted here for Task 17's reference.

### Step 5: Run full check suite

```bash
scripts/check_all.sh
```

This verifies formatting, compilation, clippy, and all wrapper binding checks pass.

## Test Verification

### Compilation test (primary)

Since the type change is from `Option<String>` to `Option<Vec<String>>` and all existing construction sites use `None`, the primary verification is that everything compiles:

```bash
cargo check --all-targets
```

### Existing tests (regression)

All existing tests must continue to pass since they all use `node_name: None`:

```bash
cargo test -p cognee-search
cargo test -p cognee-cognify --test integration_default_backend
```

### New unit test

Add a test to `crates/search/src/types/search_request.rs` verifying that `node_name` correctly serializes and deserializes as a list:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_name_deserializes_as_vec() {
        let json = r#"{
            "query_text": "test",
            "node_name": ["Alice", "Bob"]
        }"#;
        let request: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            request.node_name,
            Some(vec!["Alice".to_string(), "Bob".to_string()])
        );
    }

    #[test]
    fn node_name_none_when_absent() {
        let json = r#"{"query_text": "test"}"#;
        let request: SearchRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.node_name, None);
    }

    #[test]
    fn node_name_serializes_as_vec() {
        let request = SearchRequest {
            query_text: "test".to_string(),
            search_type: SearchType::default(),
            top_k: None,
            datasets: None,
            dataset_ids: None,
            system_prompt: None,
            system_prompt_path: None,
            only_context: None,
            use_combined_context: None,
            session_id: None,
            node_type: None,
            node_name: Some(vec!["Alice".to_string()]),
            wide_search_top_k: None,
            triplet_distance_penalty: None,
            save_interaction: None,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["node_name"], serde_json::json!(["Alice"]));
    }
}
```

## Dependencies

- **Task 17 (Add node filtering)** depends on this task. Task 17 introduces the actual usage of `node_name` in the graph retrieval pipeline, threading it from `SearchRequest` through `GraphRetrievalConfig` into `brute_force_triplet_search()` and `get_nodeset_subgraph()`. Task 17's document already notes (line 54, 74, 88) that `node_name` should be `Option<Vec<String>>`. Completing Task 18 first means Task 17 can use the correct type from the start without needing a temporary `Option<String>` workaround.
- **No other task dependencies.** This is a self-contained type correction.
