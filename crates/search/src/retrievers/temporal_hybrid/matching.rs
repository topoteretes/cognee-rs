//! The question's time window, and how tightly a date fits it.
//!
//! Port of `cognee/modules/retrieval/temporal_hybrid/matching.py`. The
//! retriever fetches an oversized hybrid candidate set, asks the graph which of
//! those candidates are attached to a `Timestamp` inside the window
//! ([`GraphDBTrait::get_temporal_anchors`](cognee_graph::GraphDBTrait::get_temporal_anchors)),
//! moves the anchored ones to the front
//! ([`HybridCandidates::prioritize`](crate::retrievers::hybrid::HybridCandidates::prioritize))
//! and cuts with the same `finalize` plain hybrid uses, so the temporal result
//! is never smaller than plain hybrid's.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};
use cognee_graph::WindowTimestamp;
use cognee_llm::{GenerationOptions, Llm, LlmExt, Message};
use cognee_models::RawExtractedTimestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::TemporalFallbackReason;
use crate::types::SearchError;

/// System prompt for the window extraction. Vendored byte-for-byte from
/// Python's `QUERY_INTERVAL_PROMPT` (`temporal_hybrid/matching.py`).
///
/// Unlike the Event-based retriever this replaced, it carries no "current
/// date": relative times ("last year") are deliberately not resolved, so the
/// extraction does not depend on the clock at all.
pub(crate) const QUERY_INTERVAL_PROMPT: &str =
    include_str!("../../utils/prompts/query_interval.txt");

/// The window the LLM extracts from a question. Port of Python's
/// `QueryInterval` (`cognee/tasks/temporal_graph/models.py`): each side is a
/// set of UTC calendar fields (`year` required, the rest defaulting to the
/// start of their unit), or `null` when that side is open.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub(crate) struct QueryInterval {
    #[serde(default)]
    pub starts_at: Option<RawExtractedTimestamp>,
    #[serde(default)]
    pub ends_at: Option<RawExtractedTimestamp>,
}

/// The question's window `(start, end)` — start inclusive, end exclusive,
/// either side `None` when open — or the reason there is none.
///
/// Python returns the triple `(start, end, reason)`; this keeps the two cases
/// apart in the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtractedWindow {
    Window {
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    },
    NoWindow(TemporalFallbackReason),
}

/// One boundary as a UTC instant: `Ok(None)` for an open side, `Err(())` for
/// calendar fields that name no real moment (`2021-02-29`, year 0, month 13).
/// Python's `datetime(**fields, tzinfo=utc)` raises `ValueError` for the
/// in-range-but-impossible ones; for the out-of-range ones see
/// [`classify_interval`].
fn boundary_datetime(
    boundary: Option<&RawExtractedTimestamp>,
) -> Result<Option<DateTime<Utc>>, ()> {
    let Some(fields) = boundary else {
        return Ok(None);
    };
    // Python's model bounds the year to 1..=9999.
    if !(1..=9999).contains(&fields.year) {
        return Err(());
    }
    NaiveDate::from_ymd_opt(
        i32::from(fields.year),
        u32::from(fields.month),
        u32::from(fields.day),
    )
    .and_then(|date| {
        date.and_hms_opt(
            u32::from(fields.hour),
            u32::from(fields.minute),
            u32::from(fields.second),
        )
    })
    .map(|moment| Some(moment.and_utc()))
    .ok_or(())
}

