//! Capture `tracing` spans during a test for structured attribute
//! assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test infrastructure — panics are acceptable"
)]
//!
//! Usage:
//!
//! ```rust,ignore
//! use cognee_test_utils::SpanCapture;
//!
//! #[tokio::test]
//! async fn ladybug_query_emits_span() {
//!     let capture = SpanCapture::install();
//!     let adapter = test_adapter().await;
//!     adapter.execute_query("MATCH (n:Node) RETURN n").unwrap();
//!     let spans = capture.spans();
//!     let s = spans
//!         .iter()
//!         .find(|s| s.name == "cognee.db.graph.query")
//!         .expect("expected query span");
//!     assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("ladybug"));
//!     assert_eq!(s.field_i64("cognee.db.row_count"), Some(0));
//! }
//! ```
//!
//! The guard returned from `install()` restores the previous tracing
//! dispatcher on drop, so parallel tests do not leak subscribers.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{Layer, Registry};

/// One completed span as observed by `SpanCapture`.
#[derive(Clone, Debug)]
pub struct CapturedSpan {
    pub name: String,
    pub fields: Map<String, Value>,
}

impl CapturedSpan {
    /// Read a string-typed field (also works for any field whose
    /// `Debug` representation is a quoted string literal — `tracing`
    /// records non-string `display`/`debug` values as JSON strings
    /// in the underlying map).
    pub fn field_str(&self, key: &str) -> Option<String> {
        match self.fields.get(key)? {
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    }

    /// Read an integer-typed field. Returns `None` if absent or not
    /// an integer.
    pub fn field_i64(&self, key: &str) -> Option<i64> {
        self.fields.get(key)?.as_i64()
    }

    /// Read a boolean-typed field.
    pub fn field_bool(&self, key: &str) -> Option<bool> {
        self.fields.get(key)?.as_bool()
    }
}

/// Shared state between the layer and the guard.
type SpanStore = Arc<Mutex<Vec<CapturedSpan>>>;

#[derive(Default, Clone)]
struct PendingFields {
    map: Map<String, Value>,
}

impl Visit for PendingFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.map
            .insert(field.name().to_string(), Value::String(value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.map
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.map
            .insert(field.name().to_string(), Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.map
            .insert(field.name().to_string(), Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.map.insert(
            field.name().to_string(),
            serde_json::Number::from_f64(value)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Mirror `tracing`'s default rendering: `format!("{:?}", value)`.
        self.map.insert(
            field.name().to_string(),
            Value::String(format!("{value:?}")),
        );
    }
}

struct CaptureLayer {
    store: SpanStore,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Stash the initial field values onto the span's extension
        // so we can mutate them via `on_record` and read them back on
        // close.
        let mut pending = PendingFields::default();
        attrs.record(&mut pending);
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            ext.insert(pending);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id) {
            let mut ext = span.extensions_mut();
            if let Some(pending) = ext.get_mut::<PendingFields>() {
                values.record(pending);
            }
        }
    }

    fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {
        // Events are not captured; only spans.
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(&id) {
            let name = span.name().to_string();
            let fields = span
                .extensions()
                .get::<PendingFields>()
                .cloned()
                .unwrap_or_default()
                .map;
            // lock poison is unrecoverable
            if let Ok(mut store) = self.store.lock() {
                store.push(CapturedSpan { name, fields });
            }
        }
    }
}

/// Install a span-capturing subscriber as the default for the
/// current thread *and* for any tasks spawned on the current
/// `tokio` runtime. The previous default is restored when the
/// returned guard is dropped.
pub struct SpanCaptureGuard {
    store: SpanStore,
    _dispatch: tracing::dispatcher::DefaultGuard,
}

impl SpanCaptureGuard {
    /// Snapshot of all spans closed so far.
    pub fn spans(&self) -> Vec<CapturedSpan> {
        // lock poison is unrecoverable
        self.store.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// Stateless installer.
pub struct SpanCapture;

impl SpanCapture {
    /// Install the capture layer as the **thread-local** default
    /// dispatcher (via `set_default`). The returned guard restores
    /// the previous dispatcher on drop. Safe to call concurrently
    /// from multiple `#[tokio::test]` functions.
    pub fn install() -> SpanCaptureGuard {
        let store: SpanStore = Arc::new(Mutex::new(Vec::new()));
        let layer = CaptureLayer {
            store: Arc::clone(&store),
        };
        let subscriber = Registry::default().with(layer);
        let dispatch = tracing::dispatcher::set_default(&subscriber.into());
        SpanCaptureGuard {
            store,
            _dispatch: dispatch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::{info_span, instrument};

    #[test]
    fn captures_span_name_and_fields() {
        let capture = SpanCapture::install();
        let span = info_span!(
            "cognee.db.graph.query",
            cognee.db.system = "ladybug",
            cognee.db.row_count = tracing::field::Empty,
        );
        span.record("cognee.db.row_count", 7i64);
        let _enter = span.enter();
        drop(_enter);
        drop(span);

        let spans = capture.spans();
        let s = spans
            .iter()
            .find(|s| s.name == "cognee.db.graph.query")
            .expect("expected query span");
        assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("ladybug"));
        assert_eq!(s.field_i64("cognee.db.row_count"), Some(7));
    }

    #[instrument(name = "cognee.test.fn", skip_all, fields(value = tracing::field::Empty))]
    fn produce_span(v: i64) {
        tracing::Span::current().record("value", v);
    }

    #[test]
    fn captures_instrument_macro_spans() {
        let capture = SpanCapture::install();
        produce_span(42);
        let spans = capture.spans();
        assert!(
            spans
                .iter()
                .any(|s| s.name == "cognee.test.fn" && s.field_i64("value") == Some(42))
        );
    }
}
