//! Bounded in-memory ring buffer of recorded spans.
//!
//! Mirrors Python's `CogneeSpanExporter` storage model
//! (`cognee/modules/observability/tracing.py`): per-trace buckets, LRU
//! eviction once `max_traces` is exceeded, no per-span eviction beyond a
//! safety cap.

use std::collections::{HashMap, VecDeque};
use std::env;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Span status as exported via the wire shape.
///
/// Serialized in UPPERCASE so JSON roundtrips line up with Python's exporter
/// (`"OK" | "ERROR" | "UNSET"`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SpanStatus {
    #[default]
    Unset,
    Ok,
    Error,
}

impl SpanStatus {
    /// Wire string per the Python exporter (`"UNSET" | "OK" | "ERROR"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unset => "UNSET",
            Self::Ok => "OK",
            Self::Error => "ERROR",
        }
    }
}

/// A single span snapshot, frozen at the moment its tracing span closed.
///
/// Field shape mirrors the Python `CogneeSpanExporter.export(...)` dict
/// byte-for-byte so frontend trace viewers render identically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedSpan {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub start_time_ns: u64,
    pub end_time_ns: u64,
    pub duration_ms: f64,
    pub status: SpanStatus,
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// Configurable caps for the ring buffer.
#[derive(Debug, Clone)]
pub struct BufferConfig {
    /// How many distinct traces to retain before LRU eviction kicks in.
    pub max_traces: usize,
    /// Per-trace span cap (defense against pathological producers).
    pub max_spans_per_trace: usize,
}

impl Default for BufferConfig {
    fn default() -> Self {
        Self {
            max_traces: 50,
            max_spans_per_trace: 1024,
        }
    }
}

impl BufferConfig {
    /// Read from `COGNEE_SPAN_BUFFER_MAX_TRACES` /
    /// `COGNEE_SPAN_BUFFER_MAX_SPANS_PER_TRACE`. Invalid values fall back to
    /// the defaults.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(v) = env::var("COGNEE_SPAN_BUFFER_MAX_TRACES")
            && let Ok(n) = v.parse::<usize>()
        {
            cfg.max_traces = n;
        }
        if let Ok(v) = env::var("COGNEE_SPAN_BUFFER_MAX_SPANS_PER_TRACE")
            && let Ok(n) = v.parse::<usize>()
        {
            cfg.max_spans_per_trace = n;
        }
        cfg
    }
}

/// Per-trace summary surfaced via `/api/v1/activity/spans`.
#[derive(Debug, Clone)]
pub struct TraceSummary {
    pub trace_id: String,
    pub root_name: Option<String>,
    pub duration_ms: f64,
    pub span_count: usize,
    pub status: Option<SpanStatus>,
    pub spans: Vec<RecordedSpan>,
}

/// Lightweight stats surfaced alongside the span list.
#[derive(Debug, Clone, Default)]
pub struct BufferStats {
    /// Number of spans dropped because the per-trace cap was exceeded.
    pub dropped_overflow: u64,
    /// Number of traces evicted via the LRU cap.
    pub dropped_lru: u64,
}

struct BufferInner {
    traces: HashMap<String, Vec<RecordedSpan>>,
    /// Trace ids in insertion order; oldest at the front.
    trace_order: VecDeque<String>,
    stats: BufferStats,
}

/// Bounded in-memory span buffer.
///
/// Cheap to clone (interior `Arc<Mutex<...>>`).
#[derive(Clone)]
pub struct SpanBuffer {
    inner: Arc<Mutex<BufferInner>>,
    config: Arc<BufferConfig>,
}

impl Default for SpanBuffer {
    fn default() -> Self {
        Self::new(BufferConfig::default())
    }
}

