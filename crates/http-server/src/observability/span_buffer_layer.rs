//! `tracing::Layer` that captures every span into a [`SpanBuffer`].
//!
//! Trace ids are synthesized: `tracing` does not have native OTEL ids, so the
//! layer assigns a fresh 32-char lowercase-hex `trace_id` per root span and
//! propagates it to children via the span's local `extensions_mut()` slot.
//! `parent_span_id` is taken from the parent `TraceCtx` so the buffer's view
//! matches Python's exporter byte-for-byte.

use std::time::SystemTime;

use rand::RngCore;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use super::redaction::redact_attributes;
use super::span_buffer::{RecordedSpan, SpanBuffer, SpanStatus};

/// Per-span context attached via `extensions_mut()`.
#[derive(Clone, Debug)]
struct TraceCtx {
    trace_id: String,
    span_id: String,
    parent_span_id: Option<String>,
    start_time_ns: u64,
    attributes: serde_json::Map<String, serde_json::Value>,
    status: SpanStatus,
}

/// `tracing` layer that captures every span into a [`SpanBuffer`].
pub struct SpanBufferLayer {
    buffer: SpanBuffer,
}

impl SpanBufferLayer {
    /// Build a new layer feeding `buffer`.
    pub fn new(buffer: SpanBuffer) -> Self {
        Self { buffer }
    }
}

