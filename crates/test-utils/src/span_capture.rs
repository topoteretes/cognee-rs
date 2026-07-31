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
//!
//! Spans are recorded **when they are created**, not when they close, and
//! later `Span::record` calls are written through to the already-stored entry.
//! That is what makes `spans()` deterministic the moment an instrumented call
//! returns: closing is not a usable trigger, because a span closes only when the
//! last reference to it drops and `sqlx-sqlite` holds a `Span::current()` clone
//! on its connection-worker thread past the point where the caller's `await`
//! resumes. See the `ACTIVE_STORES` rationale in this module for the full story.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{Layer, Registry};

/// One span as observed by `SpanCapture`, holding the field values recorded on
/// it so far. A span appears here as soon as it is created; it does not have to
/// have closed.
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

/// Registry of currently-active capture stores. The single process-global
/// subscriber (installed once by [`SpanCapture::install`]) fans every *newly
/// created* span out to each store here.
///
/// Why global rather than the old thread-local `set_default`: the earlier
/// implementation only saw spans emitted on the *installing* thread, so a span
/// opened on a tokio worker thread or on `sqlx`'s dedicated blocking SQLite
/// connection thread escaped capture — an intermittent "missing span" flake
/// under load. A process-global subscriber captures every thread. The suite
/// runs under `cargo nextest` (one test per process), so at any moment there is
/// exactly one active store and capture is exact. Under a `cargo test` fallback
/// (many tests per process) concurrent captures would share spans; that is the
/// only degradation and it never drops a span the assertions look for.
///
/// Why capture on creation rather than on close: a span closes only when the
/// *last* reference to it drops, and in `tracing_subscriber::Registry` a live
/// reference keeps the whole ancestor chain open. `sqlx-sqlite` sends a clone of
/// `Span::current()` to its connection-worker thread with every command and
/// drops it at the end of the worker's loop iteration — strictly *after* it has
/// already replied to the caller. So when an instrumented `async fn` returns,
/// the span it just left may still be held by that worker thread, and neither
/// it nor any of its parents has closed yet. A test that snapshots at that
/// moment saw everything except the outermost spans it cared about, which is
/// exactly what made `tutorial_seeder_emits_span` (and, more rarely, three of
/// its siblings) flake on a busy CI runner — topoteretes/cognee-rs#109.
/// Recording on creation and writing later `Span::record` values through to the
/// stored entry removes the dependence on close ordering entirely: the entry is
/// in the store before the instrumented call can return.
static ACTIVE_STORES: Mutex<Vec<SpanStore>> = Mutex::new(Vec::new());

/// Back-pointers from a span to its entry in every store that was active when
/// the span was created, so `on_record` can update the stored field values.
/// Held in the span's registry extensions.
///
/// The references are [`Weak`] on purpose. A span can outlive the guard that
/// captured it — that is the whole premise of this module — and a strong
/// reference here would keep the store (and every `CapturedSpan` in it) alive
/// for as long as some thread still holds a clone of the span, long after the
/// test that owned the store finished. `Weak` also makes deregistration
/// meaningful: once the guard drops, write-through stops instead of silently
/// mutating a store nobody will read.
struct CaptureSlots {
    slots: Vec<(Weak<Mutex<Vec<CapturedSpan>>>, usize)>,
}

/// Field visitor: collects a set of field values into a JSON map.
#[derive(Default)]
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

