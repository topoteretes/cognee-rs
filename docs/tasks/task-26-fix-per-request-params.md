# Task 26: Fix Per-Request Parameter Passing -- Pass `SearchRequest` Through the Trait

## Summary

`SearchRequest` carries per-request parameters (`top_k`, `system_prompt`, `system_prompt_path`, `wide_search_top_k`, `triplet_distance_penalty`, `node_type`, `node_name`) that the caller intends to override on each search call. However, the `SearchRetriever` trait methods (`get_context`, `get_completion`) never receive these parameters. The orchestrator calls `retriever.get_context(&request.query_text)` and `retriever.get_completion(&request.query_text, context, &session_context)` -- passing only the query string -- while all per-request params on `SearchRequest` are silently ignored.

In Python, this works because a **fresh retriever instance** is created for every search call with the per-request params baked into the constructor. In Rust, retrievers are **`Arc` singletons** registered at startup, so constructor-time params are fixed defaults and never change per request.

This task adds a `SearchParams` argument to the `SearchRetriever` trait methods so per-request overrides flow from `SearchRequest` through the orchestrator to each retriever.

## Python Approach

**Factory-per-call pattern:** `get_search_type_retriever_instance()` is called on every search invocation. It extracts `top_k`, `system_prompt`, `node_type`, `node_name`, `wide_search_top_k`, `triplet_distance_penalty`, etc. from the incoming kwargs, then constructs a **brand new** retriever instance with those values as constructor arguments.

```python
# From get_search_type_retriever_instance.py
top_k = kwargs.get("top_k", 10)
system_prompt = kwargs.get("system_prompt")
wide_search_top_k = kwargs.get("wide_search_top_k", 100)
triplet_distance_penalty = kwargs.get("triplet_distance_penalty", 6.5)
# ... etc

search_core_registry = {
    SearchType.GRAPH_COMPLETION: (
        GraphCompletionRetriever,
        {
            "top_k": top_k,
            "system_prompt": system_prompt,
            "wide_search_top_k": wide_search_top_k,
            "triplet_distance_penalty": triplet_distance_penalty,
            # ... all params baked into constructor dict
        },
    ),
    # ...
}

retriever_instance = retriever_cls(**retriever_args)
```

The Python retriever then uses `self.top_k`, `self.system_prompt`, etc. as instance fields throughout its methods. Each call gets isolated parameters because each call gets a fresh object.

**Key params that flow per-request in Python:**
| Parameter | Default | Used By |
|---|---|---|
| `top_k` | 10 | All vector-based retrievers |
| `system_prompt` | `None` | All completion retrievers |
| `system_prompt_path` | `"answer_simple_question.txt"` | All completion retrievers |
| `wide_search_top_k` | 100 | Graph, Temporal |
| `triplet_distance_penalty` | 6.5 | Graph, Temporal |
| `node_type` | `NodeSet` | Graph, Temporal |
| `node_name` | `None` | Graph, Temporal, CodingRules |
| `node_name_filter_operator` | `"OR"` | Graph, Temporal |
| `feedback_influence` | 0.0 | Graph, Temporal |
| `session_id` | `None` | All that support sessions |
| `max_iter` | 4 | CoT |
| `context_extension_rounds` | 4 | ContextExtension |

## Rust Limitation

**Arc singleton pattern:** In the Rust `SearchBuilder`, all retrievers are created once at build time and stored as `Arc<dyn SearchRetriever>` in a `HashMap<SearchType, SearchRetrieverRef>`. The `SearchOrchestrator` holds this registry for its entire lifetime.

```rust
// SearchBuilder::register_standard_retrievers() -- called once
self.retrievers.insert(
    SearchType::GraphCompletion,
    Arc::new(GraphCompletionRetriever::new(
        vector_db, embedding_engine, graph_db, llm,
        None,  // top_k -- locked at default 10 forever
        None,  // wide_search_top_k -- locked at default 20 forever
        None,  // triplet_distance_penalty -- locked at 0.0 forever
        None, None, None, None,
    )),
);
```

