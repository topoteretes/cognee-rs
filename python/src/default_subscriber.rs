//! Default `tracing` subscriber for the PyO3 binding (gap 07 task 02).
//!
//! Installed automatically on first import of the `_native` extension
//! module. Routes Rust `tracing` events into Python's standard
//! `logging` module via [`pyo3_log`]. Hosts that already configured
//! their own subscriber, or that set
//! `COGNEE_BINDING_SUPPRESS_LOGS=<non-empty>`, get a no-op.
//!
//! This is the minimal "events are never silently dropped" install
//! mandated by gap-07 decision 1. `setup_logging()` (gap 06 task 08)
//! and the future `setup_telemetry()` (gap 07 task 05) continue to
//! layer on top via `tracing_subscriber::Registry::try_init`
//! semantics: only the first init installs; later calls are observed
//! via the singleton guards on the Python side.
//!
//! ## Routing path
//!
//! ```text
//! tracing::event!  →  Registry  →  TracingToLogLayer
//!                                  └─ log::logger().log(&Record)
//!                                       └─ pyo3_log::Logger (global)
//!                                            └─ Python `logging`
//! ```
//!
//! `pyo3_log::try_init()` installs `pyo3_log::Logger` as the global
//! `log::Log` implementation. Tracing events are NOT auto-forwarded
//! to `log` unless the `tracing/log-always` feature is enabled (it is
//! not in our workspace pin), so [`TracingToLogLayer`] explicitly
//! bridges every tracing event into a [`log::Record`] handed to the
//! global logger.

use std::fmt::Write as _;
use std::sync::Once;

use pyo3::prelude::*;
use tracing::field::{Field, Visit};
use tracing_log::AsLog;
use tracing_subscriber::{
    EnvFilter, Layer, Registry, layer::SubscriberExt, util::SubscriberInitExt,
};

static INIT: Once = Once::new();

/// Install the default bridge subscriber. Idempotent.
///
/// Honours `COGNEE_BINDING_SUPPRESS_LOGS=<any non-empty>` as opt-out.
/// The `py` handle is accepted (and currently unused) to keep the
/// signature future-proof against `pyo3-log` versions that require a
/// `Python<'_>` for logger construction.
pub(crate) fn install(py: Python<'_>) {
    let _ = py;
    INIT.call_once(|| {
        if std::env::var_os("COGNEE_BINDING_SUPPRESS_LOGS")
            .filter(|v| !v.is_empty())
            .is_some()
        {
            return;
        }

        // (1) Install pyo3-log as the global `log::Log` impl. If
        //     another `log` impl is already installed (host owns
        //     logging), `try_init` returns Err — we ignore it and the
        //     host's prior setup wins.
        let _ = pyo3_log::try_init();

        // (2) Build EnvFilter: RUST_LOG > cognee_logging::default_filter().
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(cognee_logging::default_filter()));

        // (3) Compose a Registry with the env filter and the explicit
        //     tracing → log forwarder. `try_init` is soft: if a host
        //     already installed a `tracing::Subscriber`, theirs wins
        //     and our layer is dropped on the floor.
        let _ = Registry::default()
            .with(filter)
            .with(TracingToLogLayer)
            .try_init();
    });
}

/// `tracing_subscriber::Layer` that converts every event into a
/// `log::Record` and dispatches it through the global `log::Log`
/// implementation (which `pyo3_log::try_init` set to forward into
/// Python's `logging` module).
struct TracingToLogLayer;

impl<S> Layer<S> for TracingToLogLayer
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();
        let log_level = metadata.level().as_log();

        // Cheap interest-check: skip building the record entirely if
        // the global logger doesn't care at this target/level.
        let logger = log::logger();
        let log_meta = log::Metadata::builder()
            .level(log_level)
            .target(metadata.target())
            .build();
        if !logger.enabled(&log_meta) {
            return;
        }

        // Collect the message and any structured fields into a single
        // formatted body — `pyo3_log` ultimately calls Python
        // `logging.Logger.log(level, message)`, which is line-shaped.
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let body = visitor.finish();

        // `log::Record::args` borrows the `format_args!` temporary,
        // so the builder must live in a `let` binding that outlives
        // the call to `logger.log()`.
        let args = format_args!("{body}");
        let record = log::Record::builder()
            .args(args)
            .level(log_level)
            .target(metadata.target())
            .module_path(metadata.module_path())
            .file(metadata.file())
            .line(metadata.line())
            .build();
        logger.log(&record);
    }
}

/// Field visitor that concatenates `message` + `k=v` pairs into a
/// single string. Mirrors the shape of `tracing-log`'s internal
/// formatter so the resulting Python log line carries the same
/// information `tracing::info!("…", k = v)` would print in plain mode.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl MessageVisitor {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            self.message
        } else if self.message.is_empty() {
            self.fields
        } else {
            format!("{} {}", self.message, self.fields)
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // `write!` to a `String` cannot fail under normal allocation.
            let _ = write!(&mut self.message, "{value:?}");
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(&mut self.fields, "{}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(&mut self.fields, "{}={}", field.name(), value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_visitor_collects_message_and_fields() {
        // Construct a fake event by hand is awkward without a
        // subscriber — exercise the visitor surface directly.
        let body_only = MessageVisitor {
            message: "hello".into(),
            fields: String::new(),
        }
        .finish();
        assert_eq!(body_only, "hello");

        let with_fields = MessageVisitor {
            message: "hello".into(),
            fields: "k=v".into(),
        }
        .finish();
        assert_eq!(with_fields, "hello k=v");

        let fields_only = MessageVisitor {
            message: String::new(),
            fields: "k=v".into(),
        }
        .finish();
        assert_eq!(fields_only, "k=v");
    }
}
