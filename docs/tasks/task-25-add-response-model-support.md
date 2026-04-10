# Task 25: Add Pydantic-Style `response_model` Support (Structured LLM Output for Completions)

## Summary

In Python, every completion-generating retriever (CompletionRetriever, GraphCompletionRetriever, TripletRetriever, TemporalRetriever, GraphCompletionCotRetriever, GraphCompletionContextExtensionRetriever) accepts a `response_model` parameter (default `str`). When `response_model` is a Pydantic model class, the LLM is instructed to return structured JSON output matching that schema (via `LLMGateway.acreate_structured_output`). When `response_model=str`, the LLM returns plain text.

In Rust, the completion retrievers only call `llm.generate()` (plain text generation). There is no mechanism to request structured output from the completion path. This task adds a `response_schema` field to the Rust `CompletionRetriever` (and other completion-generating retrievers) that, when set, switches from `llm.generate()` to `llm.create_structured_output_with_messages_raw()` to produce structured JSON output.

## Current Rust Behavior

### CompletionRetriever -- plain text only

**File:** `crates/search/src/retrievers/completion_retriever.rs` (lines 20-28, 122-130)

```rust
pub struct CompletionRetriever {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    llm: Arc<dyn Llm>,
    top_k: usize,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
}

// In get_completion:
let completion = self.llm.generate(
    build_messages_with_history(system_prompt, user_prompt, session),
    self.generation_options.clone(),
).await?;
Ok(SearchOutput::Text(completion.content))
```

There is no `response_schema` or `response_model` field. The retriever always calls `llm.generate()` and wraps the result in `SearchOutput::Text`.

### Llm trait -- structured output is available but unused

**File:** `crates/llm/src/llm_trait.rs` (lines 54-58)

```rust
async fn create_structured_output_with_messages_raw(
    &self,
    messages: Vec<Message>,
    json_schema: &Value,
    options: Option<GenerationOptions>,
) -> LlmResult<Value>;
```

The `Llm` trait already supports structured output via JSON schema. The completion retrievers simply never call this method.

### SearchRequest -- no response_schema field

**File:** `crates/search/src/types/search_request.rs`

The `SearchRequest` has no field for passing a JSON schema. In Python, `response_model` is passed through `retriever_specific_config` (a dict), which is used when constructing the retriever instance per-request.

## Required Python Behavior

### CompletionRetriever with response_model

**File:** `/tmp/cognee-python/cognee/modules/retrieval/completion_retriever.py` (lines 22-37, 84-98)

```python
class CompletionRetriever(BaseRetriever):
    def __init__(self, ..., response_model: Type = str):
        self.response_model = response_model

    def _completion_kwargs(self, context: str) -> dict:
        return {
            "context": context,
            "user_prompt_path": self.user_prompt_path,
            "system_prompt_path": self.system_prompt_path,
            "system_prompt": self.system_prompt,
            "response_model": self.response_model,
        }
```

### generate_completion dispatches on response_model

**File:** `/tmp/cognee-python/cognee/modules/retrieval/utils/completion.py` (lines 9-34)

```python
async def generate_completion(query, context, ..., response_model: Type = str) -> Any:
    # ...
    result = await LLMGateway.acreate_structured_output(
        text_input=user_prompt,
        system_prompt=system_prompt,
        response_model=response_model,
    )
    return result
```

`LLMGateway.acreate_structured_output` handles both `str` (plain text) and Pydantic models (structured JSON). When `response_model=str`, it returns a plain string. When it is a Pydantic model, it returns a validated instance.

### Per-request retriever_specific_config

**File:** `/tmp/cognee-python/cognee/modules/search/methods/get_search_type_retriever_instance.py` (lines 79, 89, 105, etc.)

```python
"response_model": retriever_specific_config.get("response_model", str),
```

Python creates a new retriever instance per search request, passing `response_model` from the `retriever_specific_config` dict. This allows each request to specify a different output schema.

## Step-by-Step Changes

### Step 1: Add `response_schema` field to `CompletionRetriever`

In `crates/search/src/retrievers/completion_retriever.rs`, add an optional JSON schema field:

```rust
pub struct CompletionRetriever {
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    llm: Arc<dyn Llm>,
    top_k: usize,
    system_prompt: Option<String>,
    system_prompt_path: Option<String>,
    user_prompt_template: Option<String>,
    generation_options: Option<GenerationOptions>,
    /// Optional JSON schema for structured LLM output.
    /// When `Some`, uses `create_structured_output_with_messages_raw` instead of `generate`.
    /// When `None` (default), returns plain text like `response_model=str` in Python.
    response_schema: Option<serde_json::Value>,
}
```

Update the constructor to accept the new parameter:

```rust
impl CompletionRetriever {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vector_db: Arc<dyn VectorDB>,
        embedding_engine: Arc<dyn EmbeddingEngine>,
        llm: Arc<dyn Llm>,
        top_k: Option<usize>,
        system_prompt: Option<String>,
        system_prompt_path: Option<String>,
        user_prompt_template: Option<String>,
        generation_options: Option<GenerationOptions>,
        response_schema: Option<serde_json::Value>,
    ) -> Self {
        Self {
            vector_db,
            embedding_engine,
            llm,
            top_k: top_k.unwrap_or(DEFAULT_TOP_K),
            system_prompt,
            system_prompt_path,
            user_prompt_template,
            generation_options,
            response_schema,
        }
    }
}
```

