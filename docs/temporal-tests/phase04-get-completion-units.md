# Phase 4 — `TemporalRetriever` unit tests: `get_completion`

**Status:** Not Started

**Gap:** Python has 5 `get_completion` tests (`temporal_retriever_test.py:350-600`). Rust has zero `get_completion` tests.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

---

## Test 4.1 — `get_completion` generates text from context

```
Scenario:
  - Build retriever with TestLlm { completion_response: "Generated answer", ... }
  - Provide context: Some(vec![SearchItem { payload: json!({"event_id": "e1", "event_name": "Launch", "event_description": "Big launch" }), ... }])
  - session: SessionContext::default() (no session)
  - params: SearchParams::default()
  - Call: retriever.get_completion("What happened?", context, &session, &params)
Expected:
  - Ok(SearchOutput::Text("Generated answer"))
```

**Python reference:** `test_get_completion_without_context` at `temporal_retriever_test.py:350-398`. Mocks `generate_completion` -> `"Generated answer"`, asserts returns `["Generated answer"]`.

## Test 4.2 — `get_completion` with provided context (bypasses get_context)

```
Scenario:
  - Same as 4.1, but provide explicit context
  - Verify: LLM's generate() is called (check last_messages contains user prompt with context text)
Expected:
  - LLM receives messages containing the temporal context text
  - Returns the completion response
```

**Python reference:** `test_get_completion_with_provided_context` at `temporal_retriever_test.py:401-428`. Passes `context="Provided context"`, asserts `generate_completion` called once.

## Test 4.3 — `get_completion` without context calls `get_context` internally

```
Scenario:
  - Build retriever with graph containing temporal events (same setup as existing test 1)
  - context: None (forces internal get_context call)
  - Call: retriever.get_completion("What happened in 2024?", None, &session, &params)
Expected:
  - Ok(SearchOutput::Text(..)) -- completion succeeds after internally fetching context
```

**Python reference:** `test_get_completion_without_context` at `temporal_retriever_test.py:350-398` calls `get_completion(query=query)` without explicit context.

## Test 4.4 — `get_completion` with response_schema (structured output)

```
Scenario:
  - params: SearchParams { response_schema: Some(json!({"type": "object", "properties": {"answer": {"type": "string"}}})), ..Default::default() }
  - TestLlm configured to return structured JSON
  - Call: retriever.get_completion("What happened?", Some(context), &session, &params)
Expected:
  - Ok(SearchOutput::Structured(Value)) -- returns JSON value, not text
```

**Python reference:** `test_get_completion_with_response_model` at `temporal_retriever_test.py:547-600`. Passes `response_model=TestModel` Pydantic class, asserts result is `TestModel` instance.

## Test 4.5 — `get_completion` includes session history in messages

```
Scenario:
  - session: SessionContext with non-empty history (previous QA pairs)
  - Call: retriever.get_completion("Follow-up question", Some(context), &session, &params)
Expected:
  - Messages sent to LLM include session history entries
  - Verify via TestLlm.last_messages containing more than system + user messages
```

**Python reference:** `test_get_completion_with_session` at `temporal_retriever_test.py:431-490`. Creates `session_id="test_session"`, mocks session manager, verifies `generate_completion_with_session` called with `used_graph_element_ids`.