impl<S> Layer<S> for SpanBufferLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let span_id = random_hex(8);
        let (trace_id, parent_span_id) =
            match attrs.parent().and_then(|pid| ctx.span(pid)).or_else(|| {
                if attrs.is_contextual() {
                    ctx.lookup_current()
                } else {
                    None
                }
            }) {
                Some(parent_ref) => {
                    let exts = parent_ref.extensions();
                    match exts.get::<TraceCtx>() {
                        Some(parent_ctx) => (
                            parent_ctx.trace_id.clone(),
                            Some(parent_ctx.span_id.clone()),
                        ),
                        None => (random_hex(16), None),
                    }
                }
                None => (random_hex(16), None),
            };

        let mut visitor = AttrCollector::default();
        attrs.record(&mut visitor);

        let trace_ctx = TraceCtx {
            trace_id,
            span_id,
            parent_span_id,
            start_time_ns: now_ns(),
            attributes: visitor.into_map(),
            status: SpanStatus::Unset,
        };

        if let Some(span_ref) = ctx.span(id) {
            span_ref.extensions_mut().insert(trace_ctx);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span_ref) = ctx.span(id) {
            let mut exts = span_ref.extensions_mut();
            if let Some(trace_ctx) = exts.get_mut::<TraceCtx>() {
                let mut visitor = AttrCollector::default();
                values.record(&mut visitor);
                for (k, v) in visitor.into_map() {
                    trace_ctx.attributes.insert(k, v);
                }
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Promote the parent span's status to ERROR when an `error!` event
        // fires inside it. Mirrors Python's exporter behavior.
        if *event.metadata().level() != Level::ERROR {
            return;
        }
        if let Some(span_ref) = ctx.event_span(event) {
            let mut exts = span_ref.extensions_mut();
            if let Some(trace_ctx) = exts.get_mut::<TraceCtx>() {
                trace_ctx.status = SpanStatus::Error;
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span_ref) = ctx.span(&id) else {
            return;
        };
        let metadata = span_ref.metadata();
        let trace_ctx_opt = span_ref.extensions_mut().remove::<TraceCtx>();
        let Some(mut trace_ctx) = trace_ctx_opt else {
            // Foreign span (created by another layer) — drop silently.
            return;
        };

        let end_time_ns = now_ns();
        let duration_ns = end_time_ns.saturating_sub(trace_ctx.start_time_ns);
        let duration_ms = duration_ns as f64 / 1_000_000.0;

        redact_attributes(&mut trace_ctx.attributes);

        let recorded = RecordedSpan {
            trace_id: trace_ctx.trace_id,
            span_id: trace_ctx.span_id,
            parent_span_id: trace_ctx.parent_span_id,
            name: metadata.name().to_string(),
            start_time_ns: trace_ctx.start_time_ns,
            end_time_ns,
            duration_ms,
            status: if trace_ctx.status == SpanStatus::Unset {
                // Python's exporter normalizes "unset on close" → OK.
                SpanStatus::Ok
            } else {
                trace_ctx.status
            },
            attributes: trace_ctx.attributes,
        };
        self.buffer.record(recorded);
    }
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn now_ns() -> u64 {
    SystemTime::UNIX_EPOCH
        .elapsed()
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn random_hex(byte_len: usize) -> String {
    // 16-byte buf covers both 16-byte trace ids and 8-byte span ids.
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf[..byte_len]);
    buf[..byte_len].iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Default)]
struct AttrCollector {
    map: serde_json::Map<String, serde_json::Value>,
}

impl AttrCollector {
    fn into_map(self) -> serde_json::Map<String, serde_json::Value> {
        self.map
    }
}

impl Visit for AttrCollector {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.map
            .insert(field.name().to_string(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if let Some(num) = serde_json::Number::from_f64(value) {
            self.map
                .insert(field.name().to_string(), serde_json::Value::Number(num));
        } else {
            self.map.insert(
                field.name().to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Value::String(format!("{value:?}")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn parent_and_children_share_trace_id() {
        let buffer = SpanBuffer::default();
        let layer = SpanBufferLayer::new(buffer.clone());
        let subscriber = Registry::default().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let parent = tracing::span!(Level::INFO, "parent");
            let _g = parent.enter();
            {
                let child = tracing::span!(Level::INFO, "child1");
                let _gc = child.enter();
            }
            {
                let child = tracing::span!(Level::INFO, "child2");
                let _gc = child.enter();
            }
        });

        let traces = buffer.all_traces();
        assert_eq!(traces.len(), 1, "all spans share one trace");
        let summary = &traces[0];
        assert_eq!(summary.span_count, 3);
        let trace_id = summary.trace_id.clone();
        for s in &summary.spans {
            assert_eq!(s.trace_id, trace_id, "every span uses same trace_id");
        }
        // Find the parent span and assert children reference it.
        let parent_span = summary
            .spans
            .iter()
            .find(|s| s.parent_span_id.is_none())
            .expect("root present");
        for s in &summary.spans {
            if s.span_id != parent_span.span_id {
                assert_eq!(
                    s.parent_span_id.as_deref(),
                    Some(parent_span.span_id.as_str())
                );
            }
        }
    }

    #[test]
    fn recorded_attributes_are_redacted() {
        let buffer = SpanBuffer::default();
        let layer = SpanBufferLayer::new(buffer.clone());
        let subscriber = Registry::default().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::span!(
                Level::INFO,
                "request",
                auth = "Authorization: Bearer eyJabc.def.ghi-very-long-jwt-1234567890"
            );
            let _g = span.enter();
        });

        let traces = buffer.all_traces();
        assert_eq!(traces.len(), 1);
        let span = traces[0]
            .spans
            .iter()
            .find(|s| s.name == "request")
            .expect("request span recorded");
        let auth = span
            .attributes
            .get("auth")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(auth.contains("***REDACTED***"));
        assert!(!auth.contains("ghi-very-long-jwt"));
    }

    #[test]
    fn error_event_marks_status_error() {
        let buffer = SpanBuffer::default();
        let layer = SpanBufferLayer::new(buffer.clone());
        let subscriber = Registry::default().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::span!(Level::INFO, "task");
            let _g = span.enter();
            tracing::error!("failed to do thing");
        });

        let traces = buffer.all_traces();
        let span = traces[0]
            .spans
            .iter()
            .find(|s| s.name == "task")
            .expect("task span");
        assert_eq!(span.status, SpanStatus::Error);
    }
}