/// Classify an extracted interval. Port of the second half of Python's
/// `extract_query_interval`: both sides open is `no_time_constraint`; an
/// impossible date, or a start at or after the end, is `invalid_interval`.
///
/// Deliberate divergence: calendar fields outside their range (month 13,
/// day 32, hour 24, year 0 or above 9999) also land here as
/// `invalid_interval`, so the search falls back to plain hybrid. In Python
/// they never reach this step: `QueryInterval`'s `Timestamp` bounds every field
/// with `ge`/`le` (`cognee/tasks/temporal_graph/models.py`), pydantic rejects
/// the structured output, and the search fails. Only dates whose fields are
/// each in range but do not exist together (`2021-02-29`) are
/// `invalid_interval` on both sides. A malformed window from the model is a
/// question without a usable date, not a provider outage, so Rust keeps
/// answering it.
pub(crate) fn classify_interval(interval: &QueryInterval) -> ExtractedWindow {
    let (Ok(start), Ok(end)) = (
        boundary_datetime(interval.starts_at.as_ref()),
        boundary_datetime(interval.ends_at.as_ref()),
    ) else {
        return ExtractedWindow::NoWindow(TemporalFallbackReason::InvalidInterval);
    };
    match (start, end) {
        (None, None) => ExtractedWindow::NoWindow(TemporalFallbackReason::NoTimeConstraint),
        (Some(start), Some(end)) if start >= end => {
            ExtractedWindow::NoWindow(TemporalFallbackReason::InvalidInterval)
        }
        (start, end) => ExtractedWindow::Window { start, end },
    }
}

/// Ask the LLM for the question's window. Port of Python's
/// `extract_query_interval`.
///
/// An LLM failure propagates, as in Python (`acreate_structured_output`
/// raises through the retriever): a provider outage is not a question without
/// a date. The Event-based retriever this replaced swallowed it into a
/// fallback answer.
pub(crate) async fn extract_query_interval(
    llm: &dyn Llm,
    query: &str,
    options: Option<GenerationOptions>,
) -> Result<ExtractedWindow, SearchError> {
    let interval = llm
        .create_structured_output_with_messages::<QueryInterval>(
            vec![
                Message::system(QUERY_INTERVAL_PROMPT.to_string()),
                Message::user(query.to_string()),
            ],
            options,
        )
        .await
        .map_err(|error| SearchError::LlmError(error.to_string()))?;
    Ok(classify_interval(&interval))
}

/// A window boundary in ms since the epoch. Port of `to_epoch_ms`.
pub(crate) fn to_epoch_ms(moment: Option<DateTime<Utc>>) -> Option<i64> {
    moment.map(|moment| moment.timestamp_millis())
}

/// The window's length in ms; infinite when either side is open. Port of
/// `window_span_ms`.
pub(crate) fn window_span_ms(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>) -> f64 {
    match (to_epoch_ms(start), to_epoch_ms(end)) {
        (Some(start), Some(end)) => (end - start) as f64,
        _ => f64::INFINITY,
    }
}

/// How long a `Timestamp` node's period is, in ms. Port of
/// `timestamp_span_ms`; a node without a parseable `time_at` never reaches
/// here ([`WindowTimestamp`] requires one), so the span is always finite.
pub(crate) fn timestamp_span_ms(node: &WindowTimestamp) -> f64 {
    node.time_until.saturating_sub(node.time_at) as f64
}

