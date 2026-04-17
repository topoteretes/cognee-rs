# Phase 3 — `TemporalRetriever` unit tests: `get_context` edge cases

**Status:** Not Started

**Gap:** Python has 5 `get_context` tests covering time ranges, partial bounds, no events, and fallback. Rust has only 2 tests (full interval match + extraction failure). Missing: partial time bounds, empty timestamp results, empty graph.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

---

## Test 3.1 — `get_context` with time_from only (open-ended upper bound)

```
Scenario:
  - Graph: 3 Timestamp nodes (2020, 2024-Jan, 2024-Jul), 3 Event nodes connected via "at" edges
  - LLM returns: QueryInterval { starts_at: Some("2024-01-01"), ends_at: None }
  - Query: "What happened after 2024?"
Expected:
  - Context contains 2024-Jan and 2024-Jul events (both >= 2024-01-01)
  - 2020 event excluded
```

**Python reference:** `test_get_context_time_from_only` at `temporal_retriever_test.py:284-314`. Mocks `extract_time_from_query` to return `("2024-01-01", None)`, asserts context contains "Event 1".

## Test 3.2 — `get_context` with time_to only (open-ended lower bound)

```
Scenario:
  - Graph: same 3 Timestamp nodes and Events as Test 3.1
  - LLM returns: QueryInterval { starts_at: None, ends_at: Some("2021-12-31") }
  - Query: "What happened before 2022?"
Expected:
  - Context contains the 2020 event
  - 2024 events excluded
```

**Python reference:** `test_get_context_time_to_only` at `temporal_retriever_test.py:317-347`. Returns `(None, "2024-12-31")`, asserts context contains "Event 1".

## Test 3.3 — `get_context` falls back when timestamps exist but no events match

```
Scenario:
  - Graph: 2 Timestamp nodes at 2020 and 2021, 2 Events connected, plus 2 non-temporal entity nodes with edges
  - LLM returns: QueryInterval { starts_at: Some("2030"), ends_at: Some("2031") }
  - Query: "What happened in 2030?"
Expected:
  - No Timestamp nodes in range -> event_node_ids is empty
  - Falls back to graph triplet search
  - Context items have "relationship" payload field (not "event_id")
```

**Python reference:** `test_get_context_no_events_found` at `temporal_retriever_test.py:249-281`. Mocks `collect_time_ids.return_value = []`, asserts fallback to `get_triplets` is called.

## Test 3.4 — `get_context` on empty graph

```
Scenario:
  - Graph: no nodes, no edges (graph_db.is_empty() == true)
  - Query: "What happened?"
Expected:
  - Returns Ok(vec![])  (empty context, no error)
```

**Python reference:** `test_temporal_retriever_context_empty_graph` at integration test `test_temporal_retriever.py:249-260`. Asserts `len(context) >= 0`.

Rust implementation already has this check at line 339: `if self.graph_db.is_empty().await? { return Ok(vec![]); }`. Test confirms this path.

## Test 3.5 — `get_context` respects `top_k` parameter from `SearchParams`

```
Scenario:
  - Graph: 5 Event nodes each connected to distinct Timestamp nodes all within range
  - LLM returns valid interval covering all timestamps
  - SearchParams { top_k: Some(2), ..Default::default() }
  - Query: "What happened in 2024?"
Expected:
  - Context contains exactly 2 items (not 5)
```

**Python reference:** `test_temporal_retriever_top_k_limit` at `test_temporal_retriever.py:303-315`. Creates retriever with `top_k=2`, asserts `context.count("#####################") <= 1` (separator count = events - 1).

## Test 3.6 — `get_context` with `Event -[during]-> Interval -[from/to]-> Timestamp` (2-hop path)

```
Scenario:
  - Graph:
    - Timestamp T1 (time_at=2024-02-01 ms), Timestamp T2 (time_at=2024-03-01 ms)
    - Interval node I1
    - Event E1 "Team Meeting"
    - Edges: E1 -[during]-> I1, I1 -[from]-> T1, I1 -[to]-> T2
  - LLM returns: QueryInterval { starts_at: Some("2024-02"), ends_at: Some("2024-03") }
  - Query: "What happened in Feb-Mar 2024?"
Expected:
  - E1 is found via 2-hop traversal: Timestamp T1 -> neighbor Interval I1 -> neighbor Event E1
  - Context contains event "Team Meeting"
```

**Python reference:** Integration test fixture in `test_temporal_retriever.py:96-101` creates `event2` with `during=interval1` where `interval1 = Interval(time_from=timestamp2, time_to=timestamp3)`. Test `test_temporal_retriever_context_with_time_range` at line 196 queries "What happened in January 2021?".

This is the **only path that exercises the 2-hop Interval traversal** in the Rust `get_context()` implementation (lines 378-389).
