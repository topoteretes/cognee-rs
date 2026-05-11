# Task 06-04 — Python-byte-exact `PythonPlainFormatter`

**Status**: implemented in commit 0dea3ce (note: `regex.workspace = true` added as a dev-dependency to `crates/logging/Cargo.toml` for parity-regex tests)
**Owner**: _unassigned_
**Depends on**: [Task 06-02 — Logging config](02-logging-config.md).
**Blocks**:
- [Task 06-05 — init_logging](05-init-logging.md) (uses the formatter when `LogFormat::Plain`).
- [Task 06-10 — Tests](10-tests.md) (cross-SDK parity test compares formatter output to Python).

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 4 (byte-exact Python format), 12 (per-message parity test).

---

## 1. Goal

Implement `PythonPlainFormatter` in a new module
`crates/logging/src/formatter.rs`. The formatter is a
`tracing_subscriber::fmt::FormatEvent` impl that produces lines
**byte-equal** to Python's
[`PlainFileHandler.emit`](https://github.com/topoteretes/cognee/blob/main/cognee/shared/logging_utils.py#L150)
output:

```
<ts %Y-%m-%dT%H:%M:%S.%f> [<LEVEL ljust(8)>] <message>[ k1=v1 k2=v2 ...] [<logger_name>]
```

Concrete example:

```
2026-05-11T11:42:13.872451 [INFO    ] Pipeline started dataset_id=abc-123 user_id=u1 [cognee.cognify.pipeline]
```

## 2. Rationale

- Decision 4 requires byte-exact parity so the cross-SDK loose
  filename + strict message test (decision 12) can grep the same
  human-readable lines from Python and Rust logs.
- `tracing-subscriber`'s default formatter cannot produce this
  layout — its output is `<ts>  <LEVEL> <target>: <msg> <fields>`
  with two spaces, no brackets, and a `target:` prefix. We need a
  full `FormatEvent` impl.
- A small `FieldVisitor` collects each event's key/value pairs as
  `k=v` strings, matching Python's structlog `event_dict` flattening
  (line 192 of `logging_utils.py`).

## 3. Pre-conditions

- Task 06-02 committed.
- `crates/logging/src/formatter.rs` does not exist yet.
- `chrono` is in `[workspace.dependencies]` (added in 06-02 if it
  wasn't already there).

## 4. Step-by-step

### 4.1 Format spec — exact reference

From `logging_utils.py:189` (the line we must reproduce):

```python
log_entry = (
    f"{timestamp} [{record.levelname.ljust(8)}] "
    f"{message}{context_str} [{logger_name}]\n"
)
```

Where:

- `timestamp` is `datetime.now().strftime(get_timestamp_format())`.
  `get_timestamp_format()` returns `"%Y-%m-%dT%H:%M:%S.%f"` when
  microseconds are supported, else `"%Y-%m-%dT%H:%M:%S"` (lines
  612–629 of `logging_utils.py`). Rust must use the microsecond form;
  it always works (no platform-dependent fallback needed).
- `record.levelname.ljust(8)` pads the level name to width 8 with
  trailing spaces. The five values used by Python's `logging` are
  `"CRITICAL"`, `"ERROR   "`, `"WARNING "`, `"INFO    "`, `"DEBUG   "`,
  `"NOTSET  "`, `"TRACE   "` (last one is structlog-only; not
  emitted by Python `cognee` but included for completeness).
- `context_str` is built from any structlog `event_dict` keys not in
  `{"event", "logger", "level", "timestamp", "exc_info"}`, joined as
  ` k=v` (leading space, space-separated).
- `logger_name` is `record.msg.get("logger", record.name)` —
  effectively the tracing `target` in Rust.
- A single `\n` terminator (the `FormatEvent` `writer.write_str`
  contract requires us to emit the newline ourselves).

### 4.2 Implementation sketch

```rust
use std::fmt::Write as _;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::{format::Writer, FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// Python-byte-exact plain text formatter.
///
/// Output shape:
/// `<ts %Y-%m-%dT%H:%M:%S.%6f> [<LEVEL ljust(8)>] <msg>[ k=v ...] [<target>]\n`
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
        let now = chrono::Local::now();
        let ts = now.format("%Y-%m-%dT%H:%M:%S%.6f");

        let meta = event.metadata();
        let level_padded = level_ljust_8(meta.level());
        let target = meta.target();

        // Visit fields: separate "message" from the rest.
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        write!(writer, "{ts} [{level_padded}] {msg}",
            msg = visitor.message)?;

        for (k, v) in &visitor.fields {
            write!(writer, " {k}={v}")?;
        }

        writeln!(writer, " [{target}]")
    }
}

fn level_ljust_8(level: &tracing::Level) -> &'static str {
    // tracing::Level → Python `logging` name, padded to 8 spaces.
    match *level {
        tracing::Level::TRACE => "TRACE   ",
        tracing::Level::DEBUG => "DEBUG   ",
        tracing::Level::INFO  => "INFO    ",
        tracing::Level::WARN  => "WARNING ",
        tracing::Level::ERROR => "ERROR   ",
    }
}
```

`MessageVisitor`:

```rust
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if name == "message" {
            // Python's `event` key maps to tracing's `message`. Strip
            // the surrounding quotes that `record_debug` would produce
            // for strings by formatting through `{:?}` only when the
            // value isn't a primitive — easier: capture via record_str
            // first, fall back to record_debug here.
            use std::fmt::Write as _;
            let _ = write!(&mut self.message, "{value:?}");
        } else {
            self.fields.push((name.to_string(), format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let name = field.name();
        if name == "message" {
            self.message.push_str(value);
        } else {
            self.fields.push((name.to_string(), value.to_string()));
        }
    }
    // ... record_i64 / record_u64 / record_bool / record_f64 mirror
    // record_str using format!("{value}") so output is "k=42" not "k=42i64".
}
```

Implementation notes for the implementor:

- **`record_str` vs `record_debug`** — `tracing` calls `record_str`
  for `&str` fields and `record_debug` for everything else
  (including `String` and `format_args!` messages). The visitor
  must implement all six `record_*` methods (`str`, `i64`, `u64`,
  `f64`, `bool`, `debug`) for clean output.
- **Field ordering** — Python flattens an unordered dict; in
  practice the cross-SDK test should not depend on field order
  (the parity test should compare on a *normalised* form that sorts
  fields or matches the message before the first `[`). Document
  this in the formatter doc-comment.
- **Newline** — `writeln!` emits `\n` (Unix). Matches Python's
  `self.terminator` which defaults to `"\n"`. Do not use `\r\n` on
  Windows; Python's logger doesn't either.
- **Timezone** — Python uses naive local time
  (`datetime.now().strftime(...)`). Mirror with `chrono::Local`, not
  `chrono::Utc`. The microsecond precision flag in chrono is
  `%.6f`, not `%f`.

### 4.3 Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tracing::{info, Level};
    use tracing_subscriber::fmt::{format::DefaultFields, MakeWriter};
    use tracing_subscriber::Registry;

    // A `MakeWriter` that writes into a shared Vec<u8>.
    // (Implementor: copy from existing test helpers in
    // `crates/observability/` or `crates/cognify/` if a similar
    // capture helper already exists.)
    struct CaptureWriter(/* Arc<Mutex<Vec<u8>>> */);

    #[test]
    fn formats_basic_info_event_with_python_shape() {
        // Set up a subscriber with PythonPlainFormatter + capture.
        // Emit `info!(target: "cognee.test", "hello world");`.
        // Assert captured line matches:
        //   r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6} \[INFO    \] hello world \[cognee\.test\]\n$"
    }

    #[test]
    fn formats_event_with_keyed_fields() {
        // info!(target: "cognee.test", dataset_id = "abc", count = 3, "started");
        // Assert: r"... \[INFO    \] started dataset_id=abc count=3 \[cognee\.test\]\n$"
    }

    #[test]
    fn pads_each_level_to_width_8() {
        // Emit one event per level, assert the bracket content is
        // exactly 10 chars (1 + 8 padded + 1).
    }

    #[test]
    fn includes_no_target_prefix_or_double_space() {
        // Regression: assert "INFO    ] " not "INFO    ]: " or "INFO    ]  ".
    }
}
```

For the capture-writer pattern, see if `crates/observability/`
already exports a `TestCapture` helper from gap 04. If yes, depend on
it under `[dev-dependencies]`. If no, write a small `Arc<Mutex<Vec<u8>>>`-
based `MakeWriter` directly in the test module — about 20 lines.

### 4.4 Wire the module into `lib.rs`

Replace the comment in `crates/logging/src/lib.rs`:

```rust
// mod formatter;    // 06-04: PythonPlainFormatter
```

with:

```rust
mod formatter;
pub use formatter::PythonPlainFormatter;
```

## 5. Verification

```bash
# 1. Crate compiles.
cargo check -p cognee-logging --all-targets

# 2. Formatter tests pass.
cargo test -p cognee-logging formatter

# 3. Workspace still compiles.
cargo check --all-targets

# 4. Clippy.
cargo clippy -p cognee-logging --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh

# 6. Cross-check against Python output (manual).
#    Run a tiny Python snippet that emits one line through cognee's
#    setup_logging() and visually diff against the Rust formatter's
#    captured output. The cross-SDK test in 06-10 automates this.
```

## 6. Files modified

- `crates/logging/src/formatter.rs` — NEW.
- `crates/logging/src/lib.rs` — wire the module + re-export.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `tracing` field ordering differs from Python structlog dict ordering | Medium | The cross-SDK parity test normalises field order. Document the limitation in the formatter doc-comment so future readers don't expect byte-equal lines with multiple fields. |
| Microsecond precision drift on platforms without high-res clocks | Low (Linux/macOS/Android all support µs) | `chrono` always emits 6 digits; if the platform's clock resolution is coarser, trailing digits are zero — still byte-comparable. |
| Python emits WARN as `"WARNING "`, Rust `tracing` uses `WARN` | Mitigated by `level_ljust_8` table | Hardcoded mapping. Trace level present even though Python doesn't emit it — harmless. |
| `record_debug` adds extra quoting around `String` values (e.g. `key="value"` instead of `key=value`) | High if not handled | Implement all six `record_*` methods explicitly; `record_str` handles `&str` without quoting. |
| Performance — string formatting per event | Low (file I/O dominates) | `WorkerGuard` from 06-05 ensures writes are off the hot path. |

## 8. Out of scope

- A "fallback timestamp without microseconds" branch. Python had
  this for legacy Python 2 environments — Rust doesn't need it.
- ANSI colors on stdout. Decision 3 ties stdout format to the same
  `LogFormat` enum; if a future task wants colors for the console
  layer, it adds a separate `ColoredPlainFormatter` (out of scope
  here).
- Exception traceback handling (Python's `exc_info` branch). Rust
  `tracing` events don't carry stack traces; if needed in the
  future, attach via `tracing-error`'s `SpanTrace`. Not part of
  gap 06.
- Logger-name remapping. Python's structlog allows `logger=` field
  override; tracing uses `target` which is the module path. The
  bracketed value is whatever `event.metadata().target()` returns.
