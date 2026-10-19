//! Timestamp - a point in time (or a period) that facts anchor to.
//!
//! Mirrors Python's `cognee/modules/engine/models/Timestamp.py` and the parser
//! that builds one from text, `cognee/modules/engine/utils/timestamp_from_text.py`,
//! plus the absolute-date half of `cognee/modules/engine/utils/temporal_hints.py`
//! (`normalize_absolute_date`).
//!
//! Default cognify turns every node the LLM typed "Timestamp" whose name parses
//! here into one of these, so `*_at` edges point at real times — the graph a
//! timestamp-anchored TEMPORAL retrieval reads (`TemporalHybridRetriever` in
//! `cognee-search`, a port of Python's, through
//! `GraphDBTrait::get_timestamps_in_range` / `get_temporal_anchors`).

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

/// How much of a [`Timestamp`] the source stated.
///
/// `1950` and `1950-01-01` have the same calendar fields; the precision is what
/// keeps them apart. `Span` is a period with a stated start and end
/// (`1803/1815`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimestampPrecision {
    Year,
    Month,
    Day,
    Second,
    Span,
}

impl TimestampPrecision {
    /// The lowercase name Python stores (`"year"`, `"span"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
            Self::Day => "day",
            Self::Second => "second",
            Self::Span => "span",
        }
    }
}

/// A point in time that facts anchor to through `*_at` edges.
///
/// `timestamp_str` is the normalized form the source stated (`YYYY`, `YYYY-MM`,
/// `YYYY-MM-DD`, `YYYY-MM-DD HH:MM:SS`, or `<start>/<end>` for a period with
/// both bounds stated). The calendar fields and `time_at` (milliseconds since
/// the epoch, UTC) hold the lower bound of that period, with unstated parts
/// filled by the earliest value; `time_until` is the period's exclusive upper
/// bound, so a window `[start, end)` overlaps this timestamp exactly when
/// `time_at < end` and `time_until > start` ([`timestamp_overlaps`]).
///
/// Not embedded (`index_fields` is empty): a bare time string carries no meaning
/// for vector search. The id derives from `timestamp_str`, so every mention of
/// the same time across chunks and documents resolves to one node.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Timestamp {
    #[serde(flatten)]
    pub base: DataPoint,
    /// Repeats `timestamp_str`: what graph renderers and the hybrid context
    /// label a node by.
    pub name: String,
    pub timestamp_str: String,
    pub precision: TimestampPrecision,
    pub time_at: i64,
    pub time_until: i64,
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Timestamp {
    /// The graph node type every adapter filters on.
    pub const TYPE_NAME: &'static str = "Timestamp";

    /// Deterministic, class-namespaced id for a normalized time string.
    ///
    /// Mirrors Python's `Timestamp.id_for(value)` (`identity_fields =
    /// ["timestamp_str"]`): `uuid5(NAMESPACE_OID, "Timestamp:<normalized>")`.
    pub fn id_for(timestamp_str: &str) -> Uuid {
        cognee_utils::data_point_id_for(Self::TYPE_NAME, &[timestamp_str])
    }

    /// The `Timestamp` for a time string, or `None` when it names no absolute time.
    ///
    /// Port of Python's `timestamp_from_text`. Accepts the normalized forms
    /// (`YYYY`, `YYYY-MM`, `YYYY-MM-DD`, `YYYY-MM-DD HH:MM:SS`), absolute dates in
    /// prose ("23 March 1947", "March 1947"), which [`normalize_absolute_date`]
    /// brings to one of those shapes at the precision they state, and periods
    /// with both bounds stated (`1803/1815`, "from 1803 to 1815", "between March
    /// 2024 and June 2024"), which become one [`TimestampPrecision::Span`].
    /// Relative or year-less expressions ("that spring", "the 1950s") return
    /// `None`, and so does a string in the right shape that is not a real date
    /// (`1950-02-30`, `0000`), or year 9999, whose exclusive end would need year
    /// 10000.
    pub fn from_text(text: &str) -> Option<Self> {
        let period = parse_time_period(text)?;
        Some(Self::build(
            period.normalized,
            period.precision,
            period.lower,
            period.time_until,
        ))
    }

    fn build(
        timestamp_str: String,
        precision: TimestampPrecision,
        lower: NaiveDateTime,
        time_until: i64,
    ) -> Self {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("index_fields".to_string(), serde_json::json!([]));
        metadata.insert(
            "identity_fields".to_string(),
            serde_json::json!(["timestamp_str"]),
        );
        let mut base = DataPoint::with_metadata(Self::TYPE_NAME, None, metadata);
        base.id = Self::id_for(&timestamp_str);
        Self {
            base,
            name: timestamp_str.clone(),
            timestamp_str,
            precision,
            time_at: lower.and_utc().timestamp_millis(),
            time_until,
            year: lower.year(),
            month: lower.month(),
            day: lower.day(),
            hour: lower.hour(),
            minute: lower.minute(),
            second: lower.second(),
        }
    }
}

