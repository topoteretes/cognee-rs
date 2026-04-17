# Phase 6 — `TemporalRetriever` unit tests: `temporal_context_to_text`

**Status:** Not Started

**Gap:** Python's `descriptions_to_string` is tested in 2 unit tests (`temporal_retriever_test.py:36-116`). Rust's `temporal_context_to_text` has zero tests.

**Target file:** `crates/search/src/retrievers/temporal_retriever.rs` (inside `mod tests`)

---

## Test 6.1 — Formats event items correctly

```
Scenario:
  context: vec![
    SearchItem { payload: json!({"event_id": "e1", "event_name": "Launch", "event_description": "Big launch", "event_time": "2024-01-15"}), score: Some(0.5), id: None },
    SearchItem { payload: json!({"event_id": "e2", "event_name": "Release", "event_description": "Product release", "event_time": "2024-07-01"}), score: Some(0.8), id: None },
  ]
Expected:
  "Launch (2024-01-15): Big launch\nRelease (2024-07-01): Product release"
```

**Python reference:** `test_descriptions_to_string_basic_and_empty` at `temporal_retriever_test.py:36-50`. Asserts descriptions joined by `"#####################"` separator. Rust uses `\n` instead of separator.

## Test 6.2 — Formats fallback (non-event) items as triplets

```
Scenario:
  context: vec![
    SearchItem { payload: json!({"source_name": "Einstein", "target_name": "Physics", "relationship": "contributed_to"}), score: Some(0.3), id: None },
  ]
Expected:
  "Einstein -[contributed_to]-> Physics"
```

## Test 6.3 — Empty context produces empty string

```
Scenario:
  context: vec![]
Expected:
  ""
```

**Python reference:** `test_descriptions_to_string_basic_and_empty` at line 48: `assert descriptions_to_string([]) == ""`.

## Test 6.4 — Missing fields use defaults

```
Scenario:
  context: vec![
    SearchItem { payload: json!({"event_id": "e1"}), score: None, id: None },
  ]
Expected:
  "Unnamed event (unknown time): No description"
  -- verifies unwrap_or defaults for missing event_name, event_description, event_time
```

## Test 6.5 — Mixed event and triplet items

```
Scenario:
  context: vec![
    SearchItem with event_id payload (event item),
    SearchItem without event_id (triplet item),
  ]
Expected:
  Two lines: first formatted as event, second formatted as triplet
```