impl SpanBuffer {
    /// Build a new buffer with the supplied config.
    pub fn new(config: BufferConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BufferInner {
                traces: HashMap::new(),
                trace_order: VecDeque::new(),
                stats: BufferStats::default(),
            })),
            config: Arc::new(config),
        }
    }

    /// Record one span.
    ///
    /// On overflow:
    /// - per-trace: drops the *new* span silently and bumps `dropped_overflow`.
    /// - cross-trace: when the trace is brand-new and pushes the count past
    ///   `max_traces`, evict the oldest trace whole and bump `dropped_lru`.
    pub fn record(&self, span: RecordedSpan) {
        let trace_id = span.trace_id.clone();
        // lock poison is unrecoverable
        let mut inner = self.inner.lock().unwrap();

        let is_new_trace = !inner.traces.contains_key(&trace_id);

        if is_new_trace {
            inner.traces.insert(trace_id.clone(), Vec::with_capacity(8));
            inner.trace_order.push_back(trace_id.clone());

            // LRU eviction once the new insertion pushed the count past the cap.
            while inner.trace_order.len() > self.config.max_traces {
                if let Some(oldest) = inner.trace_order.pop_front() {
                    inner.traces.remove(&oldest);
                    inner.stats.dropped_lru = inner.stats.dropped_lru.saturating_add(1);
                } else {
                    break;
                }
            }
        }

        let cap = self.config.max_spans_per_trace;
        if let Some(bucket) = inner.traces.get_mut(&trace_id) {
            if bucket.len() >= cap {
                inner.stats.dropped_overflow = inner.stats.dropped_overflow.saturating_add(1);
            } else {
                bucket.push(span);
            }
        }
    }

    /// Snapshot every trace, most-recent first.
    pub fn all_traces(&self) -> Vec<TraceSummary> {
        // lock poison is unrecoverable
        let inner = self.inner.lock().unwrap();
        let mut out = Vec::with_capacity(inner.trace_order.len());
        // Iterate trace_order in reverse so the most recent trace lands first.
        for trace_id in inner.trace_order.iter().rev() {
            if let Some(spans) = inner.traces.get(trace_id) {
                out.push(build_trace_summary(trace_id.clone(), spans.clone()));
            }
        }
        out
    }

    /// Most recent trace, if any.
    pub fn last_trace(&self) -> Option<TraceSummary> {
        // lock poison is unrecoverable
        let inner = self.inner.lock().unwrap();
        let trace_id = inner.trace_order.back()?.clone();
        let spans = inner.traces.get(&trace_id)?.clone();
        Some(build_trace_summary(trace_id, spans))
    }

    /// Drop every trace and reset stats.
    pub fn clear(&self) {
        // lock poison is unrecoverable
        let mut inner = self.inner.lock().unwrap();
        inner.traces.clear();
        inner.trace_order.clear();
        inner.stats = BufferStats::default();
    }

    /// Cumulative drop counters.
    pub fn stats(&self) -> BufferStats {
        // lock poison is unrecoverable
        let inner = self.inner.lock().unwrap();
        inner.stats.clone()
    }

    /// Configured cap on traces.
    pub fn config(&self) -> &BufferConfig {
        &self.config
    }
}

/// Pick a root span from a flat list, mirroring Python's selection rules.
///
/// Rule: the first span whose `parent_span_id` is `None`; if none, the first
/// span in the slice.
fn pick_root(spans: &[RecordedSpan]) -> Option<&RecordedSpan> {
    spans
        .iter()
        .find(|s| s.parent_span_id.is_none())
        .or_else(|| spans.first())
}

/// `duration_ms = max(s.duration_ms for s in spans)`. Matches Python
/// L86: `max((... for s in spans), default=0)`.
fn max_duration_ms(spans: &[RecordedSpan]) -> f64 {
    spans.iter().map(|s| s.duration_ms).fold(0.0_f64, f64::max)
}

