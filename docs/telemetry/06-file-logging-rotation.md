# File-Based Logging with Rotation — Gap Analysis

## Overview

Python `cognee` ships with a **structured, file-based logger** that writes every log record to a rotating plain-text file under `~/.cognee/logs/`, in addition to colored stderr output. Rotation is **size-based** (50 MB per file × 5 backups by default, with a hard ceiling of 10 files in the directory). Many env vars tune the location, size, format, and noise level. The Rust port currently emits to **stdout only** through `tracing-subscriber::fmt`, with no file sink, no rotation, and no on-disk log retention.

This document catalogs the Python implementation, surveys the Rust ecosystem, and proposes a concrete design for closing the gap. It deliberately avoids touching [`gap-analysis.md`](./gap-analysis.md) — this is a focused sub-investigation into one specific telemetry pillar.

---

## Python Implementation

### File catalog

| File | Lines | Role |
|---|---|---|
| `cognee/shared/logging_utils.py` | 1–630 | All logging setup: structlog config, file handler, rotation, cleanup, env-var resolution |
| `cognee/base_config.py` | 11–41 | `BaseConfig.logs_root_directory` resolver — reads `COGNEE_LOGS_DIR`, defaults to `~/.cognee/logs` |

### Key code excerpts

#### `PlainFileHandler` — the rotating handler (`logging_utils.py:150–232`)

```python
class PlainFileHandler(logging.handlers.RotatingFileHandler):
    """A rotating file handler that writes simpler plain text log entries.

    Inherits from RotatingFileHandler so log files are automatically rotated
    when they reach maxBytes, keeping at most backupCount old files.
    """

    def emit(self, record) -> None:
        try:
            if self.stream is None:
                self.stream = self._open()

            if isinstance(record.msg, dict) and "event" in record.msg:
                message = record.msg.get("event", "")
                context = {k: v for k, v in record.msg.items()
                           if k not in ("event", "logger", "level", "timestamp")}
                context_str = ""
                if context:
                    context_str = " " + " ".join(
                        f"{k}={v}" for k, v in context.items() if k != "exc_info"
                    )
                logger_name = record.msg.get("logger", record.name)
                timestamp = datetime.now().strftime(get_timestamp_format())

                log_entry = (
                    f"{timestamp} [{record.levelname.ljust(8)}] "
                    f"{message}{context_str} [{logger_name}]\n"
                )
                self.stream.write(log_entry)
                self.flush()
                # ... exception traceback handling ...
            else:
                msg = self.format(record)
                self.stream.write(msg + self.terminator)
                self.flush()
        except Exception as e:
            self.handleError(record)
```

This is a subclass of stdlib `logging.handlers.RotatingFileHandler`, so rotation is fully automatic on `emit()` once `maxBytes` is exceeded.

#### Handler instantiation in `setup_logging()` (`logging_utils.py:501–536`)

```python
log_file_enabled = os.getenv("COGNEE_LOG_FILE", "true").lower() not in ("false", "0", "no")
log_file_path = None

if log_file_enabled:
    logs_dir = resolve_logs_dir()

    # Reuse path across child processes via env var
    log_file_path = os.environ.get("LOG_FILE_NAME")
    if not log_file_path and logs_dir is not None:
        start_time = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
        log_file_path = str((logs_dir / f"{start_time}.log").resolve())
        os.environ["LOG_FILE_NAME"] = log_file_path

    try:
        file_handler = PlainFileHandler(
            log_file_path,
            maxBytes=LOG_MAX_BYTES,        # default 50 MB
            backupCount=LOG_BACKUP_COUNT,  # default 5
            encoding="utf-8",
        )
        file_handler.setLevel(log_level)
        root_logger.addHandler(file_handler)
    except Exception as e:
        root_logger.warning(f"Could not create log file handler at {log_file_path}: {e}")
```

#### Rotation defaults (`logging_utils.py:135–140`)

```python
MAX_LOG_FILES = 10
LOG_MAX_BYTES = int(os.getenv("COGNEE_LOG_MAX_BYTES", 50 * 1024 * 1024))  # 50 MB
LOG_BACKUP_COUNT = int(os.getenv("COGNEE_LOG_BACKUP_COUNT", 5))
```