impl HasDataPoint for Timestamp {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }

    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
}

/// The period a time string names, without building a [`Timestamp`] datapoint.
///
/// What [`Timestamp::from_text`] parses, for callers that only need the
/// answer — the date-hint finder probes many word windows per chunk, and a
/// full datapoint (id, metadata map) per probe is wasted work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimePeriod {
    /// The normalized string (`1947-03-23`, `1803/1815`): the Timestamp's
    /// `timestamp_str` and identity.
    pub normalized: String,
    pub precision: TimestampPrecision,
    /// Lower bound of the period (unstated parts at their earliest value).
    pub lower: NaiveDateTime,
    /// Exclusive upper bound, in ms since the epoch (UTC).
    pub time_until: i64,
}

/// Parse a time string into the period it names; see [`Timestamp::from_text`]
/// for what is accepted.
pub fn parse_time_period(text: &str) -> Option<TimePeriod> {
    point_period(text).or_else(|| span_period(text))
}

/// Whether a Timestamp's period `[time_at, time_until)` meets the half-open
/// window `[start, end)` (ms since the epoch, UTC; `None` leaves a side open).
///
/// `time_until` defaults to one second after `time_at` for nodes written before
/// the field existed (the temporal-event pipeline's timestamps). Port of
/// Python's `timestamp_overlaps` in `graph_db_interface.py`.
pub fn timestamp_overlaps(
    time_at: i64,
    time_until: Option<i64>,
    start: Option<i64>,
    end: Option<i64>,
) -> bool {
    let time_until = time_until.unwrap_or(time_at + 1000);
    end.is_none_or(|end| time_at < end) && start.is_none_or(|start| time_until > start)
}

// ── Point parsing ─────────────────────────────────────────────────────────────

/// A fully-validated `YYYY[-MM[-DD[ HH:MM:SS]]]` string, before it becomes a
/// [`Timestamp`]. Fields beyond `precision` are unset (lower bound) defaults.
struct Normalized {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    precision: TimestampPrecision,
}

/// An optional field: absent is `Some(None)`, present-and-valid is
/// `Some(Some(v))`, present-but-invalid is `None`.
fn optional<'a>(
    part: Option<&'a str>,
    parse: impl Fn(&'a str) -> Option<u32>,
) -> Option<Option<u32>> {
    match part {
        None => Some(None),
        Some(part) => parse(part).map(Some),
    }
}