/// The window's timestamps ordered by span, shortest first, then by time,
/// then by id: the day node before the month before the bare year that also
/// overlaps, and within one precision in chronological order. Port of
/// `tightest_first`.
pub(crate) fn tightest_first(mut in_window: Vec<WindowTimestamp>) -> Vec<WindowTimestamp> {
    in_window.sort_by(|a, b| {
        timestamp_span_ms(a)
            .total_cmp(&timestamp_span_ms(b))
            .then_with(|| a.time_at.cmp(&b.time_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    in_window
}

/// Rank anchored chunks by how well the tightest timestamp each reached fits
/// the window (lower first). Port of `tightness_rank`.
///
/// A day-precision timestamp spans a day, a bare year a year. A timestamp at
/// least as precise as the question fits it fully, so every span up to
/// `window_span_ms` ranks the same and the fetch's similarity order decides
/// among them: for "in 2020" a paragraph dated September 2020 is not pushed
/// below a row dated 2020-09-14. Only a timestamp coarser than the window
/// ranks lower, the wider the later: for "18 March 1965" a chunk that only
/// says "1965" comes after one dated that day. An open-ended window (infinite
/// span) names no unit, so all anchored chunks rank the same. A chunk whose
/// timestamps are not in `in_window` ranks last.
pub(crate) fn tightness_rank(
    chunk_timestamps: &HashMap<String, HashSet<String>>,
    in_window: &[WindowTimestamp],
    window_span_ms: f64,
) -> HashMap<String, f64> {
    let span_by_id: HashMap<&str, f64> = in_window
        .iter()
        .map(|node| (node.id.as_str(), timestamp_span_ms(node)))
        .collect();
    chunk_timestamps
        .iter()
        .map(|(chunk_id, timestamp_ids)| {
            let tightest = timestamp_ids
                .iter()
                .map(|id| {
                    span_by_id
                        .get(id.as_str())
                        .copied()
                        .unwrap_or(f64::INFINITY)
                })
                .fold(f64::INFINITY, f64::min);
            (chunk_id.clone(), tightest.max(window_span_ms))
        })
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    //! Ports of the interval and tightness cases in Python's
    //! `tests/unit/modules/retrieval/test_temporal_hybrid_retriever.py`.

    use chrono::TimeZone;
    use serde_json::json;

    use super::*;

    const DAY: i64 = 86_400_000;
    const YEAR: i64 = 365 * DAY;

    fn utc(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
    }

    fn fields(year: u16, month: u8, day: u8) -> RawExtractedTimestamp {
        serde_json::from_value(json!({"year": year, "month": month, "day": day})).unwrap()
    }

    fn interval(
        start: Option<RawExtractedTimestamp>,
        end: Option<RawExtractedTimestamp>,
    ) -> QueryInterval {
        QueryInterval {
            starts_at: start,
            ends_at: end,
        }
    }

    fn node(id: &str, time_at: i64, time_until: i64) -> WindowTimestamp {
        WindowTimestamp {
            id: id.to_string(),
            timestamp_str: None,
            time_at,
            time_until,
        }
    }

    #[test]
    fn classify_interval_matches_python_cases() {
        use ExtractedWindow::{NoWindow, Window};
        use TemporalFallbackReason::{InvalidInterval, NoTimeConstraint};
        let cases = [
            (
                interval(Some(fields(1950, 1, 1)), Some(fields(1951, 1, 1))),
                Window {
                    start: Some(utc(1950, 1, 1)),
                    end: Some(utc(1951, 1, 1)),
                },
            ),
            (
                interval(None, Some(fields(1950, 1, 1))),
                Window {
                    start: None,
                    end: Some(utc(1950, 1, 1)),
                },
            ),
            (
                interval(Some(fields(1951, 1, 1)), None),
                Window {
                    start: Some(utc(1951, 1, 1)),
                    end: None,
                },
            ),
            // Both sides open: the question names no time.
            (interval(None, None), NoWindow(NoTimeConstraint)),
            // Start after end, or equal to it: no window.
            (
                interval(Some(fields(1951, 1, 1)), Some(fields(1950, 1, 1))),
                NoWindow(InvalidInterval),
            ),
            (
                interval(Some(fields(1950, 1, 1)), Some(fields(1950, 1, 1))),
                NoWindow(InvalidInterval),
            ),
            // A day that does not exist.
            (
                interval(Some(fields(2021, 2, 29)), None),
                NoWindow(InvalidInterval),
            ),
            (
                interval(Some(fields(0, 1, 1)), None),
                NoWindow(InvalidInterval),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(classify_interval(&input), expected, "{input:?}");
        }
    }

    #[test]
    fn query_interval_reads_nulls_and_missing_sides_as_open() {
        let parsed: QueryInterval =
            serde_json::from_value(json!({"starts_at": null, "ends_at": {"year": 1965}})).unwrap();
        assert!(parsed.starts_at.is_none());
        assert_eq!(
            classify_interval(&parsed),
            ExtractedWindow::Window {
                start: None,
                end: Some(utc(1965, 1, 1)),
            }
        );
        let empty: QueryInterval = serde_json::from_value(json!({})).unwrap();
        assert_eq!(
            classify_interval(&empty),
            ExtractedWindow::NoWindow(TemporalFallbackReason::NoTimeConstraint)
        );
    }

    #[test]
    fn epoch_ms_and_window_span() {
        assert_eq!(to_epoch_ms(None), None);
        assert_eq!(
            to_epoch_ms(Some(Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 1).unwrap())),
            Some(1000)
        );
        let (start, end) = (Some(utc(2020, 1, 1)), Some(utc(2021, 1, 1)));
        assert_eq!(window_span_ms(start, end), (366 * DAY) as f64); // leap year
        assert_eq!(window_span_ms(start, None), f64::INFINITY);
        assert_eq!(window_span_ms(None, end), f64::INFINITY);
    }

    #[test]
    fn tightest_first_orders_by_precision_then_time() {
        let nodes = vec![
            node("year", 0, 365 * DAY),
            node("day_late", 200 * DAY, 201 * DAY),
            node("day_early", 10 * DAY, 11 * DAY),
            node("month", 0, 31 * DAY),
        ];
        let ids: Vec<String> = tightest_first(nodes).into_iter().map(|n| n.id).collect();
        assert_eq!(ids, ["day_early", "day_late", "month", "year"]);
    }

    fn chunk_timestamps(pairs: &[(&str, &[&str])]) -> HashMap<String, HashSet<String>> {
        pairs
            .iter()
            .map(|(chunk, timestamps)| {
                (
                    (*chunk).to_string(),
                    timestamps.iter().map(|ts| (*ts).to_string()).collect(),
                )
            })
            .collect()
    }

    /// An exact date ranks before a year-only one for a day question.
    #[test]
    fn tightness_rank_puts_dates_coarser_than_a_day_question_last() {
        let in_window = [
            node("ts_day", 0, DAY),
            node("ts_year", -(100 * DAY), -(100 * DAY) + YEAR),
            node("ts_legacy", 0, 1000), // a legacy second
        ];
        let rank = tightness_rank(
            &chunk_timestamps(&[
                ("c_day", &["ts_day", "ts_year"]),
                ("c_year", &["ts_year"]),
                ("c_legacy", &["ts_legacy"]),
                ("c_none", &["ts_x"]),
            ]),
            &in_window,
            DAY as f64,
        );
        // A second fits a day question as fully as the day does.
        assert_eq!(rank["c_legacy"], rank["c_day"]);
        assert!(rank["c_day"] < rank["c_year"]);
        assert_eq!(rank["c_none"], f64::INFINITY);
    }

    #[test]
    fn tightness_rank_ties_every_date_inside_a_year_question() {
        let in_window = [
            node("ts_day", 0, DAY),
            node("ts_month", 0, 30 * DAY),
            node("ts_year", 0, YEAR),
            node("ts_decade", 0, 10 * YEAR),
        ];
        let rank = tightness_rank(
            &chunk_timestamps(&[
                ("c_day", &["ts_day"]),
                ("c_month", &["ts_month"]),
                ("c_year", &["ts_year"]),
                ("c_decade", &["ts_decade"]),
            ]),
            &in_window,
            YEAR as f64,
        );
        assert_eq!(rank["c_day"], rank["c_month"]);
        assert_eq!(rank["c_month"], rank["c_year"]);
        assert!(rank["c_year"] < rank["c_decade"]);
    }

    #[test]
    fn tightness_rank_ties_everything_for_an_open_window() {
        let in_window = [node("ts_day", 0, DAY), node("ts_year", 0, YEAR)];
        let rank = tightness_rank(
            &chunk_timestamps(&[("c_day", &["ts_day"]), ("c_year", &["ts_year"])]),
            &in_window,
            f64::INFINITY,
        );
        assert_eq!(rank["c_day"], rank["c_year"]);
    }

    #[test]
    fn vendored_prompt_is_the_python_text() {
        assert!(QUERY_INTERVAL_PROMPT.starts_with("Extract one time window from the question.\n"));
        assert!(QUERY_INTERVAL_PROMPT.ends_with("Do not return more than one window.\n"));
        assert!(
            !QUERY_INTERVAL_PROMPT.contains("{time_now}"),
            "the window prompt must not depend on the clock"
        );
    }
}
