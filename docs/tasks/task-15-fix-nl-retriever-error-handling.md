# Task 15: Fix NL retriever LLM error handling -- Catch LLM errors in retry loop

## Summary

The Rust `NaturalLanguageRetriever::execute_nl_query()` uses `?` (early return) on the `generate_cypher_query()` call inside the retry loop. If the LLM call fails (network timeout, rate limit, malformed response), the error propagates immediately and aborts all remaining retry attempts. The Python implementation wraps the entire attempt (both LLM generation and query execution) in a `try/except Exception` block, so LLM failures are logged as previous attempts and the loop continues to the next iteration.

## Current Rust Behavior

**File:** `crates/search/src/retrievers/cypher_nl_retrievers.rs`, lines 161-190

```rust
async fn execute_nl_query(&self, query: &str) -> Result<Vec<Vec<Value>>, SearchError> {
    let (_node_schemas, edge_schemas) = self.get_graph_schema().await?;
    let mut previous_attempts = String::new();

    for _ in 0..self.max_attempts {
        let cypher_query = self
            .generate_cypher_query(query, &edge_schemas, &previous_attempts)
            .await?;                    // <-- BUG: `?` propagates LLM errors immediately

        if cypher_query.is_empty() {
            previous_attempts.push_str("Query: <empty> -> Result: None\\n");
            continue;
        }

        match self.graph_db.query(&cypher_query, None).await {
            Ok(context) if !context.is_empty() => return Ok(context),
            Ok(_) => {
                previous_attempts
                    .push_str(&format!("Query: {cypher_query} -> Result: None\\n"));
            }
            Err(error) => {
                previous_attempts.push_str(&format!(
                    "Query: {cypher_query} -> Executed with error: {error}\\n"
                ));
            }
        }
    }

    Ok(vec![])
}
```

**The problem in detail:**

On line 168, `.await?` causes any error from `generate_cypher_query` (which calls `self.llm.generate()`) to immediately propagate as `Err(SearchError::LlmError(...))`, breaking out of the retry loop. This means:
- A transient LLM failure on attempt 1 of 3 aborts the entire search with an error.
- The graph query execution errors on line 181 are properly caught and added to `previous_attempts`, but LLM errors are not -- this is inconsistent.
- The `max_attempts` retry mechanism is effectively bypassed for LLM failures.

## Required Behavior (Python Reference)

**File:** `/tmp/cognee-python/cognee/modules/retrieval/natural_language_retriever.py`, lines 74-104

```python
for attempt in range(self.max_attempts):
    logger.info(f"Starting attempt {attempt + 1}/{self.max_attempts} for query generation")
    try:
        cypher_query = await self._generate_cypher_query(
            query, edge_schemas, previous_attempts
        )

        logger.info(
            f"Executing generated Cypher query (attempt {attempt + 1}): {cypher_query[:100]}..."
            if len(cypher_query) > 100
            else cypher_query
        )
        context = await graph_engine.query(cypher_query)

        if context:
            result_count = len(context) if isinstance(context, list) else 1
            logger.info(
                f"Successfully executed query (attempt {attempt + 1}): returned {result_count} result(s)"
            )
            return context

        previous_attempts += f"Query: {cypher_query} -> Result: None\n"

    except Exception as e:
        previous_attempts += f"Query: {cypher_query if 'cypher_query' in locals() else 'Not generated'} -> Executed with error: {e}\n"
        logger.error(f"Error executing query: {str(e)}")

logger.warning(
    f"Failed to get results after {self.max_attempts} attempts for query: '{query[:50]}...'"
)
return []
```

**Key difference:** The Python `except Exception as e` block on line 97 catches **all** exceptions, including LLM generation errors. When the LLM call fails:
1. The error is recorded in `previous_attempts` (with the cypher_query text or "Not generated" if generation itself failed).
2. The loop continues to the next attempt.
3. The next attempt's system prompt includes the error from the previous attempt, giving the LLM context to try differently.
4. Only after all `max_attempts` are exhausted does the method return an empty list (not an error).

## Step-by-Step Code Changes

### Change 1: Catch LLM errors inside the retry loop

