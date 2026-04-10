# Task 19: Add `verbose` mode for result formatting

## Summary

Python's `search()` API has a `verbose: bool` parameter that controls the shape of returned results. When `verbose=False` (default), only the "best" result field is returned (completion if available, otherwise context, otherwise raw objects). When `verbose=True`, all three result facets are returned: `text_result` (completion), `context_result` (context), and `objects_result` (raw result objects). The Rust `SearchRequest` and `SearchResponse` have no equivalent -- all responses always include `result`, `context`, and `graphs` regardless of caller preference.

## Current Rust Behavior

**File:** `crates/search/src/types/search_request.rs`

`SearchRequest` has no `verbose` field:

```rust
pub struct SearchRequest {
    pub query_text: String,
    pub search_type: SearchType,
    pub top_k: Option<usize>,
    pub datasets: Option<Vec<String>>,
    pub dataset_ids: Option<Vec<Uuid>>,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub only_context: Option<bool>,
    pub use_combined_context: Option<bool>,
    pub session_id: Option<String>,
    pub node_type: Option<String>,
    pub node_name: Option<String>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    pub save_interaction: Option<bool>,
}
```

**File:** `crates/search/src/types/search_result.rs`

`SearchResponse` always carries all fields:

```rust
pub struct SearchResponse {
    pub search_type: SearchType,
    pub result: SearchOutput,
    pub context: Option<HashMap<String, SearchContext>>,
    pub graphs: Option<HashMap<String, SearchGraph>>,
    pub diagnostics: Option<HashMap<String, Value>>,
    pub datasets: Option<Vec<Uuid>>,
    pub only_context: bool,
    pub use_combined_context: bool,
}
```

There is no post-processing step that reduces the response based on a `verbose` flag.

## Required Behavior (Python Reference)

**File:** `/tmp/cognee-python/cognee/modules/search/methods/search.py`, lines 314-362

```python
def _backwards_compatible_search_results(search_results, verbose: bool):
    if backend_access_control_enabled():
        return_value = []
        for search_result in search_results:
            search_result_dict = {
                "dataset_id": search_result.dataset_id,
                "dataset_name": search_result.dataset_name,
                "dataset_tenant_id": search_result.dataset_tenant_id,
            }
            if verbose:
                search_result_dict["text_result"] = search_result.completion
                search_result_dict["context_result"] = search_result.context
                search_result_dict["objects_result"] = search_result.result_object
            else:
                search_result_dict["search_result"] = search_result.result
            return_value.append(search_result_dict)
        return return_value
    else:
        return_value = []
        if verbose:
            for search_result in search_results:
                search_result_dict = {
                    "text_result": search_result.completion,
                    "context_result": search_result.context,
                    "objects_result": search_result.result_object,
                }
                return_value.append(search_result_dict)
            return return_value
        else:
            for search_result in search_results:
                return_value.append(search_result.result)
            if len(return_value) == 1 and isinstance(return_value[0], list):
                return return_value[0]
            else:
                return return_value
```

Key behavior:
- `verbose=False` (default): Returns only the "best" result via `SearchResultPayload.result` property (completion > context > result_object).
- `verbose=True`: Returns a dict with all three result facets: `text_result`, `context_result`, `objects_result`.
- In Rust, the `SearchResponse` already has all three pieces (`result` = the output, `context` = the context map, `graphs` = the graph representation). What is needed is a `verbose` flag on the request and a corresponding field on the response so that callers (CLI, API layer) know how to present results.

## Step-by-Step Code Changes

### Change 1: Add `verbose` field to `SearchRequest`

**File:** `crates/search/src/types/search_request.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query_text: String,
    #[serde(default)]
    pub search_type: SearchType,
    pub top_k: Option<usize>,
    pub datasets: Option<Vec<String>>,
    pub dataset_ids: Option<Vec<Uuid>>,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub only_context: Option<bool>,
    pub use_combined_context: Option<bool>,
    pub session_id: Option<String>,
    pub node_type: Option<String>,
    pub node_name: Option<String>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    pub save_interaction: Option<bool>,
    pub verbose: Option<bool>,  // <-- NEW
}

impl SearchRequest {
    // ... existing methods ...

    pub fn verbose(&self) -> bool {
        self.verbose.unwrap_or(false)
    }
}
```

### Change 2: Add `verbose` field to `SearchResponse`

**File:** `crates/search/src/types/search_result.rs`

