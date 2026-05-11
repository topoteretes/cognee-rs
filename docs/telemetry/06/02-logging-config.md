# Task 06-02 — Create `cognee-logging` crate skeleton + `LoggingConfig`

**Status**: implemented in commit 86f7e1e (note: `license.workspace = true` omitted from `crates/logging/Cargo.toml` because root `[workspace.package]` has no `license` field)
**Owner**: _unassigned_
**Depends on**: [Task 06-01 — Workspace deps](01-workspace-deps.md).
**Blocks**:
- [Task 06-03 — Path helpers & cleanup](03-paths-and-cleanup.md) (live in the same crate).
- [Task 06-04 — Python plain formatter](04-python-plain-formatter.md) (lives in the same crate).
- [Task 06-05 — init_logging](05-init-logging.md) (consumes `LoggingConfig`).
- [Task 06-08 — Binding entrypoints](08-binding-entrypoints.md) (calls `LoggingConfig::from_env()`).

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 1 (rotation kind), 2 (new crate), 3 (format toggle), 6 (default filter), 7 (`LOG_LEVEL` fallback), 14 (`COGNEE_LOG_MAX_FILES`).

---

## 1. Goal

Create the new `cognee-logging` workspace crate under
[`crates/logging/`](../../../crates/logging/) with three sources of
truth in this task:

1. The `Cargo.toml` declaring `tracing`, `tracing-subscriber`,
   `tracing-appender`, `serde_json`, `thiserror`, `chrono`, plus
   tests-only `tempfile` and `serial_test`.
2. A `lib.rs` exposing the public API surface (re-exports + module
   declarations).
3. A `config.rs` module containing the `LoggingConfig` struct, its
   enums (`LogFormat`, `LogRotation`), and the `LoggingConfig::from_env()`
   parser.

No subscriber install, no file writing, no formatter. Those land in
06-03, 06-04, and 06-05.

## 2. Rationale

- Decision 2 locked a new workspace crate (not a module inside
  `cognee-utils`). Putting it on its own keeps the dependency
  boundary explicit: only `cli`, `http-server`, and the three
  binding crates depend on `cognee-logging`. Library crates must
  not pull it in.
- `LoggingConfig` is the env-var parsing seam. Lifting it out of
  `init_logging` lets the binding entrypoints (task 06-08) and the
  binaries (tasks 06-06/07) share the same parsing, and lets unit
  tests assert env-var parsing without installing a global
  subscriber.

## 3. Pre-conditions

- Task 06-01 committed: `tracing-appender` is in
  `[workspace.dependencies]` and `tracing-subscriber` has the `json`
  feature.
- `crates/logging/` does not exist yet.
- `serial_test` is already a workspace dev-dep (used by gap 04
  tests); verify with `grep serial_test Cargo.toml` before declaring
  it in the new crate.

## 4. Step-by-step

### 4.1 Add the crate to the workspace members list

Edit [`Cargo.toml`](../../../Cargo.toml). In the `members = [ ... ]`
array (lines 7–33), insert `"crates/logging",` alphabetically near
`"crates/lib",`.

### 4.2 Create `crates/logging/Cargo.toml`

```toml
[package]
name = "cognee-logging"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
tracing.workspace = true
tracing-subscriber = { workspace = true, features = ["env-filter", "fmt", "json"] }
tracing-appender.workspace = true
serde_json.workspace = true
thiserror.workspace = true
chrono.workspace = true

[dev-dependencies]
tempfile.workspace = true
serial_test.workspace = true
```

If `chrono` is not yet in `[workspace.dependencies]`, add it
(`chrono = { version = "0.4", default-features = false, features = ["clock"] }`)
in the same commit. (It is almost certainly already there — check
first.)

### 4.3 Create `crates/logging/src/lib.rs`