fn all_digits(s: &str, len: usize) -> Option<u32> {
    (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Python's `_match_normalized`: one of the four normalized shapes, matched in
/// full, most precise first. Shape only — calendar validity is checked when the
/// bounds are built.
fn match_normalized(text: &str) -> Option<Normalized> {
    let (date, time) = match text.split_once(' ') {
        Some((date, time)) => (date, Some(time)),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let year = all_digits(parts.next()?, 4)?;
    let month = optional(parts.next(), |m| all_digits(m, 2))?;
    let day = optional(parts.next(), |d| all_digits(d, 2))?;
    if parts.next().is_some() {
        return None;
    }
    let (hour, minute, second) = match time {
        None => (0, 0, 0),
        Some(time) => {
            // A time only follows a full date.
            day?;
            let mut fields = time.split(':');
            let hour = all_digits(fields.next()?, 2)?;
            let minute = all_digits(fields.next()?, 2)?;
            let second = all_digits(fields.next()?, 2)?;
            if fields.next().is_some() {
                return None;
            }
            (hour, minute, second)
        }
    };
    let precision = match (month, day, time) {
        (_, _, Some(_)) => TimestampPrecision::Second,
        (Some(_), Some(_), None) => TimestampPrecision::Day,
        (Some(_), None, None) => TimestampPrecision::Month,
        _ => TimestampPrecision::Year,
    };
    Some(Normalized {
        year: i32::try_from(year).ok()?,
        month: month.unwrap_or(1),
        day: day.unwrap_or(1),
        hour,
        minute,
        second,
        precision,
    })
}

/// The exclusive end of the period `lower` starts, at `precision`; `None` when
/// it would fall in year 10000 (Python's `datetime` stops at 9999).
fn period_end(lower: NaiveDateTime, precision: TimestampPrecision) -> Option<NaiveDateTime> {
    let end = match precision {
        TimestampPrecision::Year => lower.with_year(lower.year() + 1)?,
        TimestampPrecision::Month => {
            if lower.month() == 12 {
                lower.with_year(lower.year() + 1)?.with_month(1)?
            } else {
                lower.with_month(lower.month() + 1)?
            }
        }
        TimestampPrecision::Day => lower + Duration::days(1),
        TimestampPrecision::Second | TimestampPrecision::Span => lower + Duration::seconds(1),
    };
    (end.year() <= 9999).then_some(end)
}

/// A single-point period (no spans); see [`Timestamp::from_text`].
fn point_period(text: &str) -> Option<TimePeriod> {
    let trimmed = text.trim();
    let (normalized_str, normalized) = match match_normalized(trimmed) {
        Some(n) => (trimmed.to_string(), n),
        None => {
            let normalized = normalize_absolute_date(text)?;
            let n = match_normalized(&normalized)?;
            (normalized, n)
        }
    };
    // Python's `datetime` rejects year 0; the shape check above already caps
    // the year at four digits.
    if normalized.year < 1 {
        return None;
    }
    let lower = NaiveDate::from_ymd_opt(normalized.year, normalized.month, normalized.day)?
        .and_hms_opt(normalized.hour, normalized.minute, normalized.second)?;
    let upper = period_end(lower, normalized.precision)?;
    Some(TimePeriod {
        normalized: normalized_str,
        precision: normalized.precision,
        lower,
        time_until: upper.and_utc().timestamp_millis(),
    })
}

// ── Spans ─────────────────────────────────────────────────────────────────────

const SPAN_WORDS: [&str; 5] = ["to", "and", "until", "till", "through"];

/// Split a period on its leftmost connector: `/`, a whitespace-delimited
/// `to`/`and`/`until`/`till`/`through`, an en or em dash, or a hyphen between
/// two four-digit years (so `1803-05` stays a month). Port of Python's
/// `_SPAN_SEPARATOR` split with `maxsplit=1`, after the `from`/`between`
/// prefix is stripped.
fn split_span(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let text = ["from ", "between "]
        .iter()
        .find_map(|prefix| {
            lower
                .starts_with(prefix)
                .then(|| text[prefix.len()..].trim_start())
        })
        .unwrap_or(text);
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();

    for (i, ch) in text.char_indices() {
        match ch {
            '/' | '\u{2013}' | '\u{2014}' => {
                return Some((&text[..i], &text[i + ch.len_utf8()..]));
            }
            '-' => {
                let before = i >= 4 && bytes[i - 4..i].iter().all(u8::is_ascii_digit);
                let after =
                    bytes.len() >= i + 5 && bytes[i + 1..i + 5].iter().all(u8::is_ascii_digit);
                if before && after {
                    return Some((&text[..i], &text[i + 1..]));
                }
            }
            c if c.is_whitespace() => {
                let rest = lower[i..].trim_start();
                let word_start = lower.len() - rest.len();
                for word in SPAN_WORDS {
                    if let Some(after) = rest.strip_prefix(word)
                        && after.starts_with(char::is_whitespace)
                    {
                        let split_end = word_start + word.len();
                        return Some((&text[..i], &text[split_end..]));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// The span period for `<start>/<end>` or a prose period, or `None`.
///
/// Both halves must parse as points and the start must come first; the span
/// runs from the start's lower bound to the end's exclusive upper bound, so
/// `1803/1815` is `[1803-01-01, 1816-01-01)`. The calendar fields are the
/// start's.
fn span_period(text: &str) -> Option<TimePeriod> {
    let (start, end) = split_span(text)?;
    let start = point_period(start)?;
    let end = point_period(end)?;
    if start.lower >= end.lower {
        return None;
    }
    Some(TimePeriod {
        normalized: format!("{}/{}", start.normalized, end.normalized),
        precision: TimestampPrecision::Span,
        lower: start.lower,
        time_until: end.time_until,
    })
}

// ── Absolute dates in prose ───────────────────────────────────────────────────

const MONTHS: [(&str, u32); 12] = [
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
];

/// The month a word names: the full name, its three-letter abbreviation, or
/// "sept". Case-insensitive.
pub fn month_from_word(word: &str) -> Option<u32> {
    let word = word.to_ascii_lowercase();
    let word = word.trim_end_matches('.');
    if word == "sept" {
        return Some(9);
    }
    MONTHS
        .iter()
        .find(|(name, _)| *name == word || (word.len() == 3 && name.starts_with(word)))
        .map(|(_, month)| *month)
}

/// Words an absolute date may carry around its parts without changing it:
/// prepositions, weekday names and the UTC designator.
fn is_filler(word: &str) -> bool {
    const FILLER: [&str; 22] = [
        "on",
        "of",
        "the",
        "at",
        "in",
        "utc",
        "gmt",
        "z",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
        "mon",
        "tue",
        "wed",
        "thu",
        "fri",
        "sat",
        "sun",
    ];
    FILLER.contains(&word.to_ascii_lowercase().as_str())
}

/// A day of the month, optionally with an ordinal suffix (`27`, `27th`).
fn day_from_word(word: &str) -> Option<u32> {
    let lower = word.to_ascii_lowercase();
    let digits = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|suffix| lower.strip_suffix(suffix))
        .unwrap_or(&lower);
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let day: u32 = digits.parse().ok()?;
    (1..=31).contains(&day).then_some(day)
}

/// `HH:MM` or `HH:MM:SS`, with an optional trailing `am`/`pm` handled by the
/// caller. Minutes and seconds are two digits, so a score like `1:2` is no time.
fn time_from_word(word: &str) -> Option<(u32, u32, u32)> {
    let mut fields = word.split(':');
    let hour = fields.next()?;
    if hour.is_empty() || hour.len() > 2 || !hour.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hour: u32 = hour.parse().ok()?;
    let minute = all_digits(fields.next()?, 2)?;
    let second = optional(fields.next(), |s| all_digits(s, 2))?.unwrap_or(0);
    if fields.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

/// A numeric calendar date: `YYYY-M-D`, `YYYY-M`, or `D/M/YYYY` / `M/D/YYYY`
/// (month first, as English dates are read, unless the first part cannot be a
/// month). Returns `(year, month, day)`.
fn numeric_date(word: &str) -> Option<(u32, u32, Option<u32>)> {
    let small = |s: &str| -> Option<u32> {
        (!s.is_empty() && s.len() <= 2 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    if word.contains('-') {
        let mut parts = word.split('-');
        let year = all_digits(parts.next()?, 4)?;
        let month = small(parts.next()?)?;
        let day = optional(parts.next(), small)?;
        if parts.next().is_some() {
            return None;
        }
        return Some((year, month, day));
    }
    let mut parts = word.split('/');
    let first = small(parts.next()?)?;
    let second = small(parts.next()?)?;
    let year = all_digits(parts.next()?, 4)?;
    if parts.next().is_some() {
        return None;
    }
    let (month, day) = if first > 12 {
        (second, first)
    } else {
        (first, second)
    };
    Some((year, month, Some(day)))
}

/// Normalize an absolute date expression to the shapes [`Timestamp::from_text`]
/// accepts, at the precision the expression states.
///
/// "23 March 1947" becomes `1947-03-23`, "March 1947" becomes `1947-03`, and
/// "05:32 on 27 April 1986" becomes `1986-04-27 05:32:00`: a month name never
/// fabricates a day. Relative or year-less expressions ("July 24", "four weeks
/// later", "spring of 1943", "the 1950s") return `None`. The year must be
/// written as four digits.
///
/// Python delegates to `dateparser` with absolute-only, year-required settings;
/// this is a deliberately small grammar covering the shapes Python's tests pin:
/// one four-digit year, at most one month (name or numeric), at most one day, at
/// most one time, and nothing else but prepositions, weekday names and `UTC`.
/// Any other word makes the expression not an absolute date.
pub fn normalize_absolute_date(text: &str) -> Option<String> {
    let mut year: Option<u32> = None;
    let mut month: Option<u32> = None;
    let mut day: Option<u32> = None;
    let mut time: Option<(u32, u32, u32)> = None;

    fn set<T>(slot: &mut Option<T>, value: T) -> Option<()> {
        if slot.is_some() {
            return None;
        }
        *slot = Some(value);
        Some(())
    }

    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')'))
        .filter(|w| !w.is_empty())
        .collect();
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        index += 1;
        if let Some(y) = all_digits(word, 4) {
            set(&mut year, y)?;
        } else if let Some(m) = month_from_word(word) {
            set(&mut month, m)?;
        } else if let Some(d) = day_from_word(word) {
            set(&mut day, d)?;
        } else if let Some((h, mi, s)) = time_from_word(word) {
            let meridian = words.get(index).map(|w| w.to_ascii_lowercase());
            let hour = match meridian.as_deref() {
                Some("am") | Some("a.m.") => {
                    index += 1;
                    if h == 12 { 0 } else { h }
                }
                Some("pm") | Some("p.m.") => {
                    index += 1;
                    if h == 12 { 12 } else { h + 12 }
                }
                _ => h,
            };
            if hour > 23 {
                return None;
            }
            set(&mut time, (hour, mi, s))?;
        } else if let Some((y, m, d)) = numeric_date(word) {
            set(&mut year, y)?;
            set(&mut month, m)?;
            if let Some(d) = d {
                set(&mut day, d)?;
            }
        } else if !is_filler(word) {
            return None;
        }
    }

    let year = year?;
    let formatted = match (month, day, time) {
        (Some(m), Some(d), Some((h, mi, s))) => {
            format!("{year:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        }
        (Some(m), Some(d), None) => format!("{year:04}-{m:02}-{d:02}"),
        (Some(m), None, None) => format!("{year:04}-{m:02}"),
        (None, None, None) => format!("{year:04}"),
        // A day or a time without the parts above it names no period.
        _ => return None,
    };
    // Only a real calendar date counts.
    NaiveDate::from_ymd_opt(
        i32::try_from(year).ok()?,
        month.unwrap_or(1),
        day.unwrap_or(1),
    )?;
    Some(formatted)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn epoch_ms(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    // Mirrors Python's test_timestamp_from_text.py and test_temporal_hints.py.

    #[test]
    fn four_precisions_fill_unstated_parts_with_the_lower_bound() {
        for (text, precision, fields, lower) in [
            (
                "1969",
                TimestampPrecision::Year,
                (1969, 1, 1, 0, 0, 0),
                epoch_ms(1969, 1, 1, 0, 0, 0),
            ),
            (
                "1969-07",
                TimestampPrecision::Month,
                (1969, 7, 1, 0, 0, 0),
                epoch_ms(1969, 7, 1, 0, 0, 0),
            ),
            (
                "1969-07-20",
                TimestampPrecision::Day,
                (1969, 7, 20, 0, 0, 0),
                epoch_ms(1969, 7, 20, 0, 0, 0),
            ),
            (
                "1969-07-20 20:17:40",
                TimestampPrecision::Second,
                (1969, 7, 20, 20, 17, 40),
                epoch_ms(1969, 7, 20, 20, 17, 40),
            ),
        ] {
            let ts = Timestamp::from_text(text).unwrap();
            assert_eq!(ts.precision, precision, "{text}");
            assert_eq!(ts.timestamp_str, text);
            assert_eq!(ts.name, text);
            assert_eq!(
                (ts.year, ts.month, ts.day, ts.hour, ts.minute, ts.second),
                fields
            );
            assert_eq!(ts.time_at, lower);
            assert_eq!(ts.base.data_type, "Timestamp");
        }
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(
            Timestamp::from_text("  2024-02-29 ").unwrap().timestamp_str,
            "2024-02-29"
        );
    }

    #[test]
    fn non_times_and_impossible_dates_are_rejected() {
        for text in [
            "1940s",
            "",
            "Apollo 11",
            "0000",
            "1950-13",
            "1950-02-30",
            "2023-02-29",
            "1950-01-01 24:00:00",
            "the 1950s",
            "that spring",
            "four weeks later",
            "spring of 1943",
            "July 24",
            "24 July",
            "9999",
        ] {
            assert!(Timestamp::from_text(text).is_none(), "{text:?} parsed");
        }
    }

    #[test]
    fn id_derives_from_the_normalized_string() {
        let a = Timestamp::from_text("1969").unwrap();
        assert_eq!(a.base.id, Timestamp::from_text(" 1969 ").unwrap().base.id);
        assert_eq!(a.base.id, Timestamp::id_for("1969"));
        assert_ne!(
            a.base.id,
            Timestamp::from_text("1969-01-01").unwrap().base.id
        );
        // Python: uuid5(NAMESPACE_OID, "Timestamp:1969").
        assert_eq!(
            a.base.id,
            Uuid::new_v5(&Uuid::NAMESPACE_OID, b"Timestamp:1969")
        );
    }

    #[test]
    fn an_absolute_date_in_prose_is_normalized_first() {
        for (text, normalized, precision) in [
            ("23 March 1947", "1947-03-23", TimestampPrecision::Day),
            ("March 1947", "1947-03", TimestampPrecision::Month),
            ("April 27, 1791", "1791-04-27", TimestampPrecision::Day),
            ("1969-07-20 (Sunday)", "1969-07-20", TimestampPrecision::Day),
            ("20/07/1969", "1969-07-20", TimestampPrecision::Day),
            ("1969-7-20", "1969-07-20", TimestampPrecision::Day),
            (
                "1969-07-20 20:17",
                "1969-07-20 20:17:00",
                TimestampPrecision::Second,
            ),
        ] {
            let ts = Timestamp::from_text(text).unwrap_or_else(|| panic!("{text:?}"));
            assert_eq!(ts.timestamp_str, normalized, "{text}");
            assert_eq!(ts.name, normalized);
            assert_eq!(ts.precision, precision, "{text}");
            assert_eq!(ts.base.id, Timestamp::id_for(normalized));
        }
    }

    #[test]
    fn normalize_absolute_date_follows_the_stated_precision() {
        for (text, expected) in [
            ("23 March 1947", Some("1947-03-23")),
            ("March 1947", Some("1947-03")),
            ("April 27, 1791", Some("1791-04-27")),
            ("05:32 on 27 April 1986", Some("1986-04-27 05:32:00")),
            ("the 1950s", None),
            ("spring of 1943", None),
            ("July 24", None),
            ("24 July", None),
            ("July 13", None),
            ("May 5", None),
            ("four weeks later", None),
            ("that spring", None),
        ] {
            assert_eq!(normalize_absolute_date(text).as_deref(), expected, "{text}");
        }
    }

    #[test]
    fn half_open_bounds_at_the_stated_precision() {
        for (text, lower, upper) in [
            (
                "1950",
                epoch_ms(1950, 1, 1, 0, 0, 0),
                epoch_ms(1951, 1, 1, 0, 0, 0),
            ),
            (
                "1950-12",
                epoch_ms(1950, 12, 1, 0, 0, 0),
                epoch_ms(1951, 1, 1, 0, 0, 0),
            ),
            (
                "2024-02-29",
                epoch_ms(2024, 2, 29, 0, 0, 0),
                epoch_ms(2024, 3, 1, 0, 0, 0),
            ),
            (
                "1969-07-20 20:17:40",
                epoch_ms(1969, 7, 20, 20, 17, 40),
                epoch_ms(1969, 7, 20, 20, 17, 41),
            ),
            (
                "March 1947",
                epoch_ms(1947, 3, 1, 0, 0, 0),
                epoch_ms(1947, 4, 1, 0, 0, 0),
            ),
        ] {
            let ts = Timestamp::from_text(text).unwrap();
            assert_eq!((ts.time_at, ts.time_until), (lower, upper), "{text}");
        }
    }

    #[test]
    fn a_period_with_both_bounds_is_one_span_timestamp() {
        for (text, normalized, lower, upper) in [
            ("1803/1815", "1803/1815", (1803, 1, 1), (1816, 1, 1)),
            ("from 1803 to 1815", "1803/1815", (1803, 1, 1), (1816, 1, 1)),
            (
                "between 1803 and 1815",
                "1803/1815",
                (1803, 1, 1),
                (1816, 1, 1),
            ),
            ("1803\u{2013}1815", "1803/1815", (1803, 1, 1), (1816, 1, 1)),
            ("1803-1815", "1803/1815", (1803, 1, 1), (1816, 1, 1)),
            (
                "2024-03/2024-06",
                "2024-03/2024-06",
                (2024, 3, 1),
                (2024, 7, 1),
            ),
            (
                "between March 2024 and June 2024",
                "2024-03/2024-06",
                (2024, 3, 1),
                (2024, 7, 1),
            ),
            (
                "from 12 May 2023 to 14 May 2023",
                "2023-05-12/2023-05-14",
                (2023, 5, 12),
                (2023, 5, 15),
            ),
            (
                "1969-07-16 / 1969-07-24",
                "1969-07-16/1969-07-24",
                (1969, 7, 16),
                (1969, 7, 25),
            ),
        ] {
            let ts = Timestamp::from_text(text).unwrap_or_else(|| panic!("{text:?}"));
            assert_eq!(ts.precision, TimestampPrecision::Span, "{text}");
            assert_eq!(ts.timestamp_str, normalized);
            assert_eq!(ts.name, normalized);
            assert_eq!(ts.time_at, epoch_ms(lower.0, lower.1, lower.2, 0, 0, 0));
            assert_eq!(ts.time_until, epoch_ms(upper.0, upper.1, upper.2, 0, 0, 0));
            assert_eq!((ts.year, ts.month, ts.day), (lower.0, lower.1, lower.2));
            assert_eq!(ts.base.id, Timestamp::id_for(normalized));
        }
    }

    #[test]
    fn malformed_periods_are_rejected() {
        for text in [
            "1815/1803",
            "1803/1803",
            "from 1803 to that spring",
            "between March and June 2024",
            "1803/1815/1820",
        ] {
            assert!(Timestamp::from_text(text).is_none(), "{text:?} parsed");
        }
    }

    #[test]
    fn a_hyphen_splits_only_two_four_digit_years() {
        assert_eq!(
            Timestamp::from_text("1803-05").unwrap().precision,
            TimestampPrecision::Month
        );
        assert_eq!(
            Timestamp::from_text("1803-1815").unwrap().precision,
            TimestampPrecision::Span
        );
    }

    #[test]
    fn a_window_inside_a_span_overlaps_it() {
        let span = Timestamp::from_text("from 1803 to 1815").unwrap();
        let overlaps = |start: i64, end: i64| {
            timestamp_overlaps(span.time_at, Some(span.time_until), Some(start), Some(end))
        };
        assert!(overlaps(
            epoch_ms(1805, 1, 1, 0, 0, 0),
            epoch_ms(1806, 1, 1, 0, 0, 0)
        ));
        assert!(!overlaps(
            epoch_ms(1816, 1, 1, 0, 0, 0),
            epoch_ms(1817, 1, 1, 0, 0, 0)
        ));
        assert!(!overlaps(
            epoch_ms(1802, 1, 1, 0, 0, 0),
            epoch_ms(1803, 1, 1, 0, 0, 0)
        ));
    }

    #[test]
    fn a_period_ending_at_the_epoch_keeps_its_real_upper_bound() {
        for text in [
            "1969",
            "1969-12",
            "1969-12-31",
            "1968/1969",
            "1969-12-31 23:59:59",
        ] {
            assert_eq!(Timestamp::from_text(text).unwrap().time_until, 0, "{text}");
        }
    }

    #[test]
    fn missing_time_until_defaults_to_one_second() {
        assert!(timestamp_overlaps(0, None, Some(999), Some(2000)));
        assert!(!timestamp_overlaps(0, None, Some(1000), Some(2000)));
        assert!(timestamp_overlaps(0, Some(0), None, Some(1)));
        assert!(!timestamp_overlaps(0, Some(0), Some(0), None));
    }
}