Add a `verbose` field to `SearchResponse`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub search_type: SearchType,
    pub result: SearchOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<HashMap<String, SearchContext>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graphs: Option<HashMap<String, SearchGraph>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<HashMap<String, Value>>,
    pub datasets: Option<Vec<Uuid>>,
    pub only_context: bool,
    pub use_combined_context: bool,
    pub verbose: bool,  // <-- NEW
}
```

Update `SearchResponse::from_output`:

```rust
impl SearchResponse {
    pub fn from_output(search_type: SearchType, output: SearchOutput) -> Self {
        Self {
            search_type,
            result: output,
            context: None,
            graphs: None,
            diagnostics: None,
            datasets: None,
            only_context: false,
            use_combined_context: false,
            verbose: false,  // <-- NEW
        }
    }
}
```

### Change 3: Wire `verbose` through `prepare_search_result`

**File:** `crates/search/src/orchestration/prepare_search_result.rs`

Update the function signature and add the `verbose` parameter:

```rust
pub fn prepare_search_result(
    search_type: SearchType,
    result: SearchOutput,
    context: Option<SearchContext>,
    datasets: Option<Vec<Uuid>>,
    only_context: bool,
    use_combined_context: bool,
    verbose: bool,  // <-- NEW
) -> SearchResponse {
```

In the function body, when `verbose` is `false`, strip context and graphs from the response:

```rust
    let (context_map, graphs) = if verbose || only_context {
        // In verbose mode or context-only mode, include everything
        let context_map = context
            .clone()
            .map(|items| HashMap::from([(context_label.clone(), items)]));
        let graphs = context
            .as_ref()
            .and_then(transform_context_to_graph)
            .map(|graph| HashMap::from([(context_label.clone(), graph)]));
        (context_map, graphs)
    } else {
        // In non-verbose mode, omit context and graphs
        (None, None)
    };
```

Set the `verbose` field in the response:

```rust
    SearchResponse {
        search_type,
        result,
        context: context_map,
        graphs,
        diagnostics,
        datasets,
        only_context,
        use_combined_context,
        verbose,
    }
```

### Change 4: Wire `verbose` through `SearchOrchestrator::search`

**File:** `crates/search/src/orchestration/search_orchestrator.rs`

Pass `request.verbose()` to all calls to `prepare_search_result`:

```rust
// In the only_context early return:
let mut response = prepare_search_result(
    request.search_type,
    SearchOutput::Items(output_context.clone()),
    Some(output_context),
    request.dataset_ids.clone(),
    true,
    request.use_combined_context(),
    request.verbose(),  // <-- NEW
);

// In the main completion path:
let mut response = prepare_search_result(
    request.search_type,
    output,
    context,
    request.dataset_ids.clone(),
    false,
    request.use_combined_context(),
    request.verbose(),  // <-- NEW
);
```

### Change 5: Update all test `SearchRequest` literals

Add `verbose: None` (or `verbose: Some(false)`) to every `SearchRequest` struct literal in tests across:

- `crates/search/src/orchestration/search_orchestrator.rs` -- all test `SearchRequest` structs
- `crates/search/src/orchestration/search_execution_builder.rs` -- all test `SearchRequest` structs

Example for each:

```rust
let request = SearchRequest {
    query_text: "hello".to_string(),
    search_type: SearchType::Chunks,
    // ... existing fields ...
    save_interaction: None,
    verbose: None,  // <-- NEW
};
```

### Change 6: Update `prepare_search_result` test call

**File:** `crates/search/src/orchestration/prepare_search_result.rs`, test module

Update the test call site to include the `verbose` parameter:

```rust
let response = super::prepare_search_result(
    SearchType::GraphCompletion,
    SearchOutput::Text("answer".to_string()),
    Some(context),
    None,
    false,
    false,
    false,  // <-- NEW: verbose
);
```

## Test Verification

### New tests to add

Add tests in `crates/search/src/orchestration/search_orchestrator.rs` test module:

```rust
#[tokio::test]
async fn verbose_response_includes_context_and_graph() {
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(FakeChunksRetriever));

    let orchestrator = super::SearchOrchestrator::new(registry);

    let request = SearchRequest {
        query_text: "hello".to_string(),
        search_type: SearchType::Chunks,
        top_k: Some(3),
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(false),
        use_combined_context: Some(false),
        session_id: None,
        node_type: None,
        node_name: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: None,
        verbose: Some(true),
    };

    let response = orchestrator.search(&request).await.unwrap();
    assert!(response.verbose);
    // Verbose mode includes context even for completion requests
    assert!(response.context.is_some());
}

#[tokio::test]
async fn non_verbose_response_strips_context() {
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(FakeChunksRetriever));

    let orchestrator = super::SearchOrchestrator::new(registry);

    let request = SearchRequest {
        query_text: "hello".to_string(),
        search_type: SearchType::Chunks,
        top_k: Some(3),
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(false),
        use_combined_context: Some(false),
        session_id: None,
        node_type: None,
        node_name: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: None,
        verbose: Some(false),
    };

    let response = orchestrator.search(&request).await.unwrap();
    assert!(!response.verbose);
    // Non-verbose mode omits context for completion requests
    assert!(response.context.is_none());
}
```

### How to verify

```bash
cargo test -p cognee-search
scripts/check_all.sh
```

## Dependencies

- No new crate dependencies required.
- This change is additive and backward-compatible: `verbose` defaults to `false`, so all existing behavior is preserved. The `Option<bool>` with `#[serde(default)]` means existing serialized `SearchRequest` values without the field will deserialize correctly.