**File:** `crates/search/src/retrievers/cypher_nl_retrievers.rs`

Replace lines 161-190:

**Old code:**
```rust
    async fn execute_nl_query(&self, query: &str) -> Result<Vec<Vec<Value>>, SearchError> {
        let (_node_schemas, edge_schemas) = self.get_graph_schema().await?;
        let mut previous_attempts = String::new();

        for _ in 0..self.max_attempts {
            let cypher_query = self
                .generate_cypher_query(query, &edge_schemas, &previous_attempts)
                .await?;

            if cypher_query.is_empty() {
                previous_attempts.push_str("Query: <empty> -> Result: None\\n");
                continue;
            }

            match self.graph_db.query(&cypher_query, None).await {
                Ok(context) if !context.is_empty() => return Ok(context),
                Ok(_) => {
                    previous_attempts
                        .push_str(&format!("Query: {cypher_query} -> Result: None\\n"));
                }
                Err(error) => {
                    previous_attempts.push_str(&format!(
                        "Query: {cypher_query} -> Executed with error: {error}\\n"
                    ));
                }
            }
        }

        Ok(vec![])
    }
```

**New code:**
```rust
    async fn execute_nl_query(&self, query: &str) -> Result<Vec<Vec<Value>>, SearchError> {
        let (_node_schemas, edge_schemas) = self.get_graph_schema().await?;
        let mut previous_attempts = String::new();

        for _ in 0..self.max_attempts {
            let cypher_query = match self
                .generate_cypher_query(query, &edge_schemas, &previous_attempts)
                .await
            {
                Ok(cq) => cq,
                Err(error) => {
                    previous_attempts.push_str(&format!(
                        "Query: Not generated -> Executed with error: {error}\\n"
                    ));
                    continue;
                }
            };

            if cypher_query.is_empty() {
                previous_attempts.push_str("Query: <empty> -> Result: None\\n");
                continue;
            }

            match self.graph_db.query(&cypher_query, None).await {
                Ok(context) if !context.is_empty() => return Ok(context),
                Ok(_) => {
                    previous_attempts
                        .push_str(&format!("Query: {cypher_query} -> Result: None\\n"));
                }
                Err(error) => {
                    previous_attempts.push_str(&format!(
                        "Query: {cypher_query} -> Executed with error: {error}\\n"
                    ));
                }
            }
        }

        Ok(vec![])
    }
```

