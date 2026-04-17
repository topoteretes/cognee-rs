# Phase 1 — Data Models

**Crate:** `cognee-models` (`crates/models/src/`)  
**Status:** Done

---

## Goal

Add four public types that mirror the Python DataPoint models used for temporal graph storage. These types are the ground truth for what gets written to the graph database and must match Python's field names exactly so cross-SDK tests can read each other's data.

---

## Python Reference

| Python class | File |
|---|---|
| `Timestamp` (task model) | `cognee/tasks/temporal_graph/models.py` |
| `Timestamp` (DataPoint) | `cognee/modules/engine/models/Timestamp.py` |
| `Interval` (DataPoint) | `cognee/modules/engine/models/Interval.py` |
| `Event` (DataPoint) | `cognee/modules/engine/models/Event.py` |

The **task model** `Timestamp` is the LLM output schema. The **DataPoint** `Timestamp` is what is actually persisted to the graph. The Rust types need to match the DataPoint versions because that is what graph queries read back.

---

## Corrections to Original Plan

- **`time_at` is milliseconds, not seconds.** Python computes:
  ```python
  dt = datetime(year, month, day, hour, minute, second, tzinfo=timezone.utc)
  time_at = int(dt.timestamp() * 1000)  # milliseconds
  ```
  The retriever's Cypher queries compare `n.time_at` values in milliseconds. If Rust stores seconds the filtering will return wrong results.

- **`Timestamp` DataPoint has a `timestamp_str` field** (`"YYYY-MM-DD HH:MM:SS"` zero-padded) that Python also persists. Including it keeps the graph node schema compatible.

- **`hour`, `minute`, `second` default to `0`, not `None`.** In the Python task model these are `int` with `default=0`, not `Optional`. Unknown time fields become zero. Only `month` and `day` default to 1 (calendar convention).

- **`Interval` is a separate graph node**, not just a data container. Python's `Interval` DataPoint is persisted as its own node type `"Interval"` with edges `Interval -[time_from]-> Timestamp` and `Interval -[time_to]-> Timestamp`. The original plan incorrectly described direct `Event -[starts_at]-> Timestamp` edges for ranges — those do not exist.

---

## Types to Add

### `CognifyTimestamp`

Mirrors Python `Timestamp` DataPoint (`cognee/modules/engine/models/Timestamp.py`).

```rust
/// A point in time extracted from text during temporal cognify.
/// Mirrors Python: cognee.modules.engine.models.Timestamp
/// time_at stores milliseconds since Unix epoch (UTC) — same unit as Python.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CognifyTimestamp {
    pub year: u16,
    pub month: u8,      // 1-12; unknown → 1
    pub day: u8,        // 1-31; unknown → 1
    pub hour: u8,       // 0-23; unknown → 0
    pub minute: u8,     // 0-59; unknown → 0
    pub second: u8,     // 0-59; unknown → 0
    /// Milliseconds since Unix epoch (UTC). Computed from the date/time fields.
    pub time_at: i64,
    /// Formatted string "YYYY-MM-DD HH:MM:SS" for human readability.
    pub timestamp_str: String,
}
```

Use `CognifyTimestamp` to avoid collision with any `chrono` type aliases in scope.

### `CognifyInterval`

Mirrors Python `Interval` DataPoint (`cognee/modules/engine/models/Interval.py`).

```rust
/// A time range stored as a graph node of type "Interval".
/// Mirrors Python: cognee.modules.engine.models.Interval
/// Field names time_from / time_to match Python exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CognifyInterval {
    pub time_from: CognifyTimestamp,
    pub time_to: CognifyTimestamp,
}
```

### `TemporalEvent`

Mirrors Python `Event` DataPoint (`cognee/modules/engine/models/Event.py`).

```rust
/// An event extracted from text, optionally anchored to a point or range in time.
/// Mirrors Python: cognee.modules.engine.models.Event
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TemporalEvent {
    pub name: String,
    pub description: Option<String>,
    pub location: Option<String>,
    /// Single point-in-time: creates edge Event -[at]-> Timestamp.
    /// Mutually exclusive with `during`.
    pub at: Option<CognifyTimestamp>,
    /// Time range: creates edge Event -[during]-> Interval.
    /// The Interval node then carries edges to its two Timestamps.
    /// Mutually exclusive with `at`.
    pub during: Option<CognifyInterval>,
    /// Entity attributes attached by the second LLM pass.
    #[serde(default)]
    pub attributes: Vec<EventAttribute>,
}
```

### `EventAttribute`

```rust
/// An entity related to an event, extracted during temporal entity enrichment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventAttribute {
    pub entity: String,
    pub entity_type: String,
    /// Snake_case relationship name, 1-2 words, e.g. "subject", "participant", "source_cause".
    pub relationship: String,
}
```

### `RawExtractedTimestamp` (internal, used by Phase 3 extractor)

This is the LLM output schema (task model) — not stored in the graph. Differs from `CognifyTimestamp` because `time_at` and `timestamp_str` are computed, not provided by the LLM.

```rust
/// LLM output schema for a timestamp. Mirrors Python task model Timestamp.
/// All fields except year default to 1/0; the extractor computes time_at.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct RawExtractedTimestamp {
    pub year: u16,
    #[serde(default = "default_month")]
    pub month: u8,   // default 1
    #[serde(default = "default_day")]
    pub day: u8,     // default 1
    #[serde(default)]
    pub hour: u8,    // default 0
    #[serde(default)]
    pub minute: u8,  // default 0
    #[serde(default)]
    pub second: u8,  // default 0
}
```

---

## Helper: `to_cognify_timestamp`

```rust
/// Convert a raw LLM-extracted timestamp to a CognifyTimestamp with computed time_at.
/// Returns None if the date is invalid (e.g. month=13).
pub fn to_cognify_timestamp(raw: RawExtractedTimestamp) -> Option<CognifyTimestamp> {
    let dt = chrono::NaiveDate::from_ymd_opt(raw.year as i32, raw.month as u32, raw.day as u32)?
        .and_hms_opt(raw.hour as u32, raw.minute as u32, raw.second as u32)?;
    let time_at = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc)
        .timestamp_millis();  // ← milliseconds, matching Python
    let timestamp_str = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        raw.year, raw.month, raw.day, raw.hour, raw.minute, raw.second
    );
    Some(CognifyTimestamp {
        year: raw.year, month: raw.month, day: raw.day,
        hour: raw.hour, minute: raw.minute, second: raw.second,
        time_at, timestamp_str,
    })
}
```

---

## Files

| Action | Path |
|---|---|
| New file | `crates/models/src/temporal_event.rs` |
| Modify | `crates/models/src/lib.rs` — add `pub mod temporal_event; pub use temporal_event::*;` |

---

## Verification

```bash
cargo check --all-targets -p cognee-models
```

All four types must compile with `Serialize + Deserialize + JsonSchema + Clone`.