**Trait signature lacks params:** The `SearchRetriever` trait has:

```rust
async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError>;
async fn get_completion(
    &self, query: &str, context: Option<SearchContext>, session: &SessionContext,
) -> Result<SearchOutput, SearchError>;
```

There is no way to pass `top_k`, `system_prompt`, etc. from the caller. The orchestrator has the `SearchRequest` with all these fields, but it only extracts `query_text` and `session_id` from it.

**Consequence:** When a user sends `{"query_text": "...", "top_k": 3, "system_prompt": "Be concise"}`, both `top_k` and `system_prompt` are silently ignored. The retriever always uses its constructor defaults.

## Proposed Solutions

### Option A: Add `&SearchRequest` directly to trait methods

Pass the entire `SearchRequest` to both `get_context` and `get_completion`.

```rust
#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;

    async fn get_context(
        &self,
        query: &str,
        request: &SearchRequest,
    ) -> Result<SearchContext, SearchError>;

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        request: &SearchRequest,
    ) -> Result<SearchOutput, SearchError>;
}
```

**Pros:**
- Simplest to implement -- one struct already exists, just thread it through.
- All current and future `SearchRequest` fields automatically available.
- Easy for the orchestrator -- it already has `&SearchRequest`.

**Cons:**
- Tight coupling: the trait now depends on `SearchRequest`, which is an API-layer struct. Retrievers that don't need any request params must still accept it.
- `SearchRequest` contains orchestration concerns (`only_context`, `use_combined_context`, `dataset_ids`, `save_interaction`) that no retriever should care about.
- Adding a field to `SearchRequest` for one retriever type pollutes all retrievers.
- Makes the trait harder to implement in tests -- every fake retriever must construct/accept a `SearchRequest`.

### Option B: Create a `SearchParams` config struct passed alongside query

Introduce a lean, retriever-focused `SearchParams` that contains only the per-request overridable behavioral params. The orchestrator extracts `SearchParams` from `SearchRequest` before calling the retriever.

```rust
#[derive(Debug, Clone, Default)]
pub struct SearchParams {
    pub top_k: Option<usize>,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    pub node_type: Option<String>,
    pub node_name: Option<String>,
    pub node_name_filter_operator: Option<String>,
    pub feedback_influence: Option<f32>,
    pub max_iter: Option<usize>,
    pub context_extension_rounds: Option<usize>,
}

#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError>;

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError>;
}
```

**Pros:**
- Clean separation: `SearchParams` carries only retriever-relevant behavioral params, not orchestration concerns.
- `SearchParams` derives `Default`, making test implementations trivial (`&SearchParams::default()`).
- Each retriever reads only the fields it cares about, using its constructor default as fallback (`params.top_k.unwrap_or(self.top_k)`).
- New retriever-specific params can be added to `SearchParams` without touching `SearchRequest` (and vice versa).
- `SearchParams` can be reused by non-orchestrator callers (e.g., direct retriever tests, library consumers).

**Cons:**
- Requires a translation layer (`SearchRequest` -> `SearchParams`) in the orchestrator.
- Two structs carry overlapping fields (`SearchRequest.top_k` and `SearchParams.top_k`), requiring a `From` impl or conversion method.
- Adding a new per-request param means adding it in two places (SearchRequest + SearchParams).

### Option C: Use task-local storage (tokio::task_local)

Store a `SearchParams` in a tokio task-local variable. Retrievers read from it implicitly.

```rust
tokio::task_local! {
    static SEARCH_PARAMS: SearchParams;
}

// In orchestrator:
SEARCH_PARAMS.scope(params, async { retriever.get_context(query).await }).await

// In retriever:
let top_k = SEARCH_PARAMS.try_with(|p| p.top_k).unwrap_or(None);
```

**Pros:**
- Zero signature changes to the trait -- fully backward compatible.
- Retrievers that don't need params don't need to change at all.