#### Logs dir resolution (`logging_utils.py:103–132`)

```python
def resolve_logs_dir() -> Path | None:
    base_config = get_base_config()
    logs_root_directory = Path(base_config.logs_root_directory)
    try:
        logs_root_directory.mkdir(parents=True, exist_ok=True)
        if os.access(logs_root_directory, os.W_OK):
            return logs_root_directory
    except Exception:
        pass

    try:
        tmp_log_path = Path(os.path.join("/tmp", "cognee_logs"))
        tmp_log_path.mkdir(parents=True, exist_ok=True)
        if os.access(tmp_log_path, os.W_OK):
            return tmp_log_path
    except Exception:
        pass
    return None
```

#### Cleanup of old files (`logging_utils.py:271–308`)

```python
def cleanup_old_logs(logs_dir, max_files) -> bool:
    log_files = [f for f in logs_dir.glob("*.log") if f.is_file()]
    log_files.sort(key=lambda x: x.stat().st_mtime, reverse=True)

    if len(log_files) > max_files:
        for old_file in log_files[max_files:]:
            try:
                old_file.unlink()
            except Exception as e:
                logger.error(f"Failed to delete old log file {old_file}: {e}")
    return True
```

This runs once at the end of `setup_logging()` (`logging_utils.py:551–552`), not continuously — it is a startup-time prune.

#### `BaseConfig.logs_root_directory` (`base_config.py:15`)

```python
logs_root_directory: str = os.getenv("COGNEE_LOGS_DIR", str(Path.home() / ".cognee" / "logs"))
```

---

## Env Var Catalog

| Variable | Default | Purpose |
|---|---|---|
| `COGNEE_LOG_FILE` | `true` | Master toggle. Set to `false`/`0`/`no` to disable file logging entirely. |
| `COGNEE_LOGS_DIR` | `~/.cognee/logs` | Primary logs directory. Falls back to `/tmp/cognee_logs` if unwritable. |
| `LOG_FILE_NAME` | (unset → generated) | Full path of the active log file. Set by parent process and re-read by children so all processes share one file. Pattern: `<COGNEE_LOGS_DIR>/<YYYY-MM-DD_HH-MM-SS>.log`. |
| `COGNEE_LOG_MAX_BYTES` | `52428800` (50 MB) | Per-file size cap that triggers rotation. |
| `COGNEE_LOG_BACKUP_COUNT` | `5` | Number of rotated backups to keep (`<file>.1`, `<file>.2`, …). |
| `MAX_LOG_FILES` | `10` (constant in module, not env-driven) | Hard ceiling enforced by `cleanup_old_logs()` across all `*.log` files in the dir. |
| `LOG_LEVEL` | `INFO` | Root log level. Accepts `CRITICAL`/`ERROR`/`WARNING`/`INFO`/`DEBUG`/`NOTSET`. |
| `COGNEE_CLI_MODE` | (unset) | When `true`, switches log messages to a more compact CLI-friendly form (cleanup prints a summary, not per-file deletions). |
| `LITELLM_LOG` | `ERROR` (default-set) | Suppresses LiteLLM's verbose output — set early via `os.environ.setdefault`. |
| `LITELLM_SET_VERBOSE` | `False` (default-set) | Turns off LiteLLM's verbose printing. |

`MAX_LOG_FILES` is hard-coded at 10 in Python (it is *named* like an env-var constant but is not env-driven). The Rust port should make it env-driven for parity flexibility.

---

## Log Format

Plain-text format string (built in `PlainFileHandler.emit`, line 189):

```
<timestamp> [<LEVEL_8-padded>] <message>[ key1=val1 key2=val2 ...] [<logger_name>]
```

Concrete example:

```
2026-05-06T11:42:13.872451 [INFO    ] Pipeline started dataset_id=abc-123 user_id=u1 [cognee.cognify.pipeline]
```

- Timestamp format: `%Y-%m-%dT%H:%M:%S.%f` if microseconds work, else `%Y-%m-%dT%H:%M:%S` (selected by `get_timestamp_format()`, lines 612–629).
- Level is `ljust(8)` so columns align.
- All structlog `event_dict` extras are appended as `key=value` pairs (excluding internal keys `event`/`logger`/`level`/`timestamp`/`exc_info`).
- Logger name is bracketed last.
- Exceptions: full `traceback.format_exception(...)` is appended on the next line.

