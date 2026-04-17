# Phase 9 — `TemporalEntityEnricher` unit tests

**Status:** Not Started

**Gap:** The `TemporalEntityEnricher` (cognify/src/temporal_extraction/entity_enricher.rs) has zero unit tests.

**Target file:** `crates/cognify/src/temporal_extraction/entity_enricher.rs` (new `#[cfg(test)] mod tests`)

---

## Test 9.1 — `enrich` populates attributes from LLM response

```
Scenario:
  - Input: vec![TemporalEvent { name: "Project Launch", attributes: vec![], ... }]
  - Mock LLM returns:
    [{ "event_name": "Project Launch", "attributes": [
      { "entity": "Acme Corp", "entity_type": "Organization", "relationship": "organizer" }
    ]}]
Expected:
  - Returns vec![event] where event.attributes.len() == 1
  - event.attributes[0].entity == "Acme Corp"
  - event.attributes[0].entity_type == "Organization"
  - event.attributes[0].relationship == "organizer"
```

## Test 9.2 — `enrich` returns original events unchanged on LLM error

```
Scenario:
  - Input: vec![TemporalEvent { name: "Launch", attributes: vec![], ... }]
  - Mock LLM returns error
Expected:
  - Returns Ok(vec![event]) with original events unchanged (attributes still empty)
  - No error propagated
```

The Rust code at entity_enricher.rs:59-62: `Err(e) => { tracing::warn!(...); return Ok(events); }`.

## Test 9.3 — `enrich` matches by event name (case-sensitive)

```
Scenario:
  - Input: 2 events: "Launch" and "Merger"
  - Mock LLM returns enrichment only for "Launch" (no entry for "Merger")
Expected:
  - "Launch" gets attributes populated
  - "Merger" attributes remain empty
```

## Test 9.4 — `enrich` with empty event list

```
Scenario:
  - Input: vec![]
Expected:
  - Returns Ok(vec![])
  - LLM is still called (with empty JSON array) but result is empty
```