**Cons:**
- Hidden implicit dependency -- makes code harder to reason about and test.
- `task_local!` has ergonomic issues: requires `.scope()` wrapping at every call site, panics if accessed outside scope.
- Not idiomatic Rust -- violates "explicit is better than implicit" principle.
- Debugging is harder when data flows through invisible channels.
- Doesn't compose well if a retriever delegates to another retriever (nested scopes).

## Recommended Approach: Option B -- `SearchParams` struct

Option B provides the best balance of clean API design, testability, and separation of concerns. It avoids the API-layer coupling of Option A and the implicit magic of Option C.

The core insight is that `SearchRequest` is an **API boundary type** (serialized from JSON, carries orchestration flags), while `SearchParams` is a **retriever behavior type** (controls how retrieval works). These are separate concerns and should be separate types.

## Step-by-Step Changes

### Step 1: Define `SearchParams` struct

**File:** `crates/search/src/types/search_params.rs` (new file)

```rust
/// Per-request retriever behavior overrides.
///
/// All fields are optional. When `None`, the retriever falls back to its
/// constructor-time defaults. This lets callers override only the params
/// they care about on a per-request basis.
#[derive(Debug, Clone, Default)]
pub struct SearchParams {
    /// Max number of results to return from vector search.
    pub top_k: Option<usize>,

    /// Override the LLM system prompt text directly.
    pub system_prompt: Option<String>,

    /// Override the LLM system prompt by file path.
    pub system_prompt_path: Option<String>,

    /// Number of candidates for wide graph search (before re-ranking).
    pub wide_search_top_k: Option<usize>,

    /// Distance penalty applied during triplet scoring.
    pub triplet_distance_penalty: Option<f32>,

    /// Filter graph to nodes of this type.
    pub node_type: Option<String>,

    /// Filter graph to nodes with this name.
    pub node_name: Option<String>,

    /// "OR" (default) or "AND" for multi-name filtering.
    pub node_name_filter_operator: Option<String>,

    /// Influence weight for feedback-based re-ranking.
    pub feedback_influence: Option<f32>,

    /// Maximum CoT iterations (GraphCompletionCot).
    pub max_iter: Option<usize>,

    /// Number of context extension rounds (GraphCompletionContextExtension).
    pub context_extension_rounds: Option<usize>,
}
```

Add helper methods:

```rust
impl SearchParams {
    pub fn top_k_or(&self, default: usize) -> usize {
        self.top_k.unwrap_or(default)
    }

    pub fn wide_search_top_k_or(&self, default: usize) -> usize {
        self.wide_search_top_k.unwrap_or(default)
    }

    pub fn triplet_distance_penalty_or(&self, default: f32) -> f32 {
        self.triplet_distance_penalty.unwrap_or(default)
    }
}
```

### Step 2: Add conversion from `SearchRequest` to `SearchParams`

**File:** `crates/search/src/types/search_params.rs`

```rust
impl From<&SearchRequest> for SearchParams {
    fn from(req: &SearchRequest) -> Self {
        Self {
            top_k: req.top_k,
            system_prompt: req.system_prompt.clone(),
            system_prompt_path: req.system_prompt_path.clone(),
            wide_search_top_k: req.wide_search_top_k,
            triplet_distance_penalty: req.triplet_distance_penalty,
            node_type: req.node_type.clone(),
            node_name: req.node_name.clone(),
            node_name_filter_operator: None, // future: add to SearchRequest
            feedback_influence: None,         // future: add to SearchRequest
            max_iter: None,                   // future: add to SearchRequest
            context_extension_rounds: None,   // future: add to SearchRequest
        }
    }
}
```

### Step 3: Export `SearchParams` from the types module

**File:** `crates/search/src/types/mod.rs`

Add `mod search_params;` and `pub use search_params::SearchParams;`.

### Step 4: Update `SearchRetriever` trait signature

**File:** `crates/search/src/retrievers/base_retriever.rs`

```rust
use crate::types::{SearchContext, SearchError, SearchOutput, SearchParams, SearchType};

#[async_trait]
pub trait SearchRetriever: Send + Sync {
    fn search_type(&self) -> SearchType;

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError>;

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError>;
}
```