The console handler uses `structlog.dev.ConsoleRenderer` with ANSI colors and is **separate** from the file format — file logs are always plain text, regardless of console settings.

---

## Log Dir Resolution Priority

From `resolve_logs_dir()`:

1. `COGNEE_LOGS_DIR` env var → `BaseConfig.logs_root_directory`. Try to `mkdir -p` and check writable.
2. Fallback: `/tmp/cognee_logs`. Try to `mkdir -p` and check writable.
3. If neither succeeds → `None`. File logging is silently skipped (with a console warning).

Note: `BaseConfig.logs_root_directory` is resolved once at module load via `os.getenv("COGNEE_LOGS_DIR", str(Path.home() / ".cognee" / "logs"))` and run through `ensure_absolute_path()` in `validate_paths()`.

---

## Multi-Process Coordination

A subtle but critical mechanism (`logging_utils.py:511–519`):

```python
log_file_path = os.environ.get("LOG_FILE_NAME")
if not log_file_path and logs_dir is not None:
    start_time = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
    log_file_path = str((logs_dir / f"{start_time}.log").resolve())
    os.environ["LOG_FILE_NAME"] = log_file_path
```

When the **parent process** calls `setup_logging()`, it generates a timestamped filename like `2026-05-06_11-42-13.log` and **writes it back to its own env**. Any child process spawned afterward (via `multiprocessing`, `subprocess` with inherited env, etc.) reads the same `LOG_FILE_NAME` and appends to the same file rather than creating a fresh per-PID log.

There is **no file-locking** — Python's `RotatingFileHandler` is not multi-process safe. Concurrent rotation from multiple processes can corrupt files. The author appears to accept this risk in exchange for unified-log behavior.

---

## Rust Current State

### CLI binary — [`crates/cli/src/main.rs:50–58`](../../crates/cli/src/main.rs#L50-L58)

```rust
let env_filter =
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
let _ = tracing_subscriber::fmt()
    .with_env_filter(env_filter)
    .with_target(false)
    .try_init();
```

### HTTP server binary — [`crates/http-server/src/main.rs:100–118`](../../crates/http-server/src/main.rs#L100-L118)

```rust
fn init_tracing(spans: Arc<SpanBuffer>) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let fmt_layer = fmt::layer().with_target(false);
    let buffer_layer = SpanBufferLayer::new((*spans).clone());

    let _ = Registry::default()
        .with(filter)
        .with(fmt_layer)
        .with(buffer_layer)
        .try_init();
}
```

### Workspace `tracing-subscriber` features — [`Cargo.toml:100`](../../Cargo.toml#L100)

```toml
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
```

Key observations:

- Subscribers are initialized **only in binaries** (CLI + HTTP server). Library crates do not auto-init — this is correct.
- Output goes to **stdout/stderr** only. No file sink, no rotation, no retention.
- The `json` feature of `tracing-subscriber` is **not** enabled.
- `tracing-appender` is **not** a workspace dependency.
- Filter uses only `RUST_LOG` (parsed by `EnvFilter::try_from_default_env`); `LOG_LEVEL` (the Python env var) is ignored.
- Library noise is mostly handled in-binary by hard-coding `"info,ort=warn"`. Other noisy crates (`reqwest`, `hyper`, `h2`, `sea_orm`, `qdrant_*`) leak into output at info level.

---

## Detailed Gap Analysis