```rust
//! Shared logging setup for the cognee Rust SDK.
//!
//! `cognee-logging` is the single home of file-based logging with
//! rotation, the Python-compatible plain text formatter, and the
//! default library-noise-suppressing `EnvFilter`. Binaries
//! (`cognee-cli`, `cognee-http-server`) and bindings (Python / JS /
//! C) call [`init_logging`] to install a global subscriber; library
//! crates **must not** depend on this crate.

#![deny(missing_docs)]

mod config;
// Future modules — declared by sibling tasks:
// mod paths;        // 06-03: resolve_logs_dir + propagate_log_file_name + cleanup_old_logs
// mod formatter;    // 06-04: PythonPlainFormatter
// mod init;         // 06-05: init_logging + LogGuards + default_filter

pub use config::{LogFormat, LogRotation, LoggingConfig, LoggingConfigError};
```

### 4.4 Create `crates/logging/src/config.rs`

Sketch (full code lands in the implementation, this is the binding
shape):

```rust
use std::path::PathBuf;
use thiserror::Error;

/// Output format for both stdout and file sinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Python-byte-exact `<ts> [<LEVEL>] <msg> k=v ... [logger]`.
    Plain,
    /// JSON Lines via `tracing-subscriber::fmt::layer().json()`.
    Json,
}

/// Time-based rotation cadence. Size-based deferred per decision 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogRotation {
    Daily,
    Hourly,
    Minutely,
    Never,
}

#[derive(Debug, Error)]
pub enum LoggingConfigError {
    #[error("invalid value for {var}: {value!r} (expected one of: {expected})")]
    InvalidValue { var: &'static str, value: String, expected: &'static str },
    #[error("invalid integer for {var}: {source}")]
    InvalidInt { var: &'static str, #[source] source: std::num::ParseIntError },
}

#[derive(Debug, Clone)]
pub struct LoggingConfig {
    /// `COGNEE_LOG_FILE` toggle. `false`/`0`/`no` → no file sink.
    pub file_enabled: bool,
    /// Resolved logs dir, or `None` when file logging is disabled or
    /// no writable directory could be found. Set later by
    /// `resolve_logs_dir` (06-03) — `from_env` only captures the
    /// user-requested override here.
    pub logs_dir_override: Option<PathBuf>,
    /// Pre-resolved file path (from `LOG_FILE_NAME` env var) for
    /// multi-process inheritance. `None` → generate on first init.
    pub log_file_name: Option<PathBuf>,
    /// Time-based rotation cadence.
    pub rotation: LogRotation,
    /// Plain vs JSON.
    pub format: LogFormat,
    /// `COGNEE_LOG_BACKUP_COUNT` — passed to `tracing-appender`'s
    /// `max_log_files` builder hint.
    pub backup_count: usize,
    /// `COGNEE_LOG_MAX_FILES` (decision 14) — used by
    /// `cleanup_old_logs` startup pass. Defaults to 10.
    pub max_files: usize,
    /// Resolved `EnvFilter` directive string (see decisions 6 + 7).
    /// `None` → caller substitutes [`default_filter`].
    pub level_filter: Option<String>,
}

impl LoggingConfig {
    pub fn from_env() -> Result<Self, LoggingConfigError> {
        // 1. file_enabled: COGNEE_LOG_FILE, default true; false/0/no => false.
        // 2. logs_dir_override: COGNEE_LOGS_DIR, raw PathBuf or None.
        // 3. log_file_name: LOG_FILE_NAME, raw PathBuf or None.
        // 4. rotation: COGNEE_LOG_ROTATION, default Daily; parse {daily,hourly,minutely,never}.
        // 5. format: COGNEE_LOG_FORMAT, default Plain; parse {plain,json}.
        // 6. backup_count: COGNEE_LOG_BACKUP_COUNT, default 5.
        // 7. max_files: COGNEE_LOG_MAX_FILES, default 10.
        // 8. level_filter: precedence RUST_LOG > LOG_LEVEL > None.
        //    LOG_LEVEL is mapped through the same EnvFilter parser
        //    (so `info` is accepted as the bare level).
        todo!("implementor fills body")
    }
}
```

Notes for the implementor:

