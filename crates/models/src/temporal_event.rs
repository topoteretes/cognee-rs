use chrono::{DateTime, NaiveDate, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A point in time extracted from text during temporal cognify.
/// Mirrors Python: cognee.modules.engine.models.Timestamp
/// time_at stores milliseconds since Unix epoch (UTC) — same unit as Python.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CognifyTimestamp {
    pub year: u16,
    pub month: u8,  // 1-12; unknown → 1
    pub day: u8,    // 1-31; unknown → 1
    pub hour: u8,   // 0-23; unknown → 0
    pub minute: u8, // 0-59; unknown → 0
    pub second: u8, // 0-59; unknown → 0
    /// Milliseconds since Unix epoch (UTC). Computed from the date/time fields.
    pub time_at: i64,
    /// Formatted string "YYYY-MM-DD HH:MM:SS" for human readability.
    pub timestamp_str: String,
}

/// A time range stored as a graph node of type "Interval".
/// Mirrors Python: cognee.modules.engine.models.Interval
/// Field names time_from / time_to match Python exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CognifyInterval {
    pub time_from: CognifyTimestamp,
    pub time_to: CognifyTimestamp,
}

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

/// An entity related to an event, extracted during temporal entity enrichment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventAttribute {
    pub entity: String,
    pub entity_type: String,
    /// Snake_case relationship name, 1-2 words, e.g. "subject", "participant", "source_cause".
    pub relationship: String,
}

fn default_month() -> u8 {
    1
}

fn default_day() -> u8 {
    1
}

/// LLM output schema for a timestamp. Mirrors Python task model Timestamp.
/// All fields except year default to 1/0; the extractor computes time_at.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RawExtractedTimestamp {
    pub year: u16,
    #[serde(default = "default_month")]
    pub month: u8, // default 1
    #[serde(default = "default_day")]
    pub day: u8, // default 1
    #[serde(default)]
    pub hour: u8, // default 0
    #[serde(default)]
    pub minute: u8, // default 0
    #[serde(default)]
    pub second: u8, // default 0
}

/// Convert a raw LLM-extracted timestamp to a CognifyTimestamp with computed time_at.
/// Returns None if the date is invalid (e.g. month=13).
pub fn to_cognify_timestamp(raw: RawExtractedTimestamp) -> Option<CognifyTimestamp> {
    let naive = NaiveDate::from_ymd_opt(raw.year as i32, raw.month as u32, raw.day as u32)?
        .and_hms_opt(raw.hour as u32, raw.minute as u32, raw.second as u32)?;
    let time_at = DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc).timestamp_millis(); // milliseconds, matching Python
    let timestamp_str = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        raw.year, raw.month, raw.day, raw.hour, raw.minute, raw.second
    );
    Some(CognifyTimestamp {
        year: raw.year,
        month: raw.month,
        day: raw.day,
        hour: raw.hour,
        minute: raw.minute,
        second: raw.second,
        time_at,
        timestamp_str,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn to_cognify_timestamp_happy_path() {
        let raw = RawExtractedTimestamp {
            year: 2024,
            month: 3,
            day: 15,
            hour: 0,
            minute: 0,
            second: 0,
        };
        let result = to_cognify_timestamp(raw).unwrap();

        let expected_dt = Utc.with_ymd_and_hms(2024, 3, 15, 0, 0, 0).unwrap();
        assert_eq!(result.time_at, expected_dt.timestamp_millis());
        assert_eq!(result.timestamp_str, "2024-03-15 00:00:00");
        assert_eq!(result.year, 2024);
        assert_eq!(result.month, 3);
        assert_eq!(result.day, 15);
    }

    #[test]
    fn to_cognify_timestamp_with_time_components() {
        let raw = RawExtractedTimestamp {
            year: 2024,
            month: 7,
            day: 4,
            hour: 14,
            minute: 30,
            second: 45,
        };
        let result = to_cognify_timestamp(raw).unwrap();

        let expected_dt = Utc.with_ymd_and_hms(2024, 7, 4, 14, 30, 45).unwrap();
        assert_eq!(result.time_at, expected_dt.timestamp_millis());
        assert_eq!(result.timestamp_str, "2024-07-04 14:30:45");
        assert_eq!(result.hour, 14);
        assert_eq!(result.minute, 30);
        assert_eq!(result.second, 45);
    }

    #[test]
    fn to_cognify_timestamp_invalid_dates_return_none() {
        // Month 13 is invalid
        let raw = RawExtractedTimestamp {
            year: 2024,
            month: 13,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert!(to_cognify_timestamp(raw).is_none());

        // Feb 30 is invalid
        let raw = RawExtractedTimestamp {
            year: 2024,
            month: 2,
            day: 30,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert!(to_cognify_timestamp(raw).is_none());
    }

    #[test]
    fn to_cognify_timestamp_serde_defaults() {
        // Simulates serde defaults: year from LLM, month=1, day=1, h/m/s=0
        let raw = RawExtractedTimestamp {
            year: 1889,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        let result = to_cognify_timestamp(raw).unwrap();

        let expected_dt = Utc.with_ymd_and_hms(1889, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(result.time_at, expected_dt.timestamp_millis());
        assert_eq!(result.timestamp_str, "1889-01-01 00:00:00");
        assert_eq!(result.year, 1889);
        assert_eq!(result.month, 1);
        assert_eq!(result.day, 1);
    }
}
