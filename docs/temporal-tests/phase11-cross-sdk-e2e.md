# Phase 11 — Strengthen cross-SDK E2E assertions

**Status:** Not Started

**Gap:** Current cross-SDK tests (`e2e-cross-sdk/harness/test_temporal_search.py`) assert `>=1` for Event and Timestamp node counts. Python E2E (`test_temporal_graph.py`) asserts `>=10`. The biography fixture contains >10 date-anchored events.

**Target file:** `e2e-cross-sdk/harness/test_temporal_search.py`

---

## Task 11.1 — Raise minimum Event node threshold

```
Current:  assert len(rust_events) >= 1
Proposed: assert len(rust_events) >= 5
```

**Python reference:** `test_temporal_graph.py:109`:
```python
assert type_counts.get("Event", 0) >= 10
```

Use `>=5` as a conservative threshold (accounting for LLM variance across providers).

## Task 11.2 — Raise minimum Timestamp node threshold

```
Current:  assert len(rust_ts) >= 1
Proposed: assert len(rust_ts) >= 5
```

**Python reference:** `test_temporal_graph.py:113`:
```python
assert type_counts.get("Timestamp", 0) >= 10
```

## Task 11.3 — Add edge type validation

```
New test: test_temporal_cognify_produces_temporal_edges

Scenario:
  - After temporal cognify on biography text
  - Query graph edges
Expected:
  - At least 5 edges with relationship "at" or "during"
  - Every Event node has at least one "at" or "during" outgoing edge
```

**Python reference:** `test_temporal_graph.py:116-122`:
```python
assert edge_type_counts.get("contains", 0) >= 10
assert edge_type_counts.get("is_a", 0) >= 10
```

(Python checks `contains`/`is_a`; for temporal-specific, check `at`/`during`.)

## Task 11.4 — Add Python search parity test

```
New test: test_temporal_search_parity_both_sdks_return_non_empty

Scenario:
  - Run TEMPORAL search on both Python and Rust CLIs with same query
  - Compare: both return non-empty results
```

**Python reference:** `test_search_db.py:253-257` runs `cognee.search(query_type=SearchType.TEMPORAL)` and verifies non-empty.