| Feature | Python | Rust | Gap |
|---|---|---|---|
| File sink | `PlainFileHandler` (rotating) | None | Missing entirely |
| Size-based rotation | 50 MB × 5 backups | None | Missing |
| Time-based rotation | No (only size) | N/A | n/a |
| Backup count | `COGNEE_LOG_BACKUP_COUNT` | None | Missing |
| Log dir resolution with fallback | `~/.cognee/logs` → `/tmp/cognee_logs` | None | Missing |
| Multi-process shared file via `LOG_FILE_NAME` | Yes | None | Missing |
| Cleanup of stale `*.log` files (>10) | `cleanup_old_logs()` | None | Missing |
| Plain-text format string | `<ts> [<LEVEL>] <msg> k=v ... [logger]` | tracing-fmt default | Different format; no on-disk file |
| JSON structured output | No (only console+plain file) | No | Both lack JSON; opportunity for an upgrade in Rust |
| `LOG_LEVEL` env var | Yes | No (only `RUST_LOG`) | Missing alias |
| `COGNEE_LOG_FILE` toggle | Yes | No | Missing |
| `COGNEE_LOGS_DIR` env var | Yes | No | Missing |
| External-library noise suppression | `configure_external_library_logging()` (litellm, openai) | Hard-coded `ort=warn` only | Partial; needs reqwest/hyper/h2/sea_orm/qdrant defaults |
| Console output | structlog colored renderer | `tracing-subscriber::fmt` | Equivalent |
| In-memory ring buffer for HTTP API | No | `SpanBufferLayer` | Rust-only feature; must compose with new file layer |

---

## Proposed Design

### 1. Crate selection

**Recommendation: `tracing-appender` + a hand-rolled size-based rolling guard.**