struct CaptureLayer;

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        // Snapshot the active stores first and release the registry lock before
        // touching anything else. The global subscriber is never uninstalled, so
        // this callback keeps running for every span in the process after the
        // last guard drops; bailing out here keeps that case free of field
        // serialization and of the span's extensions lock.
        //
        // No lock is ever held while another is acquired: this guard dies at the
        // end of the statement, each `store.lock()` guard at the end of its
        // block, and `extensions_mut()` is taken only after the loop. That, not
        // a global ordering, is what makes the two callbacks deadlock-free —
        // `on_record` takes extensions before a store, the opposite nesting.
        // lock poison is unrecoverable.
        let Ok(stores) = ACTIVE_STORES.lock().map(|s| s.clone()) else {
            return;
        };
        if stores.is_empty() {
            return;
        }

        let Some(span) = ctx.span(id) else { return };

        let mut pending = PendingFields::default();
        attrs.record(&mut pending);
        let mut captured = Some(CapturedSpan {
            name: span.name().to_string(),
            fields: pending.map,
        });

        let mut slots = Vec::with_capacity(stores.len());
        let last = stores.len() - 1;
        for (i, store) in stores.iter().enumerate() {
            // Hand the original to the last store instead of cloning for every
            // one; in the common single-store case that is no clone at all.
            let entry = if i == last {
                captured.take()
            } else {
                captured.clone()
            };
            let Some(entry) = entry else { continue };

            let idx = {
                let Ok(mut entries) = store.lock() else {
                    continue;
                };
                entries.push(entry);
                entries.len() - 1
            };
            slots.push((Arc::downgrade(store), idx));
        }

        span.extensions_mut().insert(CaptureSlots { slots });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };

        let mut recorded = PendingFields::default();
        values.record(&mut recorded);
        if recorded.map.is_empty() {
            return;
        }

        // Upgrade the back-pointers out of the extensions guard so it is
        // released before any store lock is taken (see the note in
        // `on_new_span`: no lock is held while another is acquired). A `None`
        // upgrade means the owning guard has dropped, so the store is gone and
        // this write has no reader.
        let Some(stores) = span.extensions().get::<CaptureSlots>().map(|s| {
            s.slots
                .iter()
                .filter_map(|(store, idx)| store.upgrade().map(|store| (store, *idx)))
                .collect::<Vec<_>>()
        }) else {
            return;
        };

        for (store, idx) in stores {
            // lock poison is unrecoverable
            if let Ok(mut entries) = store.lock()
                && let Some(entry) = entries.get_mut(idx)
            {
                for (key, value) in &recorded.map {
                    entry.fields.insert(key.clone(), value.clone());
                }
            }
        }
    }

    fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {
        // Events are not captured; only spans.
    }

    // Deliberately no `on_close`: spans are recorded at creation. Closing is
    // not observable in time here — see the `ACTIVE_STORES` rationale above.
}

/// Guard tying a capture store's lifetime to a test. Registered in
/// `ACTIVE_STORES` on [`SpanCapture::install`] and removed on drop, so a
/// test's spans stop being collected once its guard goes out of scope.
///
/// The store is **append-only**: `CaptureSlots` holds each span's index into it,
/// so anything that removes, drains, truncates or reorders entries would
/// silently redirect later field writes to the wrong span. Add to the end only.
pub struct SpanCaptureGuard {
    store: SpanStore,
}

