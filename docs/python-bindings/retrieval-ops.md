# Retrieval Operations: search, recall

## Status: ❌ Not implemented

## What is missing

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `Cognee.search(query, opts?)` | `cg_sdk_search` | `cognee.search()` | Query the knowledge graph with one of 15 search strategies |
| `Cognee.recall(query, opts?)` | `cg_sdk_recall` | `cognee.recall()` | Smart session-first routing search |

### Search types (15 variants)

```python
"GRAPH_COMPLETION"                  # default — LLM over graph context
"GRAPH_COMPLETION_COT"              # chain-of-thought variant
"GRAPH_COMPLETION_CONTEXT_EXTENSION"
"GRAPH_SUMMARY_COMPLETION"
"TRIPLET_COMPLETION"
"RAG_COMPLETION"                    # vector similarity + LLM
"CHUNKS"                            # raw chunks
"SUMMARIES"                         # document summaries
"TEMPORAL"                          # temporal graph traversal
"CYPHER"                            # raw Cypher/graph query
"NATURAL_LANGUAGE"
"FEELING_LUCKY"
"FEEDBACK"
"CODING_RULES"
"CHUNKS_LEXICAL"
```

### Search options

```python
opts = {
    "search_type": str,              # default: "GRAPH_COMPLETION"
    "datasets": [str],               # filter by dataset name
    "dataset_ids": [str],            # filter by dataset UUID
    "top_k": int,
    "system_prompt": str,
    "session_id": str,
    "node_type": str,
    "node_name": [str],
    "only_context": bool,
    "use_combined_context": bool,
    "verbose": bool,
    "save_interaction": bool,        # default True
    "auto_feedback_detection": bool,
}
```

### Recall options

```python
opts = {
    "search_type": str,
    "datasets": [str],
    "top_k": int,                    # default 10
    "auto_route": bool,              # default False
    "session_id": str,
    "scope": str | [str],            # "auto", "graph", "session", "trace", "graph_context"
}
```

### Result shapes

**Search** result: JSON array or object (pass-through from Rust serde, matches TS `CogneeSearchResponse`).

**Recall** result:
```python
{
    "items": [...],
    "search_type_used": str | None,
    "auto_routed": bool,
    "search_response": dict | None,
}
```

## Rationale

`search()` completes the primary user workflow: add → cognify → search. Without it, users can
ingest and build a knowledge graph but cannot query it. `recall()` adds the session-aware routing
layer used in conversational applications. Together these are the most user-visible operations in
the SDK.

## Implementation plan

**Prerequisite:** hoist the search/recall op bodies from `capi/cognee-capi/src/sdk_retrieval.rs`
(SearchType parsing, `ScopeInput` building, `SearchRequest` assembly) into
`cognee_bindings_common::ops` — see [core-pipeline-ops.md](core-pipeline-ops.md) Step 0.

Note one behaviour detail from the capi implementation worth preserving: a `userId` key in opts
is ignored — `SearchRequest.user_id` is always populated from the handle's `owner_id` so that
dataset-name resolution works (`capi/cognee-capi/src/sdk_retrieval.rs:343`).

### Step 1 — Create `python/src/sdk_retrieval.rs`

Follow the same pattern as `sdk_ops.rs` from [core-pipeline-ops.md](core-pipeline-ops.md):

```rust
pub fn py_sdk_search<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    query: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let opts_value = opts.map(|o| py_to_serde(&o)).transpose()?;
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = ops::search(&handle, &query, opts_value.as_ref())
            .await
            .map_err(sdk_error_to_py)?;
        Python::with_gil(|py| serde_to_py(py, &result))
    })
}

pub fn py_sdk_recall<'py>(
    py: Python<'py>,
    handle: Arc<HandleState>,
    query: String,
    opts: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> { /* same pattern, ops::recall */ }
```

### Step 2 — Wire into `PyCognee`

```rust
#[pyo3(signature = (query, opts=None))]
fn search<'py>(&self, py: Python<'py>, query: String, opts: Option<Bound<'py, PyAny>>)
    -> PyResult<Bound<'py, PyAny>>
{
    sdk_retrieval::py_sdk_search(py, Arc::clone(&self.inner), query, opts)
}

#[pyo3(signature = (query, opts=None))]
fn recall<'py>(&self, py: Python<'py>, query: String, opts: Option<Bound<'py, PyAny>>)
    -> PyResult<Bound<'py, PyAny>>
{
    sdk_retrieval::py_sdk_recall(py, Arc::clone(&self.inner), query, opts)
}
```

### Step 3 — Opts key normalisation

Python users will naturally use `snake_case` keys (`search_type`, `top_k`). The Rust layer
(inherited from C API and TS) expects `camelCase` JSON keys (`searchType`, `topK`). Add a
normalisation layer in `marshal.rs`:

```rust
pub fn snake_opts_to_serde(opts: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    let mut value = py_to_serde(opts)?;
    // convert top-level object keys: snake_case → camelCase
    snake_to_camel_keys(&mut value);
    Ok(value)
}
```

Alternatively, accept both `snake_case` and `camelCase` in opts (lenient) and normalise before
passing to Rust. Matching the TS binding exactly (camelCase) is also acceptable if documented.

### Step 4 — Export `SearchType` constants

Expose a `SearchType` enum or constants dict so users can use symbolic names:

```python
# python/cognee_pipeline/__init__.py
class SearchType:
    GRAPH_COMPLETION = "GRAPH_COMPLETION"
    RAG_COMPLETION = "RAG_COMPLETION"
    CHUNKS = "CHUNKS"
    SUMMARIES = "SUMMARIES"
    # ... all 15 variants
```

This is a pure Python addition, no Rust changes needed.

### Step 5 — Tests

Add `python/tests/test_retrieval.py` (integration tests require LLM; unit tests can mock):

```python
async def test_search_default_type(cognee_with_data):
    result = await cognee_with_data.search("What is X?")
    assert isinstance(result, (list, dict))

async def test_search_chunks(cognee_with_data):
    result = await cognee_with_data.search(
        "What is X?", {"search_type": "CHUNKS", "top_k": 5}
    )
    assert isinstance(result, list)

async def test_search_unknown_type(cognee):
    with pytest.raises(CogneeValidationError):
        await cognee.search("q", {"search_type": "NOT_A_TYPE"})

async def test_recall_basic(cognee_with_data):
    result = await cognee_with_data.recall("What is X?")
    assert "items" in result
    assert "auto_routed" in result
```

### Acceptance criteria

- `await cognee.search("query")` returns a list or dict without raising
- `search_type` option accepts all 15 string variants (case-sensitive)
- An unknown `search_type` raises `CogneeValidationError`
- `await cognee.recall("query")` returns a dict with `items`, `auto_routed`, `search_type_used`
- `SearchType.GRAPH_COMPLETION` (or equivalent) is importable as a constant