- Use a single `parse_bool(name) -> bool` helper for `COGNEE_LOG_FILE`
  (and any future bool vars). Truthy default unless value is one of
  `"false" | "0" | "no"` (case-insensitive). Matches Python's
  `.lower() not in ("false", "0", "no")` semantics in
  `logging_utils.py:501`.
- `COGNEE_LOG_MAX_BYTES` is accepted but **not validated** in v1
  (decision 1 deferred size rotation). Document the variable in a
  doc comment on `LoggingConfig` but do not store it as a field. If
  it is set, log a one-shot `warn!` at startup ("size-based rotation
  is not yet supported; using time-based rotation"). The warn site
  lives in `init_logging` (task 06-05), not in `from_env` — keep
  `from_env` free of `tracing::*` calls.

### 4.5 Unit tests

Add `#[cfg(test)] mod tests` at the bottom of `config.rs` with
`#[serial_test::serial]` on every test that mutates env vars.
Required cases:

1. Empty env → defaults: `file_enabled = true`, `rotation = Daily`,
   `format = Plain`, `backup_count = 5`, `max_files = 10`,
   `level_filter = None`.
2. `COGNEE_LOG_FILE=false` → `file_enabled = false`. Same for `"0"`
   and `"no"` (case-insensitive).
3. `COGNEE_LOGS_DIR=/tmp/foo` → `logs_dir_override = Some(PathBuf::from("/tmp/foo"))`.
4. `LOG_FILE_NAME=/tmp/foo/x.log` → `log_file_name = Some(...)`.
5. `COGNEE_LOG_ROTATION=hourly` → `rotation = Hourly`. Invalid value
   → `LoggingConfigError::InvalidValue`.
6. `COGNEE_LOG_FORMAT=json` → `format = Json`. Invalid → error.
7. `COGNEE_LOG_BACKUP_COUNT=3` → `backup_count = 3`. Non-integer →
   `LoggingConfigError::InvalidInt`.
8. `COGNEE_LOG_MAX_FILES=20` → `max_files = 20`.
9. `RUST_LOG=debug,foo=warn` → `level_filter = Some("debug,foo=warn".into())`.
10. `RUST_LOG` unset, `LOG_LEVEL=DEBUG` → `level_filter = Some("DEBUG".into())`.
11. Both `RUST_LOG` and `LOG_LEVEL` set → `RUST_LOG` wins.

All tests must save/restore env vars (`temp_env` crate is NOT
required — a small `EnvGuard` RAII struct in the test module is
sufficient, or use `std::env::set_var` + manual cleanup via a `Drop`
guard).

## 5. Verification

```bash
# 1. Crate compiles standalone.
cargo check -p cognee-logging --all-targets

# 2. Unit tests pass.
cargo test -p cognee-logging

# 3. Workspace still compiles (no accidental cross-crate breakage).
cargo check --all-targets

# 4. Clippy.
cargo clippy -p cognee-logging --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`Cargo.toml`](../../../Cargo.toml) — add `"crates/logging"` to
  `members`.
- `crates/logging/Cargo.toml` — NEW.
- `crates/logging/src/lib.rs` — NEW.
- `crates/logging/src/config.rs` — NEW.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Env-var-mutating tests flake under `cargo test --jobs N` | Medium | All env-mutating tests must be `#[serial_test::serial]`. The full-check script already runs tests with `--test-threads=1` for LLM isolation, but `cargo test -p cognee-logging` directly may parallelize. |
| `chrono` not yet a workspace dep | Low (it is used by other crates already, e.g. `cognee-models`) | If missing, add to `[workspace.dependencies]` in this same commit. |
| `#![deny(missing_docs)]` makes adding undocumented items painful | Low | Keep public surface minimal; private helpers stay free of doc comments. |

## 8. Out of scope

- The path resolver and cleanup pass — task 06-03.
- The formatter — task 06-04.
- Anything that touches `tracing::subscriber` install — task 06-05.
- `COGNEE_LOG_MAX_BYTES` honouring — decision 1 deferred it.
- CLI flags for any of these env vars — decision 8 locked
  env-var-only.