fn build_trace_summary(trace_id: String, spans: Vec<RecordedSpan>) -> TraceSummary {
    let span_count = spans.len();
    let duration_ms = max_duration_ms(&spans);
    let (root_name, status) = match pick_root(&spans) {
        Some(root) => (Some(root.name.clone()), Some(root.status)),
        None => (None, None),
    };
    TraceSummary {
        trace_id,
        root_name,
        duration_ms,
        span_count,
        status,
        spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(trace: &str, span_id: &str, parent: Option<&str>, name: &str) -> RecordedSpan {
        RecordedSpan {
            trace_id: trace.into(),
            span_id: span_id.into(),
            parent_span_id: parent.map(|s| s.into()),
            name: name.into(),
            start_time_ns: 0,
            end_time_ns: 1_000_000,
            duration_ms: 1.0,
            status: SpanStatus::Ok,
            attributes: serde_json::Map::new(),
        }
    }

    #[test]
    fn record_then_snapshot_roundtrips() {
        let buf = SpanBuffer::default();
        buf.record(span("aa", "01", None, "root"));
        buf.record(span("aa", "02", Some("01"), "child"));

        let traces = buf.all_traces();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].trace_id, "aa");
        assert_eq!(traces[0].span_count, 2);
        assert_eq!(traces[0].root_name.as_deref(), Some("root"));
        assert!((traces[0].duration_ms - 1.0).abs() < 1e-9);
    }

    #[test]
    fn lru_evicts_oldest_trace_when_cap_exceeded() {
        let buf = SpanBuffer::new(BufferConfig {
            max_traces: 50,
            max_spans_per_trace: 8,
        });
        for i in 0..51_u32 {
            let trace = format!("{i:032x}");
            buf.record(span(&trace, "0000000000000001", None, "root"));
        }
        let traces = buf.all_traces();
        assert_eq!(traces.len(), 50);
        // Oldest trace was id 0 — must be gone.
        let zero = format!("{:032x}", 0);
        assert!(traces.iter().all(|t| t.trace_id != zero));
        // Most recent trace must be at the front.
        let newest = format!("{:032x}", 50);
        assert_eq!(traces[0].trace_id, newest);
        assert_eq!(buf.stats().dropped_lru, 1);
    }

    #[test]
    fn per_trace_cap_drops_silently() {
        let buf = SpanBuffer::new(BufferConfig {
            max_traces: 4,
            max_spans_per_trace: 2,
        });
        buf.record(span("aa", "01", None, "root"));
        buf.record(span("aa", "02", Some("01"), "c1"));
        buf.record(span("aa", "03", Some("01"), "c2"));
        buf.record(span("aa", "04", Some("01"), "c3"));

        let traces = buf.all_traces();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].span_count, 2);
        assert_eq!(buf.stats().dropped_overflow, 2);
    }

    #[test]
    fn trace_summary_picks_parentless_root() {
        let buf = SpanBuffer::default();
        buf.record(span("aa", "02", Some("01"), "child"));
        buf.record(span("aa", "01", None, "real_root"));

        let traces = buf.all_traces();
        assert_eq!(traces[0].root_name.as_deref(), Some("real_root"));
    }

    #[test]
    fn trace_summary_status_unset_when_root_unset() {
        let buf = SpanBuffer::default();
        let mut s = span("aa", "01", None, "root");
        s.status = SpanStatus::Unset;
        buf.record(s);

        let traces = buf.all_traces();
        assert_eq!(traces[0].status, Some(SpanStatus::Unset));
    }

    #[test]
    fn span_status_serializes_uppercase() {
        let json = serde_json::to_string(&SpanStatus::Ok).expect("serialize");
        assert_eq!(json, "\"OK\"");
        let json = serde_json::to_string(&SpanStatus::Error).expect("serialize");
        assert_eq!(json, "\"ERROR\"");
    }

    #[test]
    fn last_trace_returns_most_recent() {
        let buf = SpanBuffer::default();
        buf.record(span("aa", "01", None, "first"));
        buf.record(span("bb", "01", None, "second"));
        let last = buf.last_trace().expect("last trace");
        assert_eq!(last.trace_id, "bb");
        assert_eq!(last.root_name.as_deref(), Some("second"));
    }

    #[test]
    fn clear_resets_state() {
        let buf = SpanBuffer::default();
        buf.record(span("aa", "01", None, "root"));
        buf.clear();
        assert!(buf.all_traces().is_empty());
        assert_eq!(buf.stats().dropped_lru, 0);
    }
}
