//! Python-byte-exact plain text formatter.
//!
//! `PythonPlainFormatter` implements
//! [`tracing_subscriber::fmt::FormatEvent`] and produces lines that
//! match — byte-for-byte — the output of the Python
//! [`cognee.shared.logging_utils.PlainFileHandler.emit`](https://github.com/topoteretes/cognee/blob/main/cognee/shared/logging_utils.py#L150)
//! handler:
//!
//! ```text
//! <ts %Y-%m-%dT%H:%M:%S.%6f> [<LEVEL ljust(8)>] <msg>[ k1=v1 k2=v2 ...] [<target>]\n
//! ```
//!
//! ## Decision references
//!
//! - **Decision 4** — byte-exact Python format for cross-SDK parity.
//! - **Decision 12** — per-message-strict, per-filename-loose parity
//!   test reads the formatted line back and compares to Python's
//!   output.
//!
//! ## Notes
//!
//! - Timestamps use [`chrono::Local`] (naive local time, no offset)
//!   to mirror Python's `datetime.now().strftime(...)`.
//! - The level name maps directly to Python's `logging.Logger`
//!   level names, left-justified to width 8 with trailing spaces
//!   (e.g. `WARN` → `"WARNING "`).
//! - The `message` tracing field is emitted as the bare event body
//!   (no `message=` prefix); all other fields follow as
//!   space-separated ` k=v` pairs in the order `tracing` visits them.
//!   Python flattens an unordered structlog `event_dict`, so the
//!   cross-SDK parity test must normalise field order (sort or
//!   compare on the message prefix); single-field events compare
//!   byte-equal.
//! - The bracketed logger name is
//!   [`event.metadata().target()`](tracing::Metadata::target), which
//!   tracing populates from the module path or the explicit
//!   `target: "..."` event argument — analogous to Python's
//!   `logger.name`.

use std::fmt::Write as _;

use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, format::Writer};
use tracing_subscriber::registry::LookupSpan;

/// Python-byte-exact plain text formatter.
///
/// See the [module-level documentation](self) for the exact output
/// shape and parity guarantees.
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonPlainFormatter;

impl<S, N> FormatEvent<S, N> for PythonPlainFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        // Naive local time with 6-digit microsecond precision — matches
        // Python's `datetime.now().strftime("%Y-%m-%dT%H:%M:%S.%f")`.
        let now = chrono::Local::now();
        let ts = now.format("%Y-%m-%dT%H:%M:%S%.6f");

        let meta = event.metadata();
        let level_padded = level_ljust_8(meta.level());
        let target = meta.target();

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        write!(writer, "{ts} [{level_padded}] {msg}", msg = visitor.message)?;

        for (k, v) in &visitor.fields {
            write!(writer, " {k}={v}")?;
        }

        writeln!(writer, " [{target}]")
    }
}

/// Map a [`tracing::Level`] to its Python `logging` level name,
/// left-justified to width 8 with trailing ASCII spaces.
///
/// The Python `logging` module uses `WARNING` (not `WARN`); `tracing`
/// uses `WARN`. The cross-SDK parity test expects the Python spelling.
fn level_ljust_8(level: &tracing::Level) -> &'static str {
    // Each literal is exactly 8 characters wide. `TRACE` is included
    // for completeness even though Python `cognee` never emits it.
    match *level {
        tracing::Level::TRACE => "TRACE   ",
        tracing::Level::DEBUG => "DEBUG   ",
        tracing::Level::INFO => "INFO    ",
        tracing::Level::WARN => "WARNING ",
        tracing::Level::ERROR => "ERROR   ",
    }
}

/// Field visitor that separates the `message` field from the rest,
/// matching Python structlog's `event_dict` flattening.
///
/// `tracing` dispatches `record_str` for `&str` fields and
/// `record_debug` for everything else (including `format_args!`
/// messages). We implement every `record_*` method so numeric and
/// boolean fields render as `k=42` / `k=true` (no `Debug` quoting),
/// and strings render unquoted.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl MessageVisitor {
    fn push(&mut self, name: &str, value: String) {
        if name == "message" {
            self.message.push_str(&value);
        } else {
            self.fields.push((name.to_string(), value));
        }
    }
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        // `record_debug` is the catch-all path; it's invoked for the
        // `message` field (which carries `format_args!`) and for any
        // typed field whose specialised `record_*` method we did not
        // implement. `format!("{value:?}")` quotes plain strings
        // (`"hello"`), which Python does not — strip a single matched
        // pair of surrounding ASCII double quotes so a `String`-typed
        // event field renders as `k=value`, not `k="value"`.
        let mut formatted = String::new();
        // Writing into a `String` via `write!` is infallible.
        let _ = write!(&mut formatted, "{value:?}");
        let stripped = strip_debug_quotes(&formatted);
        self.push(field.name(), stripped.to_string());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.push(field.name(), value.to_string());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.push(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.push(field.name(), value.to_string());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.push(field.name(), value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.push(field.name(), value.to_string());
    }
}

