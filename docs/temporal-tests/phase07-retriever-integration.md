# Phase 7 — `TemporalRetriever` integration tests with pre-populated events

**Status:** Not Started

**Gap:** Python has 10 integration tests (`test_temporal_retriever.py:196-336`) that create known events in real graph/vector DBs and query them. Rust has zero retriever-focused integration tests with pre-populated events -- existing tests only test the cognify pipeline.

**Target file:** New file `crates/search/tests/temporal_retriever_integration.rs`

**Test fixture:** Create a setup function that:
- Creates a `TempDir`
- Initializes `LadybugAdapter` (graph) + `QdrantAdapter` or `MockVectorDB` (vector)
- Pre-populates graph with known Event, Timestamp, and Interval nodes and edges
- Creates vector collection "Event"/"name" with embeddings for event names

**Event data** (ported from Python fixture at `test_temporal_retriever.py:15-127`):

| Event | Timestamp / Interval | time_at (ms) | Location |
|---|---|---|---|
| Project Alpha Launch | at: 2021-01-01T00:00:00Z | 1609459200000 | San Francisco |
| Team Meeting | during: 2021-02-01 -> 2021-03-01 | 1612137600000 -> 1614556800000 | New York |
| Product Release | at: 2021-07-01T00:00:00Z | 1625097600000 | Remote |
| Company Retreat | at: 2021-10-01T00:00:00Z | 1633046400000 | Lake Tahoe |

These tests **require a real LLM** (for `extract_interval` and `get_completion`). Skip if `OPENAI_URL`/`OPENAI_TOKEN` not set.

---

## Test 7.1 — Time range query retrieves correct event

```
Scenario:
  - Pre-populated 4 events as above
  - Query: "What happened in January 2021?"
  - Retriever top_k = 5
Expected:
  - Context is non-empty
  - Context contains "Project Alpha" (the Jan 2021 event)
```

**Python reference:** `test_temporal_retriever_context_with_time_range` at `test_temporal_retriever.py:196-210`.
```python
assert "Project Alpha" in context or "Launch" in context
```

## Test 7.2 — Single month query

```
Scenario:
  - Query: "What happened in July 2021?"
Expected:
  - Context contains "Product Release"
```

**Python reference:** `test_temporal_retriever_context_with_single_time` at `test_temporal_retriever.py:213-227`.
```python
assert "Product Release" in context or "July" in context
```

## Test 7.3 — Non-temporal query falls back to triplets

```
Scenario:
  - Pre-populate graph with non-temporal entity nodes (e.g. Company "Figma", Person "Steve") connected by "works_for" edge
  - No temporal events
  - Query: "Who works at Figma?"
Expected:
  - Context is non-empty
  - Context contains "Steve" or "Figma"
```

**Python reference:** `test_temporal_retriever_context_fallback_to_triplets` at `test_temporal_retriever.py:230-246`.
```python
assert "Steve" in context or "Figma" in context
```

## Test 7.4 — Full completion pipeline

```
Scenario:
  - Pre-populated 4 events
  - Query: "What happened in January 2021?"
  - Call get_completion (with no pre-built context, forces internal get_context)
Expected:
  - Returns SearchOutput::Text with non-empty string
```

**Python reference:** `test_temporal_retriever_get_completion` at `test_temporal_retriever.py:263-280`.
```python
assert isinstance(completion, list) and len(completion) > 0
assert all(isinstance(item, str) and item.strip() for item in completion)
```

## Test 7.5 — Completion fallback on non-temporal data

```
Scenario:
  - Pre-populate graph with entity nodes only (no temporal events)
  - Query: "Who works at Figma?"
  - Call get_completion
Expected:
  - Returns non-empty text (LLM generates answer from triplet context)
```

**Python reference:** `test_temporal_retriever_get_completion_fallback` at `test_temporal_retriever.py:283-300`.

## Test 7.6 — `top_k` limits results

```
Scenario:
  - Pre-populated 4 events, all in year 2021
  - Retriever top_k = 2
  - Query: "What happened in 2021?"
Expected:
  - Context contains at most 2 event items
```

**Python reference:** `test_temporal_retriever_top_k_limit` at `test_temporal_retriever.py:303-315`.
```python
assert context.count("#####################") <= 1  # separator count = events - 1
```

## Test 7.7 — Multiple events retrieved

```
Scenario:
  - Pre-populated 4 events, all in year 2021
  - Retriever top_k = 10
  - Query: "What events occurred in 2021?"
Expected:
  - Context is non-empty
  - Contains at least one of: "Project Alpha", "Team Meeting", "Product Release", "Company Retreat"
```

**Python reference:** `test_temporal_retriever_multiple_events` at `test_temporal_retriever.py:318-336`.
```python
assert ("Project Alpha" in context or "Team Meeting" in context or
        "Product Release" in context or "Company Retreat" in context)
```