### Step 5: Update `SearchOrchestrator::search()` to build and pass `SearchParams`

**File:** `crates/search/src/orchestration/search_orchestrator.rs`

In the `search()` method, build `SearchParams` from the request and pass it through:

```rust
pub async fn search(
    &self,
    request: &SearchRequest,
) -> Result<SearchResponse, SearchError> {
    let retriever = self.registry.get(request.search_type)?;
    let params = SearchParams::from(request);
    // ...

    let base_context = if include_context {
        Some(retriever.get_context(&request.query_text, &params).await?)
    } else {
        None
    };
    // ...

    let output = retriever
        .get_completion(&request.query_text, context.clone(), &session_context, &params)
        .await?;
    // ...
}
```

### Step 6: Update `ChunksRetriever` -- use `params.top_k` with fallback

**File:** `crates/search/src/retrievers/chunks_retriever.rs`

The retriever's constructor `top_k` becomes the fallback default. The per-request `params.top_k` takes priority.

```rust
async fn get_context(
    &self,
    query: &str,
    params: &SearchParams,
) -> Result<SearchContext, SearchError> {
    let top_k = params.top_k_or(self.top_k);
    // ... use top_k instead of self.top_k in search_similar call
}
```

### Step 7: Update `SummariesRetriever` -- same pattern as Chunks

**File:** `crates/search/src/retrievers/summaries_retriever.rs`

Same as Step 6: `params.top_k_or(self.top_k)` in `get_context`.

### Step 8: Update `CompletionRetriever` -- use `params.top_k` and `params.system_prompt`

**File:** `crates/search/src/retrievers/completion_retriever.rs`

```rust
async fn get_context(
    &self,
    query: &str,
    params: &SearchParams,
) -> Result<SearchContext, SearchError> {
    let top_k = params.top_k_or(self.top_k);
    // ... use top_k in search_similar
}

async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
    params: &SearchParams,
) -> Result<SearchOutput, SearchError> {
    // ...
    let system_prompt = resolve_system_prompt(
        params.system_prompt.as_deref().or(self.system_prompt.as_deref()),
        params.system_prompt_path.as_deref().or(self.system_prompt_path.as_deref()),
    )?;
    // ...
}
```

### Step 9: Update `TripletRetriever` -- use `params.top_k` and `params.system_prompt`

**File:** `crates/search/src/retrievers/triplet_retriever.rs`

Same pattern as CompletionRetriever: `params.top_k_or(self.top_k)` and `params.system_prompt` override in both `get_context` and `get_completion`.

### Step 10: Update `GraphCompletionRetriever` -- use `params.top_k`, `wide_search_top_k`, `triplet_distance_penalty`, `system_prompt`

**File:** `crates/search/src/retrievers/graph_completion_retriever.rs`

```rust
async fn get_context(
    &self,
    query: &str,
    params: &SearchParams,
) -> Result<SearchContext, SearchError> {
    let config = GraphRetrievalConfig {
        top_k: params.top_k_or(self.top_k),
        wide_search_top_k: params.wide_search_top_k_or(self.wide_search_top_k),
        triplet_distance_penalty: params.triplet_distance_penalty_or(
            self.triplet_distance_penalty,
        ),
    };
    // ...
}

async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
    params: &SearchParams,
) -> Result<SearchOutput, SearchError> {
    // ...
    let system_prompt = resolve_system_prompt(
        params.system_prompt.as_deref().or(self.system_prompt.as_deref()),
        params.system_prompt_path.as_deref().or(self.system_prompt_path.as_deref()),
    )?;
    // ...
}
```

### Step 11: Update `GraphSummaryCompletionRetriever` -- same graph params plus system_prompt

**File:** `crates/search/src/retrievers/advanced_graph_retrievers.rs`

Apply the same pattern to the `GraphRetrieverCore` helper or pass params through. Each advanced retriever's `get_context` delegates to `core.get_context(query)` which must now accept `&SearchParams` for `top_k`, `wide_search_top_k`, and `triplet_distance_penalty`. Also override `system_prompt` in `get_completion`.