/// Strip exactly one pair of surrounding ASCII double quotes from a
/// `Debug`-formatted scalar so that `String` fields render the same as
/// `&str` fields (`k=value`, not `k="value"`). Multi-character escapes
/// inside the quoted string are left untouched — they would mismatch
/// Python's `str()` output anyway, but this is closer than the raw
/// `Debug` form.
fn strip_debug_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        // SAFETY: ASCII `"` is one byte; trimming one byte at each end
        // of a `&str` lands on UTF-8 char boundaries.
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::{debug, error, info, trace, warn};
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::prelude::*;

    /// `MakeWriter` that appends to a shared `Vec<u8>` so tests can
    /// inspect the bytes emitted by the formatter.
    #[derive(Clone, Default)]
    struct CaptureWriter {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl CaptureWriter {
        fn contents(&self) -> String {
            // lock poison is unrecoverable
            let guard = self.buf.lock().unwrap();
            String::from_utf8(guard.clone()).expect("formatter emits UTF-8")
        }
    }

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            // lock poison is unrecoverable
            let mut guard = self.buf.lock().unwrap();
            guard.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Install a `PythonPlainFormatter` subscriber backed by `writer`
    /// for the duration of `f`. Uses `with_default` so concurrent tests
    /// each get their own thread-local subscriber and don't trample
    /// global state.
    fn with_subscriber<F: FnOnce()>(writer: CaptureWriter, f: F) {
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .event_format(PythonPlainFormatter)
            .with_ansi(false);
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::TRACE)
            .with(layer);
        tracing::subscriber::with_default(subscriber, f);
    }

    #[test]
    fn formats_basic_info_event_with_python_shape() {
        let writer = CaptureWriter::default();
        with_subscriber(writer.clone(), || {
            info!(target: "cognee.test", "hello world");
        });
        let line = writer.contents();
        let re = regex::Regex::new(
            r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6} \[INFO    \] hello world \[cognee\.test\]\n$",
        )
        .expect("valid regex");
        assert!(re.is_match(&line), "unexpected output: {line:?}");
    }

    #[test]
    fn formats_event_with_keyed_fields() {
        let writer = CaptureWriter::default();
        with_subscriber(writer.clone(), || {
            info!(target: "cognee.test", dataset_id = "abc", count = 3, "started");
        });
        let line = writer.contents();
        let re = regex::Regex::new(
            r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6} \[INFO    \] started dataset_id=abc count=3 \[cognee\.test\]\n$",
        )
        .expect("valid regex");
        assert!(re.is_match(&line), "unexpected output: {line:?}");
    }

    #[test]
    fn pads_each_level_to_width_8() {
        // Each `[LEVEL]` bracket must be exactly 10 chars: `[` + 8
        // padded + `]`.
        for (emit, expected_name) in [
            (Level::Trace, "TRACE   "),
            (Level::Debug, "DEBUG   "),
            (Level::Info, "INFO    "),
            (Level::Warn, "WARNING "),
            (Level::Error, "ERROR   "),
        ] {
            assert_eq!(expected_name.len(), 8, "{expected_name:?}");
            let writer = CaptureWriter::default();
            with_subscriber(writer.clone(), || match emit {
                Level::Trace => trace!(target: "cognee.test", "x"),
                Level::Debug => debug!(target: "cognee.test", "x"),
                Level::Info => info!(target: "cognee.test", "x"),
                Level::Warn => warn!(target: "cognee.test", "x"),
                Level::Error => error!(target: "cognee.test", "x"),
            });
            let line = writer.contents();
            let expected_bracket = format!("[{expected_name}]");
            assert!(
                line.contains(&expected_bracket),
                "level {expected_name:?} missing from {line:?}"
            );
            // The bracketed level must be 10 chars (`[` + 8 + `]`).
            assert_eq!(expected_bracket.len(), 10);
        }
    }

    /// Mirror enum to drive the level-padding test without depending
    /// on the macros' generic-level entry point.
    #[derive(Clone, Copy)]
    enum Level {
        Trace,
        Debug,
        Info,
        Warn,
        Error,
    }

    #[test]
    fn includes_no_target_prefix_or_double_space() {
        let writer = CaptureWriter::default();
        with_subscriber(writer.clone(), || {
            info!(target: "cognee.test", "hi");
        });
        let line = writer.contents();
        // Python format puts level bracket directly before message with
        // exactly one space; no `target:` prefix, no double space, no
        // colon. Regress against the default `tracing-subscriber`
        // layout.
        assert!(line.contains("[INFO    ] hi "), "got: {line:?}");
        assert!(!line.contains("INFO    ]:"), "got: {line:?}");
        assert!(!line.contains("INFO    ]  "), "got: {line:?}");
    }

    #[test]
    fn string_field_renders_without_debug_quotes() {
        let writer = CaptureWriter::default();
        with_subscriber(writer.clone(), || {
            let owned: String = String::from("value");
            // `%owned` would call `Display`; the default specialisation
            // for `String` uses `record_debug`, which is the path we
            // need to exercise.
            info!(target: "cognee.test", key = ?owned, "msg");
        });
        let line = writer.contents();
        assert!(line.contains("key=value"), "got: {line:?}");
        assert!(!line.contains("key=\"value\""), "got: {line:?}");
    }

    #[test]
    fn boolean_field_renders_as_plain_token() {
        let writer = CaptureWriter::default();
        with_subscriber(writer.clone(), || {
            info!(target: "cognee.test", ready = true, "msg");
        });
        let line = writer.contents();
        assert!(line.contains("ready=true"), "got: {line:?}");
    }

    #[test]
    fn integer_field_has_no_type_suffix() {
        let writer = CaptureWriter::default();
        with_subscriber(writer.clone(), || {
            info!(target: "cognee.test", count = 42_i64, "msg");
        });
        let line = writer.contents();
        assert!(line.contains("count=42 "), "got: {line:?}");
        assert!(!line.contains("count=42i64"), "got: {line:?}");
    }

    #[test]
    fn level_ljust_8_table_widths() {
        for level in [
            tracing::Level::TRACE,
            tracing::Level::DEBUG,
            tracing::Level::INFO,
            tracing::Level::WARN,
            tracing::Level::ERROR,
        ] {
            assert_eq!(level_ljust_8(&level).len(), 8);
        }
        assert_eq!(level_ljust_8(&tracing::Level::WARN), "WARNING ");
        assert_eq!(level_ljust_8(&tracing::Level::INFO), "INFO    ");
    }
}
