# Phase 8 — `TemporalEventExtractor` unit tests

**Status:** Not Started

**Gap:** The `TemporalEventExtractor` (cognify/src/temporal_extraction/event_extractor.rs) has zero unit tests. Python's event extraction tests are in `test_temporal_graph.py` (integration) and `extract_events_and_entities.py` (task-level).

**Target file:** `crates/cognify/src/temporal_extraction/event_extractor.rs` (new `#[cfg(test)] mod tests`)

---

## Test 8.1 — `extract_events` happy path with mock LLM

```
Scenario:
  - Mock LLM returns structured output:
    [
      { "name": "Project Launch", "description": "Launched in 2024", "time_from": {"year": 2024, "month": 3}, "time_to": null, "location": "NYC" },
      { "name": "Merger", "description": "Company merged", "time_from": {"year": 2024, "month": 6}, "time_to": {"year": 2024, "month": 9}, "location": null }
    ]
  - Input: "The company launched in March 2024. It merged between June and September 2024."
Expected:
  - Returns Ok(vec![event1, event2])
  - event1.name == "Project Launch", event1.at == Some(CognifyTimestamp for 2024-03-01)
  - event2.name == "Merger", event2.during == Some(CognifyInterval { time_from: 2024-06-01, time_to: 2024-09-01 })
```

**Python reference:** Event extraction is tested indirectly in `test_temporal_graph.py:76-122` which asserts `>=10 Event nodes` and `>=10 Timestamp nodes` after cognify.

## Test 8.2 — `extract_events` returns empty Vec on LLM error

```
Scenario:
  - Mock LLM returns error from create_structured_output
  - Input: "Some text"
Expected:
  - Returns Ok(vec![])  (does NOT propagate error)
```

The Rust code at event_extractor.rs:46-48 shows: `Err(e) => { tracing::warn!(...); return Ok(vec![]); }`.

## Test 8.3 — `extract_events` filters out empty-name events

```
Scenario:
  - Mock LLM returns: [{ "name": "", "description": "Unnamed" }, { "name": "Real Event", ... }]
Expected:
  - Returns vec![event] with only "Real Event" (empty name filtered)
```

The Rust code at event_extractor.rs:55: `.filter(|raw| !raw.name.is_empty())`.

## Test 8.4 — `convert_raw_event` with point-in-time (at)

```
Scenario:
  - RawEvent { name: "Launch", time_from: Some(RawExtractedTimestamp { year: 2024, month: 1, day: 15 }), time_to: None, ... }
Expected:
  - TemporalEvent.at = Some(CognifyTimestamp for 2024-01-15)
  - TemporalEvent.during = None
```

## Test 8.5 — `convert_raw_event` with interval (during)

```
Scenario:
  - RawEvent { name: "Meeting", time_from: Some({ year: 2024, month: 2 }), time_to: Some({ year: 2024, month: 3 }), ... }
Expected:
  - TemporalEvent.at = None
  - TemporalEvent.during = Some(CognifyInterval { time_from: 2024-02-01, time_to: 2024-03-01 })
```

## Test 8.6 — `convert_raw_event` with invalid timestamp returns None

```
Scenario:
  - RawEvent { name: "Bad", time_from: Some({ year: 2024, month: 13, day: 1 }), time_to: None }
Expected:
  - convert_raw_event returns None (invalid month)
  - Event filtered out of final results
```