Update `GraphRetrieverCore::get_context` to accept `&SearchParams`:

```rust
impl GraphRetrieverCore {
    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        let config = GraphRetrievalConfig {
            top_k: params.top_k_or(self.top_k),
            wide_search_top_k: params.wide_search_top_k_or(self.wide_search_top_k),
            triplet_distance_penalty: params.triplet_distance_penalty_or(
                self.triplet_distance_penalty,
            ),
        };
        // ...
    }
}
```

### Step 12: Update `GraphCompletionContextExtensionRetriever` -- add `context_extension_rounds` override

In `get_completion`, use `params.context_extension_rounds.unwrap_or(self.context_extension_rounds)` for the loop bound.

### Step 13: Update `GraphCompletionCotRetriever` -- add `max_iter` override

In `get_completion`, use `params.max_iter.unwrap_or(self.max_iter)` for the CoT iteration bound.

### Step 14: Update `TemporalRetriever` -- same graph params

**File:** `crates/search/src/retrievers/temporal_retriever.rs`

Same as GraphCompletionRetriever: `params.top_k_or(self.top_k)`, `params.wide_search_top_k_or(...)`, `params.triplet_distance_penalty_or(...)`, `params.system_prompt` override.

### Step 15: Update `LexicalRetriever` / `JaccardChunksRetriever` -- use `params.top_k`

**File:** `crates/search/src/retrievers/lexical_retriever.rs`

Use `params.top_k_or(self.top_k)` in `get_context`.

### Step 16: Update `CypherSearchRetriever` -- pass `params` through (no overrides needed)

**File:** `crates/search/src/retrievers/cypher_nl_retrievers.rs`

`CypherSearchRetriever` doesn't currently use any of the overridable params, but must accept `&SearchParams` to satisfy the updated trait. It can simply ignore the argument.

### Step 17: Update `NaturalLanguageRetriever` -- pass `params` through

Same as Cypher: accept `&SearchParams` to satisfy the trait. No current overrides.

### Step 18: Update `FeelingLuckyRetriever` -- forward `params` to delegate

**File:** `crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs`

`FeelingLuckyRetriever` delegates to another retriever. It must forward `params`:

```rust
async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
    params: &SearchParams,
) -> Result<SearchOutput, SearchError> {
    let delegate = self.select_retriever(query).await?;
    delegate.get_completion(query, context, session, params).await
}
```

### Step 19: Update `FeedbackRetriever` and `CodingRulesRetriever` -- pass `params` through

Accept `&SearchParams`. `CodingRulesRetriever` could use `params.node_name` as an override for the rules nodeset name in a future enhancement.

### Step 20: Update all test implementations

Every test struct implementing `SearchRetriever` must add the `params: &SearchParams` parameter to both `get_context` and `get_completion`. Since all test impls can use `&SearchParams::default()`, this is a mechanical change.

**Files affected:**
- `crates/search/src/orchestration/search_orchestrator.rs` (3 test structs: `FakeChunksRetriever`, `FakeGraphRetriever`, `FakeDatasetRetriever`)
- `crates/search/src/orchestration/search_execution_builder.rs` (2 test structs: `FakeRetriever`, `ContextRetriever`)
- Each retriever's own `mod tests` section
- Any integration tests in `tests/` that implement the trait

### Step 21: Add unit test verifying per-request `top_k` override

**File:** `crates/search/src/retrievers/chunks_retriever.rs` (append to existing tests)

```rust
#[tokio::test]
async fn per_request_top_k_overrides_constructor_default() {
    let retriever = ChunksRetriever::new(
        Arc::new(TestVectorDb {
            has_collection: true,
            results: vec![
                sample_result("a", 0.9),
                sample_result("b", 0.8),
                sample_result("c", 0.7),
            ],
        }),
        Arc::new(TestEmbeddingEngine),
        Some(10), // constructor default: 10
    );

    let params = SearchParams {
        top_k: Some(1), // override to 1
        ..Default::default()
    };

    let context = retriever.get_context("query", &params).await.unwrap();
    assert_eq!(context.len(), 1);
}
```