- `tracing-appender` (https://docs.rs/tracing-appender) is the de-facto standard for file-based tracing output. It provides:
  - `RollingFileAppender` with `Rotation::DAILY`, `Rotation::HOURLY`, `Rotation::MINUTELY`, `Rotation::NEVER`.
  - `non_blocking(...)` wrapper that returns `(NonBlocking, WorkerGuard)` and offloads writes to a dedicated thread, so log I/O never blocks an async task. **The `WorkerGuard` must be held until shutdown** to flush the buffer.
  - File-rolling cleanup (`max_log_files(...)` + `filename_prefix` + `filename_suffix`).
- It does **not** support size-based rotation. Python's 50 MB × 5 model has no direct match.

**Trade-off — size vs time rotation:**

| Approach | Pros | Cons |
|---|---|---|
| Use `tracing-appender::Rotation::DAILY` (or HOURLY) | Stock-standard, zero custom code, multi-process-safe per-file, integrates cleanly with `non_blocking` | Bytes-per-day vary; a chatty pipeline can produce a 5 GB single file before the day ends. Loses Python-API parity for `COGNEE_LOG_MAX_BYTES`. |
| Add `tracing-rolling-file` | Crate exists and provides true size-based rotation | Lower-traffic crate (~few hundred downloads/day vs `tracing-appender`'s millions). Smaller maintenance pool. |
| Use older `file-rotate` + custom `Layer` | Battle-tested rotation logic | Requires writing a `tracing` `MakeWriter` adapter; more glue code. |
| Hand-roll a `Mutex<File>` writer that checks size on every write | Full control, can mirror Python's semantics exactly | Ownership model: must implement `MakeWriter`, plus rotation and cleanup; we re-invent stdlib `RotatingFileHandler`. |

**Recommended path:** start with `tracing-appender` + `Rotation::DAILY` (matching the spirit of "rotate periodically and keep N files"), and **document the divergence** from Python's size-based rotation. If parity becomes critical (e.g., disk-constrained edge devices), add an opt-in size-based mode behind a feature flag using `tracing-rolling-file` later. This keeps the dependency footprint small and the implementation auditable.

Add to `[workspace.dependencies]`:

```toml
tracing-appender = "0.2"
```

Enable JSON in `tracing-subscriber`:

```toml
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
```

### 2. Env-var surface (mirrors Python where reasonable)

| Variable | Default | Behavior |
|---|---|---|
| `COGNEE_LOG_FILE` | `true` | Master toggle. `false`/`0`/`no` disables file sink. |
| `COGNEE_LOGS_DIR` | `~/.cognee/logs` | Primary directory. Fallback: `/tmp/cognee_logs`. |
| `LOG_FILE_NAME` | (generated) | Absolute file path; set on parent and inherited by children for unified output. |
| `COGNEE_LOG_BACKUP_COUNT` | `5` | Translates to `RollingFileAppender::builder().max_log_files(5)`. |
| `COGNEE_LOG_MAX_BYTES` | `52428800` | Accepted for parity. Honored only when size-based mode is enabled (otherwise: warn once and proceed with time rotation). |
| `COGNEE_LOG_ROTATION` | `daily` | New Rust-native knob: `daily`/`hourly`/`minutely`/`never`/`size`. Drives `Rotation` selection. |
| `COGNEE_LOG_FORMAT` | `plain` | New Rust-native knob: `plain` or `json`. |
| `COGNEE_LOG_MAX_FILES` | `10` | Translates to `cleanup_old_logs` equivalent (or `max_log_files` if `tracing-appender` rotation drives it). |
| `LOG_LEVEL` | `INFO` | New alias for `RUST_LOG` when `RUST_LOG` is unset, mapped to `EnvFilter::new(level)`. |

`RUST_LOG` continues to take precedence when set, since it carries richer per-module syntax that `LOG_LEVEL` cannot express.

### 3. Where init lives

| Binary / Crate | Should init? |
|---|---|
| `crates/cli` (binary) | Yes — replace inline `tracing_subscriber::fmt()` call with new helper. |
| `crates/http-server` (binary) | Yes — extend `init_tracing()` to chain the file layer alongside `fmt_layer` and `SpanBufferLayer`. |
| `python/` (PyO3 bindings) | Yes — wrap in a Python-callable `setup_logging()` so the Python SDK can opt in. Important so the embedding application (e.g., the cognee Python facade) controls its own subscriber. |
| `capi/cognee-capi` (FFI) | Optional — expose a `cognee_setup_logging()` C entry point. |
| `js/` (Neon bindings) | Optional — same as C API. |
| `android/` runner | Yes — pick up `COGNEE_LOGS_DIR=/data/local/tmp/cognee/runtime/logs` automatically. |
| Library crates (anything under `crates/` other than `cli`/`http-server`) | **NO** — libraries must never install global subscribers. They emit `tracing` events; the embedder owns subscriber setup. |

### 4. Layer composition (Rust)

The layered subscriber should look like:

```
Registry
  .with(EnvFilter)
  .with(fmt_layer_stdout)        # always on
  .with(file_layer)              # only if COGNEE_LOG_FILE=true
  .with(span_buffer_layer)       # only in http-server
```

Pseudocode for a shared init helper (proposed location: a new tiny crate `cognee-logging` or inlined in `cognee-utils`):

```rust
pub struct LogGuards {
    /// Must be held for the lifetime of the process, otherwise the
    /// non_blocking writer can drop in-flight events on shutdown.
    pub _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

pub fn init_logging(
    extra_layers: impl IntoIterator<Item = Box<dyn Layer<Registry> + Send + Sync>>,
) -> LogGuards { /* ... */ }
```

The HTTP server passes its `SpanBufferLayer` via `extra_layers`; the CLI passes nothing extra. Both binaries hold the returned `LogGuards` for the lifetime of `main`.

### 5. JSON support

Recommendation: **yes, gate it behind `COGNEE_LOG_FORMAT=json`**. JSON Lines is strictly more powerful than the Python plain format for downstream ingestion (Elastic, Loki, Datadog, jq pipelines). The plain format remains the default for grep/tail-friendliness in development.

When `json` is selected, both stdout and file layers should use `fmt::layer().json()`. When `plain` is selected, file output uses a custom formatter that matches Python's `<ts> [<LEVEL>] <msg> k=v ... [logger]` to make cross-SDK log diffs grep-comparable. (`tracing-subscriber::fmt` does not produce that exact shape out of the box; we will need a small custom `FormatEvent` impl.)

---

## Library Noise Suppression

Python suppresses LiteLLM/OpenAI loggers via `configure_external_library_logging()` and a custom filter. Rust analogs (events emitted via the `log` crate's bridge or directly via `tracing`) come from:

| Crate | Typical noise at `info` |
|---|---|
| `reqwest` | request/response debug at `debug+`; mostly silent at `info` but pollutes on `RUST_LOG=debug` |
| `hyper` | extensive HTTP-state messages at `debug+` |
| `h2` | HPACK frame logs at `trace`/`debug` |
| `ort` (ONNX Runtime) | already suppressed (`ort=warn`) |
| `sea_orm` / `sqlx` | every SQL statement at `info`/`debug` |
| `qdrant_*` (segment, shard) | shard-state and indexing logs |
| `tower_http` | per-request access logs at `info` |
| `rustls` / `tokio-rustls` | TLS handshake at `debug+` |

**Recommended default filter** (replaces the current `"info,ort=warn"`):

```text
info,
ort=warn,
reqwest=warn,
hyper=warn,
h2=warn,
rustls=warn,
sqlx=warn,
sea_orm=warn,
sea_orm_migration=warn,
tower_http=warn,
qdrant_segment=warn,
qdrant_shard=warn
```

This default applies only when neither `RUST_LOG` nor `LOG_LEVEL` is set. Users can still set `RUST_LOG=debug` for full firehose. Document this in [`docs/telemetry/`](.) and surface it via CLI `--verbose`.

---

## Action Items

1. **Add dependency** — append `tracing-appender = "0.2"` to `[workspace.dependencies]` in [`Cargo.toml`](../../Cargo.toml) and enable the `json` feature on `tracing-subscriber`.
2. **Create `crates/utils/src/logging.rs`** (or a new `crates/logging` crate) exposing:
   - `LoggingConfig::from_env()` — reads all env vars listed above.
   - `init_logging(cfg, extra_layers) -> LogGuards`.
   - Helpers `resolve_logs_dir()` (mirroring Python priority) and `propagate_log_file_name()` (writes to env so children inherit).
   - Custom `FormatEvent` impl producing Python-compatible `<ts> [<LEVEL>] <msg> k=v ... [logger]` for `plain` format.
   - Cleanup pass that mirrors `cleanup_old_logs()` (glob `*.log`, sort by mtime desc, unlink past `COGNEE_LOG_MAX_FILES`).
3. **Refactor [`crates/cli/src/main.rs:50–58`](../../crates/cli/src/main.rs#L50-L58)** — replace inline `fmt()` setup with `cognee_utils::logging::init_logging(...)` and hold the returned guards in `main()`'s scope.
4. **Refactor [`crates/http-server/src/main.rs:100–118`](../../crates/http-server/src/main.rs#L100-L118)** — pass `SpanBufferLayer` as `extra_layers` to the new helper. Keep the `Arc<SpanBuffer>` wiring intact.
5. **Update default `EnvFilter`** in both binaries to the noise-suppression baseline above (or move that knowledge into `LoggingConfig::default_filter()`).
6. **Expose Python binding** in [`python/`](../../python) — a `cognee_rust.setup_logging(level=None, logs_dir=None, ...)` function that calls into the shared init helper.
7. **Add CLI flags** (optional) — `--log-level`, `--log-file`, `--log-format` to `crates/cli/src/cli.rs` for ad-hoc overrides.
8. **Document** the new env vars in `README.md`, the CLI docs under [`docs/cli/`](../cli/), and add a deployment note in [`docs/http-server/`](../http-server/).
9. **Wire Android demo** — set `COGNEE_LOGS_DIR=/data/local/tmp/cognee/runtime/logs` in [`scripts/android-run.sh`](../../scripts/android-run.sh) and [`demo/run_cognee_rust_demo_android.sh`](../../demo/run_cognee_rust_demo_android.sh).
10. **Cross-SDK parity test** in [`e2e-cross-sdk/`](../../e2e-cross-sdk) — assert that both Python and Rust create at least one `*.log` file under `COGNEE_LOGS_DIR` after a pipeline run.

---

## Open Questions

1. **Size vs time rotation.** Should we ship size-based rotation for byte-exact parity with Python (`tracing-rolling-file` or hand-rolled), or accept time-based rotation (`tracing-appender::Rotation::DAILY`) as "good enough"? Edge devices (Android) may justify size-based given fixed-storage constraints. **Lean: time-based v1, size-based behind a feature flag in v2.**
2. **JSON-only or configurable?** Always-on JSON is operationally cleaner for production but worse for grep/tail in development. **Lean: configurable with `plain` default**, since Python's CLI users are accustomed to plain text.
3. **Library-level init.** Should `cognee-lib` provide a `try_init_logging()` that runs once on first use if the binary forgot? Risk: silently swallowing the embedder's own subscriber choice. **Lean: no.** Keep init binary-only and document it loudly.
4. **Multi-process file sharing.** Python's `LOG_FILE_NAME` mechanism is multi-process-unsafe with `RotatingFileHandler`. Should the Rust port replicate the (broken) Python behavior, or sidestep it by giving each process its own file under `<dir>/<ts>-<pid>.log`? **Lean: replicate `LOG_FILE_NAME` for SDK parity, but add a doc warning about concurrent rotation.**
5. **`MAX_LOG_FILES` env-var.** Python hard-codes 10. Should Rust expose `COGNEE_LOG_MAX_FILES` and let users tune it? **Lean: yes**, since it costs nothing to add.
6. **Console + file format coupling.** When `COGNEE_LOG_FORMAT=json`, should that affect both stdout and file, or only file? **Lean: both** — easier mental model and matches what production users expect.

---

## Testing Strategy

### Unit tests (in `crates/utils/src/logging.rs` or the new logging crate)

1. **Resolution priority.**
   - `COGNEE_LOGS_DIR=<temp>` → file written under temp.
   - Read-only primary → falls back to `/tmp/cognee_logs` (use `tempfile::TempDir` set with restricted perms).
2. **Filename generation.**
   - With `LOG_FILE_NAME` set → identical filename used.
   - Without → matches `\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}\.log` and is written back to env.
3. **Rotation trigger.**
   - Configure tiny `Rotation::MINUTELY` (or size cap if size mode is implemented), emit enough events, sleep across boundary, assert two files now exist with expected naming pattern.
4. **Cleanup.**
   - Pre-create 15 `*.log` files in temp dir with monotonically older mtimes; call `cleanup_old_logs(dir, 10)`; assert exactly the 10 newest remain.
5. **Format parity.**
   - Capture a single event into a buffer using the custom `FormatEvent`; assert the line matches `r"^\d{4}-\d{2}-\d{2}T.+ \[(?:INFO    |DEBUG   |...)\] .+ \[.+\]$"`.
6. **JSON mode.**
   - With `COGNEE_LOG_FORMAT=json`, parse each captured line as JSON and assert `level`, `target`, `fields.message`.
7. **Disabled.**
   - With `COGNEE_LOG_FILE=false`, assert no files are created in the resolved dir.

### Integration tests

8. **CLI E2E.** Run `cognee add` with `COGNEE_LOGS_DIR=<temp>`; assert at least one `<temp>/*.log` exists and contains a known startup line (e.g., something matching `Logging initialized`).
9. **HTTP server E2E.** Boot the server with logging enabled; hit a route; confirm both the file and the in-memory `SpanBufferLayer` captured the event (proves the layers compose correctly).
10. **Cross-SDK E2E.** In [`e2e-cross-sdk/`](../../e2e-cross-sdk), share `COGNEE_LOGS_DIR` between the Python and Rust runs; assert both produce at least one log file (separate filenames are acceptable for v1).

### Concurrency

11. **Worker-guard correctness.** Spawn the helper, drop the guard, assert any pending log lines flushed (i.e., no truncated last line).
12. **Multi-process inheritance.** Set `LOG_FILE_NAME=<path>` in env, fork two child processes, both should append to the same file. Verify line-level interleaving is intact (no mid-line corruption) — this is the multi-process-safety smoke test.

---

## References

- Python source: `/tmp/cognee-python/cognee/shared/logging_utils.py` (clone with `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`).
- Python `BaseConfig`: `/tmp/cognee-python/cognee/base_config.py`.
- Rust CLI subscriber: [`crates/cli/src/main.rs`](../../crates/cli/src/main.rs).
- Rust HTTP-server subscriber: [`crates/http-server/src/main.rs`](../../crates/http-server/src/main.rs).
- Workspace dependencies: [`Cargo.toml`](../../Cargo.toml).
- Existing telemetry overview: [`gap-analysis.md`](./gap-analysis.md).
- `tracing-appender` docs: https://docs.rs/tracing-appender
- `tracing-rolling-file` (size-based alternative): https://docs.rs/tracing-rolling-file
- `tracing-subscriber` JSON formatter: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/format/struct.Json.html
- Python stdlib `RotatingFileHandler`: https://docs.python.org/3/library/logging.handlers.html#rotatingfilehandler
