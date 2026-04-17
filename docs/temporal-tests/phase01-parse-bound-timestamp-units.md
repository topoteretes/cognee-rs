# Phase 1 — `parse_bound` / `to_cognify_timestamp` unit tests

**Status:** Not Started

**Gap:** Python's `TemporalRetriever` and `Timestamp` model are tested across various date formats. Rust's `parse_bound()` (temporal_retriever.rs:514-565), `is_within_interval_ms()` (line 510), and `to_cognify_timestamp()` (temporal_event.rs:86-104) have zero unit tests.

**Target files:**
- `crates/search/src/retrievers/temporal_retriever.rs` (inside existing `#[cfg(test)] mod tests` at line 577)
- `crates/models/src/temporal_event.rs` (new `#[cfg(test)] mod tests` block)

**Python reference:** `test_extract_time_from_query_with_none_values` in `temporal_retriever_test.py:675-701` tests the `None, None` case. Time extraction prompt parsing with year ranges comes from `test_temporal_graph.py:126-129` which calls `extract_time_from_query("What happened between 1890 and 1900?")`.

---

## Test 1.1 — `parse_bound` full ISO-8601 datetime

```
Scenario: parse_bound("2024-01-15 10:30:00", false)
Expected: Some(DateTime<Utc> for 2024-01-15T10:30:00Z)
```

## Test 1.2 — `parse_bound` RFC-3339

```
Scenario: parse_bound("2024-01-15T10:30:00Z", false)
Expected: Some(DateTime<Utc> for 2024-01-15T10:30:00Z)
```

## Test 1.3 — `parse_bound` date only, start of day

```
Scenario: parse_bound("2024-03-15", false)
Expected: Some(DateTime<Utc> for 2024-03-15T00:00:00Z)
```

## Test 1.4 — `parse_bound` date only, end of day

```
Scenario: parse_bound("2024-03-15", true)
Expected: Some(DateTime<Utc> for 2024-03-15T23:59:59Z)
```

## Test 1.5 — `parse_bound` year-month "2024-03", start

```
Scenario: parse_bound("2024-03", false)
Expected: Some(DateTime<Utc> for 2024-03-01T00:00:00Z)
```

## Test 1.6 — `parse_bound` year-month "2024-03", end

```
Scenario: parse_bound("2024-03", true)
Expected: Some(DateTime<Utc> for 2024-03-31T23:59:59Z)
  -- month-end correctly resolves to last day
```

## Test 1.7 — `parse_bound` year-month "2024-02", end (leap year)

```
Scenario: parse_bound("2024-02", true)
Expected: Some(DateTime<Utc> for 2024-02-29T23:59:59Z)
  -- 2024 is a leap year, so February has 29 days
```

## Test 1.8 — `parse_bound` year-month "2023-02", end (non-leap year)

```
Scenario: parse_bound("2023-02", true)
Expected: Some(DateTime<Utc> for 2023-02-28T23:59:59Z)
```

## Test 1.9 — `parse_bound` year-month "2024-12", end (December wraps year)

```
Scenario: parse_bound("2024-12", true)
Expected: Some(DateTime<Utc> for 2024-12-31T23:59:59Z)
```

## Test 1.10 — `parse_bound` year only, start

```
Scenario: parse_bound("2024", false)
Expected: Some(DateTime<Utc> for 2024-01-01T00:00:00Z)
```

## Test 1.11 — `parse_bound` year only, end

```
Scenario: parse_bound("2024", true)
Expected: Some(DateTime<Utc> for 2024-12-31T23:59:59Z)
```

## Test 1.12 — `parse_bound` empty / whitespace

```
Scenario: parse_bound("", false), parse_bound("  ", false)
Expected: None for both
```

## Test 1.13 — `parse_bound` garbage input

```
Scenario: parse_bound("not-a-date", false), parse_bound("abc", false)
Expected: None for both
```

## Test 1.14 — `is_within_interval_ms` basic

```
Scenario: is_within_interval_ms(1000, Some(500), Some(1500))
Expected: true

Scenario: is_within_interval_ms(100, Some(500), Some(1500))
Expected: false

Scenario: is_within_interval_ms(2000, Some(500), Some(1500))
Expected: false
```

## Test 1.15 — `is_within_interval_ms` open-ended bounds

```
Scenario: is_within_interval_ms(1000, None, Some(1500))
Expected: true  (no lower bound)

Scenario: is_within_interval_ms(1000, Some(500), None)
Expected: true  (no upper bound)

Scenario: is_within_interval_ms(1000, None, None)
Expected: true  (fully open)
```

## Test 1.16 — `to_cognify_timestamp` happy path

```
Scenario: to_cognify_timestamp(RawExtractedTimestamp { year: 2024, month: 1, day: 1, hour: 0, minute: 0, second: 0 })
Expected: Some(CognifyTimestamp {
    year: 2024, month: 1, day: 1, hour: 0, minute: 0, second: 0,
    time_at: 1704067200000,  // 2024-01-01T00:00:00Z in ms
    timestamp_str: "2024-01-01 00:00:00"
})
```

## Test 1.17 — `to_cognify_timestamp` with time components

```
Scenario: to_cognify_timestamp(RawExtractedTimestamp { year: 2021, month: 7, day: 1, hour: 14, minute: 30, second: 45 })
Expected: Some(CognifyTimestamp {
    year: 2021, month: 7, day: 1, hour: 14, minute: 30, second: 45,
    time_at: <compute>, timestamp_str: "2021-07-01 14:30:45"
})
```

## Test 1.18 — `to_cognify_timestamp` invalid date returns None

```
Scenario: to_cognify_timestamp(RawExtractedTimestamp { year: 2024, month: 13, day: 1, ... })
Expected: None  (month 13 is invalid)

Scenario: to_cognify_timestamp(RawExtractedTimestamp { year: 2024, month: 2, day: 30, ... })
Expected: None  (Feb 30 doesn't exist)
```

## Test 1.19 — `to_cognify_timestamp` defaults (month=1, day=1 when LLM omits)

```
Scenario: to_cognify_timestamp(RawExtractedTimestamp { year: 1889, month: 1, day: 1, hour: 0, minute: 0, second: 0 })
Expected: Some(CognifyTimestamp { year: 1889, ..., timestamp_str: "1889-01-01 00:00:00" })
  -- Verifies serde defaults produce valid output
```

## Test 1.20 — `QueryInterval::parse` integration

```
Scenario: QueryInterval { starts_at: Some("2024-01-01"), ends_at: Some("2024-12-31") }.parse()
Expected: ParsedInterval { start: Some(2024-01-01T00:00:00Z), end: Some(2024-12-31T23:59:59Z) }

Scenario: QueryInterval { starts_at: None, ends_at: None }.parse()
Expected: ParsedInterval { start: None, end: None }

Scenario: QueryInterval { starts_at: Some("2024"), ends_at: None }.parse()
Expected: ParsedInterval { start: Some(2024-01-01T00:00:00Z), end: None }
```