### Step 22: Add unit test verifying per-request `system_prompt` override

**File:** `crates/search/src/retrievers/completion_retriever.rs` (append to existing tests)

```rust
#[tokio::test]
async fn per_request_system_prompt_overrides_constructor_default() {
    let llm = Arc::new(TestLlm {
        response_text: "answer".to_string(),
        ..Default::default()
    });

    let retriever = CompletionRetriever::new(
        Arc::new(TestVectorDb { has_collection: true, results: vec![sample_result("chunk", 0.9)] }),
        Arc::new(TestEmbeddingEngine),
        Arc::clone(&llm) as Arc<dyn Llm>,
        Some(2),
        Some("default system prompt".to_string()), // constructor
        None,
        None,
        None,
    );

    let params = SearchParams {
        system_prompt: Some("override system prompt".to_string()),
        ..Default::default()
    };

    let _ = retriever
        .get_completion("q", None, &SessionContext::default(), &params)
        .await
        .unwrap();

    let messages = llm.last_messages.lock().unwrap().clone();
    assert_eq!(messages[0].content, "override system prompt");
}
```

### Step 23: Add orchestrator-level test verifying params flow end-to-end

**File:** `crates/search/src/orchestration/search_orchestrator.rs` (append to existing tests)

Create a test retriever that captures the `SearchParams` it receives, then verify it matches the `SearchRequest` fields.

```rust
#[tokio::test]
async fn search_params_flow_from_request_to_retriever() {
    struct CapturingRetriever {
        captured_params: Mutex<Option<SearchParams>>,
    }

    #[async_trait]
    impl SearchRetriever for CapturingRetriever {
        fn search_type(&self) -> SearchType { SearchType::Chunks }

        async fn get_context(
            &self, _query: &str, _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            Ok(vec![])
        }

        async fn get_completion(
            &self, _query: &str, _ctx: Option<SearchContext>,
            _session: &SessionContext, params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            *self.captured_params.lock().unwrap() = Some(params.clone());
            Ok(SearchOutput::Text("ok".to_string()))
        }
    }

    let retriever = Arc::new(CapturingRetriever {
        captured_params: Mutex::new(None),
    });

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::clone(&retriever) as SearchRetrieverRef);
    let orchestrator = SearchOrchestrator::new(registry);

    let request = SearchRequest {
        query_text: "test".to_string(),
        search_type: SearchType::Chunks,
        top_k: Some(42),
        system_prompt: Some("custom prompt".to_string()),
        // ... other fields None/default
    };

    let _ = orchestrator.search(&request).await.unwrap();

    let captured = retriever.captured_params.lock().unwrap().clone().unwrap();
    assert_eq!(captured.top_k, Some(42));
    assert_eq!(captured.system_prompt.as_deref(), Some("custom prompt"));
}
```

## Impact Analysis

### Retrievers That Need Parameter Overrides

| Retriever | `top_k` | `system_prompt` | `wide_search_top_k` | `triplet_distance_penalty` | `node_type`/`node_name` | Other |
|---|---|---|---|---|---|---|
| `ChunksRetriever` | YES | -- | -- | -- | -- | -- |
| `SummariesRetriever` | YES | -- | -- | -- | -- | -- |
| `CompletionRetriever` | YES | YES | -- | -- | -- | -- |
| `TripletRetriever` | YES | YES | -- | -- | -- | -- |
| `GraphCompletionRetriever` | YES | YES | YES | YES | future (Task 17) | -- |
| `GraphSummaryCompletionRetriever` | YES | YES | YES | YES | future (Task 17) | -- |
| `GraphCompletionContextExtensionRetriever` | YES | YES | YES | YES | future (Task 17) | `context_extension_rounds` |
| `GraphCompletionCotRetriever` | YES | YES | YES | YES | future (Task 17) | `max_iter` |
| `TemporalRetriever` | YES | YES | YES | YES | future (Task 17) | -- |
| `LexicalRetriever` / `JaccardChunksRetriever` | YES | -- | -- | -- | -- | -- |
| `CypherSearchRetriever` | -- | -- | -- | -- | -- | -- |
| `NaturalLanguageRetriever` | -- | -- | -- | -- | -- | -- |
| `FeelingLuckyRetriever` | -- | -- | -- | -- | -- | forwards to delegate |
| `FeedbackRetriever` | -- | -- | -- | -- | -- | -- |
| `CodingRulesRetriever` | -- | -- | -- | -- | `node_name` (future) | -- |

