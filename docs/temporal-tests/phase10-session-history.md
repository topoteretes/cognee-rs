# Phase 10 — Session/conversation history with temporal search

**Status:** Not Started

**Gap:** Python `test_conversation_history.py:278-297` validates that temporal search results are stored in session history and `used_graph_element_ids` has the correct shape. Rust has no session tracking test for temporal search.

**Target file:** New file `crates/search/tests/temporal_session.rs` or add to existing `crates/search/tests/integration_search_matrix.rs`.

---

## Test 10.1 — Temporal search stores QA pair in session

```
Scenario:
  - Full pipeline: add data -> cognify (temporal) -> search with SearchType::Temporal
  - Provide session_id in SessionContext
  - Query: "Tell me about the events"
Expected:
  - Search returns non-empty result
  - SessionStore contains a QA entry with the question
  - QA entry has used_graph_element_ids as a dict (or None)
```

**Python reference:** `test_conversation_history.py:278-297`:
```python
result_temporal = await cognee.search(
    query_type=SearchType.TEMPORAL,
    query_text="Tell me about the companies",
    session_id=session_id_temporal,
)
assert isinstance(result_temporal, list) and len(result_temporal) > 0

history_temporal = await cache_engine.get_latest_qa(str(user.id), session_id_temporal, last_n=10)
our_qa_temporal = [h for h in history_temporal if h["question"] == "Tell me about the companies"]
assert len(our_qa_temporal) == 1
_assert_used_graph_element_ids_shape(our_qa_temporal[0])
```

The `_assert_used_graph_element_ids_shape` helper (lines 25-41) validates:
- Key `used_graph_element_ids` exists
- Value is `None` or a `dict` with keys `<= {"node_ids", "edge_ids"}`
- Each value is a `list` of `str`

## Test 10.2 — Multiple temporal queries create separate history entries

```
Scenario:
  - Same session_id, two queries: "What happened in 2021?" and "What happened in 2024?"
Expected:
  - Session history contains 2 distinct QA entries
```
