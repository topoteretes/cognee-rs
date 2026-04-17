# Phase 5 — `TemporalRetriever` unit tests: `rank_temporal_events`

**Status:** Not Started

**Gap:** Python has 5 `filter_top_k_events` tests (`temporal_retriever_test.py:54-148`). Rust's `rank_temporal_events` combines graph + vector scores and has zero tests.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

Note: `rank_temporal_events` is an `async fn` on `TemporalRetriever`, not `pub` -- but can be tested within the module's `#[cfg(test)]`.

---

## Test 5.1 — Ranking sorts by combined score and limits to top_k

```
Scenario:
  - event_ids: {"event-1", "event-2", "event-3"}
  - ranked_edges: edges with varying scores referencing different events
  - vector_db returns search results with scores: event-1=0.9, event-2=0.5, event-3=0.1
  - top_k = 2
Expected:
  - Returns 3 ranked events (rank_temporal_events doesn't limit -- limiting is done in get_context)
  - Sorted by ascending combined score (lower = better)
  - event-3 (lowest vector distance) ranked first
```

**Python reference:** `test_filter_top_k_events_sorts_and_limits` at `temporal_retriever_test.py:54-78`. Asserts ordered by score, limited to `top_k`.

## Test 5.2 — Events not in vector results get high (infinity-like) score

```
Scenario:
  - event_ids: {"event-1", "event-2", "event-unknown"}
  - vector_db returns results only for event-1 and event-2
  - ranked_edges: no edges involving event-unknown
Expected:
  - event-unknown gets a high default score (no vector match, no graph edge match)
  - event-1 and event-2 ranked above event-unknown
```

**Python reference:** `test_filter_top_k_events_includes_unknown_as_infinite_but_not_in_top_k` at `temporal_retriever_test.py:82-103`. Unknown events get `float('inf')` score and appear last.

## Test 5.3 — Empty vector search results

```
Scenario:
  - event_ids: {"event-1", "event-2"}
  - vector_db returns empty search results
  - ranked_edges: contains edges for both events
Expected:
  - All events scored only by graph edge presence
  - Both appear in result (no crash on empty vector results)
```

**Python reference:** `test_filter_top_k_events_handles_empty_scored_results` at `temporal_retriever_test.py:133-140`. Empty `scored_results`, all events get infinite score.

## Test 5.4 — Empty event_ids returns empty

```
Scenario:
  - event_ids: empty HashSet
  - ranked_edges: non-empty
Expected:
  - Returns empty Vec
```

## Test 5.5 — Error handling with malformed event data

```
Scenario:
  - event_ids: {"event-1"}
  - vector_db returns result with id that doesn't match any event
Expected:
  - event-1 still appears in result with default (high) score
  - No crash
```

**Python reference:** `test_filter_top_k_events_error_handling` at `temporal_retriever_test.py:144-148`. Passes malformed `[{}]`, asserts `KeyError` or `TypeError`. In Rust, we expect graceful handling instead of panic.