### Parameters That Need Flowing Through

| `SearchRequest` field | Maps to `SearchParams` field | Currently used? |
|---|---|---|
| `top_k` | `top_k` | NO -- silently ignored |
| `system_prompt` | `system_prompt` | NO -- silently ignored |
| `system_prompt_path` | `system_prompt_path` | NO -- silently ignored |
| `wide_search_top_k` | `wide_search_top_k` | NO -- silently ignored |
| `triplet_distance_penalty` | `triplet_distance_penalty` | NO -- silently ignored |
| `node_type` | `node_type` | NO -- silently ignored (also Task 17) |
| `node_name` | `node_name` | NO -- silently ignored (also Task 17) |

### Files Changed

Total: ~16 files modified, 1 new file created.

**New file:**
- `crates/search/src/types/search_params.rs`

**Modified files:**
- `crates/search/src/types/mod.rs` -- add module + re-export
- `crates/search/src/retrievers/base_retriever.rs` -- trait signature change
- `crates/search/src/orchestration/search_orchestrator.rs` -- build `SearchParams`, pass to retriever, update tests
- `crates/search/src/orchestration/search_execution_builder.rs` -- update test impls
- `crates/search/src/retrievers/chunks_retriever.rs` -- use `params.top_k`
- `crates/search/src/retrievers/summaries_retriever.rs` -- use `params.top_k`
- `crates/search/src/retrievers/completion_retriever.rs` -- use `params.top_k` + `params.system_prompt`
- `crates/search/src/retrievers/triplet_retriever.rs` -- use `params.top_k` + `params.system_prompt`
- `crates/search/src/retrievers/graph_completion_retriever.rs` -- use all graph params
- `crates/search/src/retrievers/advanced_graph_retrievers.rs` -- update `GraphRetrieverCore` + all 3 retrievers
- `crates/search/src/retrievers/temporal_retriever.rs` -- use all graph params
- `crates/search/src/retrievers/lexical_retriever.rs` -- use `params.top_k`
- `crates/search/src/retrievers/cypher_nl_retrievers.rs` -- trait signature only
- `crates/search/src/retrievers/lucky_feedback_rules_retrievers.rs` -- trait signature + forwarding

## Test Verification

1. **Existing tests pass** -- All current tests should compile and pass after mechanical `params: &SearchParams` / `params: &SearchParams::default()` updates.
2. **New: per-request `top_k` override** (Step 21) -- Verify `SearchParams.top_k` overrides constructor default in `ChunksRetriever`.
3. **New: per-request `system_prompt` override** (Step 22) -- Verify `SearchParams.system_prompt` overrides constructor default in `CompletionRetriever`.
4. **New: orchestrator end-to-end param flow** (Step 23) -- Verify `SearchRequest.top_k` reaches the retriever as `SearchParams.top_k`.
5. **New: `SearchParams::default()` produces all `None`** -- Verify that the default params don't interfere with constructor defaults.
6. **Run `scripts/check_all.sh`** -- Verify formatting, compilation, clippy, and all wrapper binding checks pass.

## Dependencies

- **None blocking.** This task is self-contained and can be implemented independently.
- **Task 17 (node filtering)** will benefit from `SearchParams` already having `node_type`, `node_name`, and `node_name_filter_operator` fields, but is not required before or after this task.
- **Task 20 (retriever-specific config)** could eventually extend `SearchParams` with a `HashMap<String, Value>` for retriever-specific config, complementing the typed fields.
- **Task 25 (response model support)** may add a `response_model` field to `SearchParams` once structured output is supported.