impl SpanCaptureGuard {
    /// Snapshot of every span created so far, each carrying the field values
    /// recorded on it up to this point.
    ///
    /// A span does not need to have closed to appear here, so this is already
    /// populated the moment an instrumented call returns — which is the point.
    ///
    /// Two consequences worth knowing. Entries are appended at span *creation*,
    /// so a parent precedes its children (the reverse of a close-ordered log),
    /// but the order is only approximately creation order: concurrent span
    /// creation on several threads races on the store lock, so no assertion
    /// should depend on the position of an entry. And because an entry appears
    /// before the instrumented body runs, fields declared
    /// `tracing::field::Empty` and filled later are absent until their
    /// `Span::record` lands — match on a span whose operation you have already
    /// awaited, and prefer `iter().any(..)` with the field in the predicate over
    /// `find`-by-name-then-assert when several spans share a name.
    pub fn spans(&self) -> Vec<CapturedSpan> {
        // lock poison is unrecoverable
        self.store.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for SpanCaptureGuard {
    fn drop(&mut self) {
        // Deregister this store (by pointer identity) so later spans in the
        // process are not appended to it. lock poison is unrecoverable.
        if let Ok(mut stores) = ACTIVE_STORES.lock() {
            stores.retain(|s| !Arc::ptr_eq(s, &self.store));
        }
    }
}

/// Stateless installer.
pub struct SpanCapture;

impl SpanCapture {
    /// Register a fresh capture store and ensure the process-global capture
    /// subscriber is installed. The returned guard collects every span that is
    /// **created** while it is alive — from **any** thread the test touches
    /// (tokio workers, `sqlx`'s blocking SQLite thread) — and deregisters on
    /// drop.
    ///
    /// Install this before the code under test runs: a span created earlier is
    /// never captured, even if it is still open.
    ///
    /// The subscriber is installed exactly once per process via
    /// [`set_global_default`](tracing::subscriber::set_global_default); under
    /// `cargo nextest` (one test per process) that means one active store at a
    /// time and exact capture.
    pub fn install() -> SpanCaptureGuard {
        static INIT: OnceLock<()> = OnceLock::new();
        INIT.get_or_init(|| {
            let subscriber = Registry::default().with(CaptureLayer);
            // If a global default is somehow already set we cannot replace it;
            // capture then sees nothing rather than panicking. No code in these
            // test binaries installs a competing global subscriber.
            let _ = tracing::subscriber::set_global_default(subscriber);
        });

        let store: SpanStore = Arc::new(Mutex::new(Vec::new()));
        // lock poison is unrecoverable
        if let Ok(mut stores) = ACTIVE_STORES.lock() {
            stores.push(Arc::clone(&store));
        }
        SpanCaptureGuard { store }
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

    /// Regression test for topoteretes/cognee-rs#109.
    ///
    /// Reproduces the shape of the flake without the timing: `sqlx-sqlite`
    /// hands a clone of `Span::current()` to its connection-worker thread and
    /// drops it only after it has replied to the caller, so an instrumented
    /// `async fn` can return while its span is still referenced elsewhere and
    /// therefore still open. Capture must not wait for the close.
    #[test]
    fn captures_span_still_open_because_a_reference_is_held_elsewhere() {
        let capture = SpanCapture::install();

        let span = info_span!("cognee.test.still_open", value = tracing::field::Empty);
        span.record("value", 5i64);
        // Stand in for the worker thread's clone: this keeps the span's
        // reference count above zero, so `on_close` never fires for it.
        let held_elsewhere = span.clone();
        drop(span);

        let spans = capture.spans();
        let s = spans
            .iter()
            .find(|s| s.name == "cognee.test.still_open")
            .expect("an open span must be captured before it closes");
        assert_eq!(s.field_i64("value"), Some(5));

        drop(held_elsewhere);
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

    /// Two guards alive at once must each get the span *and* their own index
    /// into their own store. The stores are at different lengths here, so a
    /// scheme that reused one index across stores would write the late field
    /// into the wrong entry (or none) in the shorter one.
    #[test]
    fn fans_out_to_every_active_store_with_per_store_indices() {
        let first = SpanCapture::install();
        // Only `first` sees this one, so the two stores diverge in length and
        // the shared span lands at a different index in each.
        info_span!("cognee.test.first_only", value = tracing::field::Empty).record("value", 1i64);

        let second = SpanCapture::install();
        let shared = info_span!("cognee.test.shared", value = tracing::field::Empty);
        shared.record("value", 2i64);

        for (label, spans) in [("first", first.spans()), ("second", second.spans())] {
            let s = spans
                .iter()
                .find(|s| s.name == "cognee.test.shared")
                .unwrap_or_else(|| panic!("{label} store missing the shared span"));
            assert_eq!(
                s.field_i64("value"),
                Some(2),
                "{label} store got the write-through at the wrong index",
            );
        }

        // The span created before `second` installed is absent from it — spans
        // are captured at creation, so an earlier span is never back-filled.
        assert!(
            !second
                .spans()
                .iter()
                .any(|s| s.name == "cognee.test.first_only")
        );
        assert!(
            first
                .spans()
                .iter()
                .any(|s| s.name == "cognee.test.first_only")
        );
    }

    /// Dropping the guard must release the store even while a span captured into
    /// it is still open, and must stop write-through into it. This is what the
    /// `Weak` in `CaptureSlots` buys: a strong reference would pin the store for
    /// as long as any thread held a clone of the span.
    #[test]
    fn guard_drop_releases_the_store_even_with_a_span_still_open() {
        let capture = SpanCapture::install();
        let weak = Arc::downgrade(&capture.store);

        let span = info_span!("cognee.test.outlives_guard", value = tracing::field::Empty);
        let held_elsewhere = span.clone();
        drop(span);

        drop(capture);

        // `on_new_span` clones the store list, so a sibling test creating a span
        // right now can hold a transient strong reference. Retry rather than
        // assert on the first observation — the point is that nothing holds the
        // store *durably*, which a strong ref in `CaptureSlots` would.
        let released = (0..100).any(|_| {
            if weak.upgrade().is_none() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        });
        assert!(
            released,
            "store must be freed on guard drop, not pinned by the open span",
        );

        // Write-through after deregistration must be a no-op rather than a panic
        // or a write into an orphaned store.
        held_elsewhere.record("value", 9i64);
        drop(held_elsewhere);
    }
}
