# Phase 2 — `TemporalRetriever` unit tests: `extract_interval`

**Status:** Not Started

**Gap:** Python has 3 tests for `extract_time_from_query` (`temporal_retriever_test.py:603-701`). Rust has zero isolated tests for `extract_interval`.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

All tests reuse the existing `TestLlm` mock. The `interval_response` field on `TestLlm` controls what the LLM returns.

---

## Test 2.1 — `extract_interval` returns parsed interval from LLM

```
Scenario:
  - TestLlm.interval_response = Some(QueryInterval { starts_at: Some("2024-01-01"), ends_at: Some("2024-12-31") })
  - TestLlm.fail_structured_output = false
  - Call: retriever.extract_interval("What happened in 2024?")
Expected:
  - Ok(Some(ParsedInterval { start: 2024-01-01T00:00:00Z, end: 2024-12-31T23:59:59Z }))
```

**Python reference:** `test_extract_time_from_query_relative_path` at `temporal_retriever_test.py:603-631`. Mocks `LLMGateway.acreate_structured_output` to return `QueryInterval(starts_at=Timestamp(year=2024,month=1,day=1), ends_at=Timestamp(year=2024,month=12,day=31))`.

## Test 2.2 — `extract_interval` returns None when LLM returns None/None

```
Scenario:
  - TestLlm.interval_response = Some(QueryInterval { starts_at: None, ends_at: None })
  - Call: retriever.extract_interval("Who is Einstein?")
Expected:
  - Ok(None)
```

**Python reference:** `test_extract_time_from_query_with_none_values` at `temporal_retriever_test.py:675-701`. Asserts both return values are `None`.

## Test 2.3 — `extract_interval` returns None when LLM fails

```
Scenario:
  - TestLlm.fail_structured_output = true
  - Call: retriever.extract_interval("What happened?")
Expected:
  - Ok(None)  (error swallowed, returns None gracefully)
```

**Python reference:** Implicit in fallback tests; the Rust `extract_interval` returns `Ok(None)` on LLM error (line 130).

## Test 2.4 — `extract_interval` with only starts_at

```
Scenario:
  - TestLlm.interval_response = Some(QueryInterval { starts_at: Some("2024-01-01"), ends_at: None })
  - Call: retriever.extract_interval("What happened after 2024?")
Expected:
  - Ok(Some(ParsedInterval { start: Some(2024-01-01T00:00:00Z), end: None }))
```

**Python reference:** `test_get_context_time_from_only` at `temporal_retriever_test.py:284-314` mocks `extract_time_from_query` to return `("2024-01-01", None)`.

## Test 2.5 — `extract_interval` with only ends_at

```
Scenario:
  - TestLlm.interval_response = Some(QueryInterval { starts_at: None, ends_at: Some("2024-12-31") })
  - Call: retriever.extract_interval("What happened before 2025?")
Expected:
  - Ok(Some(ParsedInterval { start: None, end: Some(2024-12-31T23:59:59Z) }))
```

**Python reference:** `test_get_context_time_to_only` at `temporal_retriever_test.py:317-347` mocks `extract_time_from_query` to return `(None, "2024-12-31")`.
