# Task 06-05 — `init_logging`, `LogGuards`, and `default_filter`

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**:
- [Task 06-02 — Logging config](02-logging-config.md)
- [Task 06-03 — Path helpers](03-paths-and-cleanup.md)
- [Task 06-04 — Python plain formatter](04-python-plain-formatter.md)

**Blocks**:
- [Task 06-06 — CLI refactor](06-cli-refactor.md)
- [Task 06-07 — HTTP server refactor](07-http-server-refactor.md)
- [Task 06-08 — Binding entrypoints](08-binding-entrypoints.md)

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 1 (time-based rotation), 3 (configurable format), 5 (`LOG_FILE_NAME`), 6 (default filter), 7 (`LOG_LEVEL` fallback), 11 (startup-only cleanup), 13 (`SpanBufferLayer` via `extra_layers`).

---

## 1. Goal

Compose all gap-06 machinery into one public function:

```rust
pub fn init_logging<I, L>(
    cfg: LoggingConfig,
    extra_layers: I,
) -> LogGuards
where
    I: IntoIterator<Item = L>,
    L: tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync + 'static;
```

Behaviour:

1. Build an `EnvFilter` from `cfg.level_filter` (falling back to
   [`default_filter()`](#default_filter)).
2. Build a stdout layer using the format selected by `cfg.format`.
3. If `cfg.file_enabled`:
   - `resolve_logs_dir(&cfg)` → `Option<PathBuf>`.
   - If resolved: `propagate_log_file_name(dir)` → file path; build a
     `RollingFileAppender` with `cfg.rotation` and the resolved file
     path's stem as the filename prefix; wrap with `non_blocking`;
     attach the same formatter as stdout. Also run
     `cleanup_old_logs(dir, cfg.max_files)` (decision 11).
   - If unresolved: emit a single `tracing::warn!` ("file logging
     disabled — no writable directory") and skip the file layer.
4. Compose: `Registry::default().with(env_filter).with(stdout_layer)
   .with(file_layer).with(extra_layers...)` and call
   `try_init()`. Failure is soft (logged via `eprintln!` so a test
   that already installed a subscriber doesn't panic).
5. Emit a one-shot `info!` line ("Logging initialized: file=<path>
   rotation=<...> format=<...>") so the cross-SDK file-presence
   assertion in 06-10 has a known anchor to grep.
6. Return `LogGuards { _file_guard: Option<WorkerGuard> }` for the
   caller to keep alive.

Also export `default_filter() -> &'static str` returning the
hardcoded decision-6 string.

## 2. Rationale

- A single `init_logging` is the only seam the rest of the codebase
  consumes. Tests, binaries, and bindings all call it the same way.
- Taking `extra_layers` as a generic iterator lets the HTTP server
  add `SpanBufferLayer` (decision 13) and lets `cli` add its
  optional OTEL telemetry layer without `cognee-logging` knowing
  about either.
- The `LogGuards` return is the well-known
  `tracing-appender::non_blocking` pattern. Forgetting to hold it
  drops in-flight events at process exit.

## 3. Pre-conditions

- Tasks 06-02, 06-03, 06-04 all committed.
- `crates/logging/src/init.rs` does not exist yet.

## 4. Step-by-step

### 4.1 Create `crates/logging/src/init.rs`

Public API:

```rust
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer, Registry,
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::{LogFormat, LoggingConfig};

/// Owns the non-blocking writer's worker thread. Must outlive the
/// program; dropping flushes any in-flight log lines.
#[must_use = "LogGuards must be held until process exit; dropping flushes pending log lines"]
pub struct LogGuards {
    _file_guard: Option<WorkerGuard>,
}

impl LogGuards {
    /// Construct a no-op guard for code paths that skipped file logging.
    pub fn noop() -> Self { Self { _file_guard: None } }
}

/// Library-noise-suppressing default filter (decision 6).
pub fn default_filter() -> &'static str {
    "info,\
     ort=warn,\
     reqwest=warn,\
     hyper=warn,\
     h2=warn,\
     rustls=warn,\
     sqlx=warn,\
     sea_orm=warn,\
     sea_orm_migration=warn,\
     tower_http=warn,\
     qdrant_segment=warn,\
     qdrant_shard=warn"
}

pub fn init_logging<I, L>(cfg: LoggingConfig, extra_layers: I) -> LogGuards
where
    I: IntoIterator<Item = L>,
    L: Layer<Registry> + Send + Sync + 'static,
{
    // (1) EnvFilter.
    let filter_directive = cfg.level_filter.clone()
        .unwrap_or_else(|| default_filter().to_string());
    let env_filter = EnvFilter::try_new(&filter_directive)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // (2) Stdout layer per cfg.format.
    let stdout_layer: Box<dyn Layer<Registry> + Send + Sync> = match cfg.format {
        LogFormat::Plain => Box::new(
            tracing_subscriber::fmt::layer()
                .event_format(crate::PythonPlainFormatter)
                .with_writer(std::io::stdout),
        ),
        LogFormat::Json => Box::new(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::io::stdout),
        ),
    };

    // (3) Optional file layer.
    let (file_layer, file_guard, resolved_file): (
        Option<Box<dyn Layer<Registry> + Send + Sync>>,
        Option<WorkerGuard>,
        Option<std::path::PathBuf>,
    ) = if cfg.file_enabled {
        match crate::resolve_logs_dir(&cfg) {
            Some(dir) => {
                let file_path = crate::propagate_log_file_name(&dir);
                let (layer, guard) = build_file_layer(&dir, &file_path, &cfg);
                (Some(layer), Some(guard), Some(file_path))
            }
            None => (None, None, None),
        }
    } else {
        (None, None, None)
    };

    // (4) Compose + try_init.
    let registry = Registry::default()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer);

    let registry = extra_layers.into_iter().fold(
        // BoxedSubscriber-style: layer composition requires a final
        // Layered<...> type. Implementor: use `.with(layer)` in a loop
        // by collecting extras into Vec<Box<dyn Layer<...>>> and adding
        // them one at a time. The pattern is well-known; see
        // crates/observability/src/init.rs for prior art.
        registry,
        |reg, layer| reg.with(Some(layer)),
    );

    if registry.try_init().is_err() {
        eprintln!("cognee-logging: a tracing subscriber is already installed; init_logging is a no-op");
    }

    // (5) Startup cleanup pass and announcement.
    if let Some(path) = resolved_file.as_ref() {
        if let Some(dir) = path.parent() {
            crate::cleanup_old_logs(dir, cfg.max_files);
        }
        tracing::info!(
            file = %path.display(),
            rotation = ?cfg.rotation,
            format = ?cfg.format,
            "Logging initialized"
        );
    } else if cfg.file_enabled {
        tracing::warn!("file logging requested but no writable directory was found");
    } else {
        tracing::info!(format = ?cfg.format, "Logging initialized (stdout only)");
    }

    if std::env::var_os("COGNEE_LOG_MAX_BYTES").is_some() {
        tracing::warn!(
            "COGNEE_LOG_MAX_BYTES is accepted for parity but ignored: \
             size-based rotation is not yet supported (see decision 1)"
        );
    }

    LogGuards { _file_guard: file_guard }
}

fn build_file_layer(
    dir: &Path,
    file_path: &Path,
    cfg: &LoggingConfig,
) -> (Box<dyn Layer<Registry> + Send + Sync>, WorkerGuard) {
    use tracing_appender::rolling::{RollingFileAppender, Rotation};

    let rotation = match cfg.rotation {
        crate::LogRotation::Daily    => Rotation::DAILY,
        crate::LogRotation::Hourly   => Rotation::HOURLY,
        crate::LogRotation::Minutely => Rotation::MINUTELY,
        crate::LogRotation::Never    => Rotation::NEVER,
    };

    // Filename prefix = file_path's file_stem (without extension).
    // tracing-appender writes <prefix>.<YYYY-MM-DD> on DAILY.
    let prefix = file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cognee")
        .to_string();

    let appender = RollingFileAppender::builder()
        .rotation(rotation)
        .filename_prefix(prefix)
        .filename_suffix("log")
        .max_log_files(cfg.backup_count)
        .build(dir)
        .expect("RollingFileAppender::build succeeds for a directory we already mkdir-p'd and write-probed");
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    let layer: Box<dyn Layer<Registry> + Send + Sync> = match cfg.format {
        LogFormat::Plain => Box::new(
            tracing_subscriber::fmt::layer()
                .event_format(crate::PythonPlainFormatter)
                .with_writer(non_blocking)
                .with_ansi(false),
        ),
        LogFormat::Json => Box::new(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(non_blocking)
                .with_ansi(false),
        ),
    };
    (layer, guard)
}
```

### 4.2 Implementation notes

- **Type-erasure** — `tracing_subscriber::Layer` composition is
  notoriously fiddly with mixed concrete types. Wrap each concrete
  layer in `Box<dyn Layer<Registry> + Send + Sync>` and consume the
  builder one-at-a-time. See
  [`crates/observability/src/init.rs`](../../../crates/observability/src/init.rs)
  (gap 04 lives here) for the `BoxedTelemetryLayer<Registry>` type
  alias used to thread `init_telemetry` results through nested
  subscribers. Mirror that style — define a public type alias
  `pub type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;`
  if it doesn't already exist somewhere we can import.
- **`extra_layers` fold pattern** — `.with(layer)` returns a fresh
  `Layered<L, S>`, which is not the same type as the input.
  Boxing every layer (including extras when they arrive) means the
  fold's accumulator type is stable across iterations. The caller's
  `extra_layers: I` should be constrained to `Item = BoxedLayer` —
  re-evaluate the function signature once `BoxedLayer` is in scope.
  Adjust the public signature to take
  `extra_layers: impl IntoIterator<Item = BoxedLayer>` for ergonomic
  use from binaries.
- **No `unwrap` outside tests** — the `.expect(...)` on
  `RollingFileAppender::build` is justified: at that call site the
  directory has been `mkdir -p`'d and write-probed by
  `resolve_logs_dir`. Document this in the `.expect` message.
- **`try_init` failure** — `eprintln!` (not `tracing::warn!` — the
  subscriber isn't installed). This branch only fires in tests when
  a prior test already installed one; production binaries call
  `init_logging` exactly once and succeed.

### 4.3 Tests in `init.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::tempdir;

    #[test]
    fn default_filter_includes_baseline_crates() {
        let f = default_filter();
        assert!(f.contains("ort=warn"));
        assert!(f.contains("reqwest=warn"));
        assert!(f.contains("hyper=warn"));
        assert!(f.contains("sqlx=warn"));
        assert!(f.contains("sea_orm=warn"));
        assert!(f.contains("qdrant_segment=warn"));
    }

    #[test]
    #[serial]
    fn init_logging_writes_file_when_enabled() {
        let dir = tempdir().unwrap();
        std::env::set_var("COGNEE_LOGS_DIR", dir.path());
        std::env::remove_var("LOG_FILE_NAME");
        let cfg = LoggingConfig::from_env().unwrap();
        let _guards = init_logging(cfg, std::iter::empty::<BoxedLayer>());

        tracing::info!("hello from init_logging test");
        drop(_guards); // flush
        std::env::remove_var("COGNEE_LOGS_DIR");

        // At least one *.log under dir contains "hello from init_logging test".
        let found = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension() == Some("log".as_ref()))
            .any(|e| std::fs::read_to_string(e.path()).unwrap()
                       .contains("hello from init_logging test"));
        assert!(found);
    }

    #[test]
    #[serial]
    fn init_logging_skips_file_when_disabled() {
        let dir = tempdir().unwrap();
        std::env::set_var("COGNEE_LOGS_DIR", dir.path());
        std::env::set_var("COGNEE_LOG_FILE", "false");
        let cfg = LoggingConfig::from_env().unwrap();
        let _guards = init_logging(cfg, std::iter::empty::<BoxedLayer>());
        std::env::remove_var("COGNEE_LOGS_DIR");
        std::env::remove_var("COGNEE_LOG_FILE");

        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    #[serial]
    fn init_logging_warns_about_max_bytes() {
        // COGNEE_LOG_MAX_BYTES set → warn-once.
        // Capturing the warn requires installing the subscriber first,
        // which init_logging itself does. Implementor: use a temp
        // file as the log sink and grep for "size-based rotation".
    }
}
```

### 4.4 Wire the module into `lib.rs`

Replace the comment:

```rust
// mod init;         // 06-05: init_logging + LogGuards + default_filter
```

with:

```rust
mod init;
pub use init::{BoxedLayer, LogGuards, default_filter, init_logging};
```

(`BoxedLayer` is the public type alias defined inside `init.rs`.)

## 5. Verification

```bash
# 1. Crate compiles.
cargo check -p cognee-logging --all-targets

# 2. All cognee-logging tests pass.
cargo test -p cognee-logging

# 3. Workspace still compiles.
cargo check --all-targets

# 4. Clippy.
cargo clippy -p cognee-logging --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- `crates/logging/src/init.rs` — NEW.
- `crates/logging/src/lib.rs` — wire the module + re-exports.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `Layer<Registry>` boxing pattern doesn't compose with the OTEL boxed layer from `crates/observability` | Medium | Adopt the same `BoxedTelemetryLayer<Registry>` alias used by gap 04. Keep `BoxedLayer` shape-equal so callers can pass either. |
| Calling `try_init` twice in test runs poisons the global subscriber | Medium | `#[serial_test::serial]` + the soft-fail `eprintln!` branch. Tests that need their own subscriber install one *before* calling `init_logging` and expect the eprintln. |
| `RollingFileAppender::build` fails despite our prior write probe (e.g. race with another process unmounting the directory) | Very low | The `.expect` documents the contract; if it ever fires it indicates a real environmental problem worth crashing on. Alternative: bubble through `Result<LogGuards, _>` — but every caller would `.expect` it anyway. |
| `non_blocking`'s worker thread leaks if `WorkerGuard` is dropped while writes are pending | By design — guard's `Drop` flushes | `#[must_use]` on `LogGuards` plus a one-line doc reminder makes the contract explicit. |

## 8. Out of scope

- Per-call structured tags. The `info!("Logging initialized", ...)`
  line is for the cross-SDK assertion only; no other crate should
  treat it as an event hook.
- Hot-reloading config at runtime. `LoggingConfig` is read once at
  `init_logging` time. Changing `COGNEE_LOG_LEVEL` mid-process
  requires a process restart (matches Python).
- Per-target sinks (e.g. errors to one file, info to another).
  Not needed for parity; Python uses one file for everything.