The changes:
1. Replaced `.await?` with `match ... { Ok(cq) => cq, Err(error) => { ... continue; } }` on the `generate_cypher_query` call.
2. On LLM error, the error message is appended to `previous_attempts` with `"Query: Not generated"` (matching Python's `'Not generated'` fallback text) and the loop continues to the next attempt.
3. The graph schema fetch on line 162 still uses `?` -- this is intentional. If we cannot even get the schema, there is no point retrying query generation. This matches Python where `_get_graph_schema` is called before the retry loop and failures would propagate up.

## Test Verification

### Existing test coverage

**File:** `crates/search/src/retrievers/cypher_nl_retrievers.rs`, lines 457-494

The test `natural_language_retriever_retries_until_results` validates retries but only for **graph query returning empty results**, not for LLM errors. The `TestLlm` mock always returns `Ok(...)`.

### New test to add

Add the following test inside the existing `mod tests` block in `crates/search/src/retrievers/cypher_nl_retrievers.rs`. This requires a new mock LLM that can fail on specific attempts.

```rust
/// LLM mock that fails on the first N calls, then succeeds.
struct FailThenSucceedLlm {
    fail_count: Mutex<usize>,
    success_response: String,
}

impl FailThenSucceedLlm {
    fn new(fail_count: usize, success_response: &str) -> Self {
        Self {
            fail_count: Mutex::new(fail_count),
            success_response: success_response.to_string(),
        }
    }
}

#[async_trait]
impl Llm for FailThenSucceedLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        let mut remaining = self.fail_count.lock().unwrap(); // lock poison is unrecoverable
        if *remaining > 0 {
            *remaining -= 1;
            return Err(LlmError::ApiError("simulated LLM failure".to_string()));
        }
        Ok(GenerationResponse {
            content: self.success_response.clone(),
            model: "test".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            }),
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<serde_json::Value> {
        Err(LlmError::ConfigError("not implemented".to_string()))
    }

    fn model(&self) -> &str {
        "test"
    }
}

#[tokio::test]
async fn natural_language_retriever_retries_on_llm_error() {
    let graph_db = Arc::new(TestGraphDb {
        empty: false,
        rows_by_query: std::collections::HashMap::from([
            (
                "\n            MATCH (n)\n            UNWIND keys(n) AS prop\n            RETURN DISTINCT labels(n) AS NodeLabels, collect(DISTINCT prop) AS Properties;\n            "
                    .to_string(),
                vec![vec![json!(["Entity"]), json!(["name"])]],
            ),
            (
                "\n            MATCH ()-[r]->()\n            UNWIND keys(r) AS key\n            RETURN DISTINCT key;\n            "
                    .to_string(),
                vec![vec![json!("relationship")]],
            ),
            (
                "MATCH (n) WHERE n.name = 'Alice' RETURN n".to_string(),
                vec![vec![json!({"name": "Alice"})]],
            ),
        ]),
    });

    // LLM fails on first call, succeeds on second with a valid query
    let llm = Arc::new(FailThenSucceedLlm::new(
        1,
        "MATCH (n) WHERE n.name = 'Alice' RETURN n",
    ));

    let retriever = NaturalLanguageRetriever::new(graph_db, llm, Some(3), None);
    let output = retriever
        .get_completion("Find Alice", None, &SessionContext::default())
        .await
        .unwrap();

    match output {
        SearchOutput::GraphQueryRows(rows) => {
            assert_eq!(rows.len(), 1, "should return results after recovering from LLM error");
        }
        _ => panic!("expected graph query rows"),
    }
}

#[tokio::test]
async fn natural_language_retriever_returns_empty_when_all_llm_attempts_fail() {
    let graph_db = Arc::new(TestGraphDb {
        empty: false,
        rows_by_query: std::collections::HashMap::from([
            (
                "\n            MATCH (n)\n            UNWIND keys(n) AS prop\n            RETURN DISTINCT labels(n) AS NodeLabels, collect(DISTINCT prop) AS Properties;\n            "
                    .to_string(),
                vec![vec![json!(["Entity"]), json!(["name"])]],
            ),
            (
                "\n            MATCH ()-[r]->()\n            UNWIND keys(r) AS key\n            RETURN DISTINCT key;\n            "
                    .to_string(),
                vec![vec![json!("relationship")]],
            ),
        ]),
    });

    // LLM fails on all 3 attempts
    let llm = Arc::new(FailThenSucceedLlm::new(3, "should not reach this"));

    let retriever = NaturalLanguageRetriever::new(graph_db, llm, Some(3), None);
    let output = retriever
        .get_completion("Find Alice", None, &SessionContext::default())
        .await
        .unwrap();

    match output {
        SearchOutput::GraphQueryRows(rows) => {
            assert!(rows.is_empty(), "should return empty when all LLM attempts fail");
        }
        _ => panic!("expected graph query rows"),
    }
}
```

Note: You will need to add `LlmError` to the existing `use cognee_llm::{...}` import at the top of the test module (it is already imported -- see line 236).

### How to verify

```bash
cargo test -p cognee-search -- cypher_nl_retrievers::tests
```

All four tests should pass:
1. `cypher_retriever_returns_query_rows` -- existing, unchanged
2. `natural_language_retriever_retries_until_results` -- existing, unchanged
3. `natural_language_retriever_retries_on_llm_error` -- new, verifies recovery from transient LLM failure
4. `natural_language_retriever_returns_empty_when_all_llm_attempts_fail` -- new, verifies graceful degradation when LLM is completely unavailable

### Bug reproduction (before fix)

The test `natural_language_retriever_retries_on_llm_error` will **fail** on the current code with:

```
called `Result::unwrap()` on an `Err` value: LlmError("simulated LLM failure")
```

This confirms that the `?` operator causes the LLM error to propagate out of `execute_nl_query` and `get_context`, aborting the retry loop on the first failure. After the fix, the error is caught, the loop continues, and the second attempt succeeds.