### Step 2: Branch on `response_schema` in `get_completion`

In the `SearchRetriever` impl for `CompletionRetriever`, replace the current `generate` call with a branch:

```rust
async fn get_completion(
    &self,
    query: &str,
    context: Option<SearchContext>,
    session: &SessionContext,
) -> Result<SearchOutput, SearchError> {
    // ... existing context assembly code ...

    let messages = build_messages_with_history(system_prompt, user_prompt, session);

    if let Some(schema) = &self.response_schema {
        let structured_value = self
            .llm
            .create_structured_output_with_messages_raw(
                messages,
                schema,
                self.generation_options.clone(),
            )
            .await?;
        Ok(SearchOutput::Structured(structured_value))
    } else {
        let completion = self
            .llm
            .generate(messages, self.generation_options.clone())
            .await?;
        Ok(SearchOutput::Text(completion.content))
    }
}
```

### Step 3: Add `SearchOutput::Structured` variant

In `crates/search/src/types/`, add a `Structured` variant to the `SearchOutput` enum:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SearchOutput {
    Text(String),
    Items(SearchContext),
    Structured(serde_json::Value),
}
```

This allows the orchestrator and API layer to forward structured JSON results to the caller without deserializing them into a concrete Rust type.

### Step 4: Apply the same pattern to other completion-generating retrievers

Apply the same `response_schema: Option<serde_json::Value>` field and branching logic to:

- `GraphCompletionRetriever` (`crates/search/src/retrievers/graph_completion_retriever.rs`)
- `TripletRetriever` (`crates/search/src/retrievers/triplet_retriever.rs`)
- `GraphCompletionCotRetriever` (`crates/search/src/retrievers/advanced_graph_retrievers.rs`)
- `GraphCompletionContextExtensionRetriever` (`crates/search/src/retrievers/advanced_graph_retrievers.rs`)
- `GraphSummaryCompletionRetriever` (`crates/search/src/retrievers/advanced_graph_retrievers.rs`)

Each retriever's constructor gains `response_schema: Option<serde_json::Value>`, and the `get_completion` method branches on it.

### Step 5: Update `SearchBuilder` to pass `None` for `response_schema`

In `crates/search/src/orchestration/search_execution_builder.rs`, update each retriever construction call to pass `None` as the `response_schema` parameter. This preserves the existing default behavior (plain text).

### Step 6: Add `response_schema` to `SearchRequest`

In `crates/search/src/types/search_request.rs`, add:

```rust
/// Optional JSON schema for structured LLM output.
/// When present, completion-generating retrievers return structured JSON
/// matching this schema instead of plain text.
/// Equivalent to Python's `retriever_specific_config["response_model"]`.
pub response_schema: Option<serde_json::Value>,
```

### Step 7: Pass `response_schema` from orchestrator to retriever

This connects to Task 26 (per-request parameter passing). Either:
- (a) Extend the `SearchRetriever` trait's `get_completion` to accept `&SearchRequest`, or
- (b) The `SearchOrchestrator` reconstructs the retriever per-request with the `response_schema` from the request.

For now, in this task, the `response_schema` is set at retriever construction time via `SearchBuilder`. Dynamic per-request `response_schema` is deferred to Task 26.

### Step 8: Update all call sites passing `None` to existing constructors

Since the existing constructors are extended with a new parameter, update all call sites (tests, examples, builder) to pass `None` for the new `response_schema` parameter.

## Test Verification

1. **Existing tests**: All existing `CompletionRetriever` tests pass `response_schema: None` and continue to use `SearchOutput::Text`. No behavior change.

2. **New test: `returns_structured_output_when_response_schema_is_set`**:

```rust
#[tokio::test]
async fn returns_structured_output_when_response_schema_is_set() {
    let schema = json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" },
            "confidence": { "type": "number" }
        },
        "required": ["answer", "confidence"]
    });

    // TestLlm that returns structured JSON from create_structured_output_with_messages_raw
    struct StructuredTestLlm;
    // ... implement Llm with create_structured_output_with_messages_raw returning json!({...}) ...

    let retriever = CompletionRetriever::new(
        // ... vector_db, embedding_engine, ...
        Arc::new(StructuredTestLlm),
        Some(2),
        None, None, None, None,
        Some(schema),
    );

    let output = retriever
        .get_completion("what?", Some(provided_context), &SessionContext::default())
        .await
        .unwrap();

    match output {
        SearchOutput::Structured(value) => {
            assert!(value.get("answer").is_some());
            assert!(value.get("confidence").is_some());
        }
        _ => panic!("expected structured output"),
    }
}
```

3. **New test: `returns_text_output_when_response_schema_is_none`**: Confirms the existing behavior is preserved.

4. Run `cargo check --all-targets` and `scripts/check_all.sh`.

## Dependencies

- **`cognee-llm`**: Already provides `Llm::create_structured_output_with_messages_raw`. No changes needed.
- **`serde_json`**: Already a dependency.
- **Task 26**: Full per-request `response_schema` passing requires the trait or orchestrator changes from Task 26. This task adds the field and branching logic; Task 26 wires it up dynamically.
- **No new crate dependencies.**
