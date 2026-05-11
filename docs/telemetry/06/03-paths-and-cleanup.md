# Task 06-03 — `resolve_logs_dir`, `propagate_log_file_name`, `cleanup_old_logs`

**Status**: implemented in commit 038e6a8 (note: `dirs.workspace = true` added to `crates/logging/Cargo.toml`; the `filetime`-based mtime ordering in test 6 was replaced with `std::thread::sleep`-based ordering since `filetime` is not a workspace dep)
**Owner**: _unassigned_
**Depends on**: [Task 06-02 — Logging config](02-logging-config.md).
**Blocks**:
- [Task 06-05 — init_logging](05-init-logging.md) (composes these helpers).

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 5 (`LOG_FILE_NAME` multi-process inheritance), 11 (cleanup is startup-only), 14 (`COGNEE_LOG_MAX_FILES`).

---

## 1. Goal

Add three helpers to `cognee-logging` (new module
`crates/logging/src/paths.rs`), each mirroring the corresponding
Python function in
[`/tmp/cognee-python/cognee/shared/logging_utils.py`](https://github.com/topoteretes/cognee/blob/main/cognee/shared/logging_utils.py):

1. `resolve_logs_dir(cfg: &LoggingConfig) -> Option<PathBuf>` —
   priority list: `COGNEE_LOGS_DIR` (or `cfg.logs_dir_override`) →
   `~/.cognee/logs` → `/tmp/cognee_logs` → `None`. Tries `mkdir -p`
   and `W_OK` access at each level.
2. `propagate_log_file_name(dir: &Path) -> PathBuf` — generates a
   timestamped filename (`<YYYY-MM-DD_HH-MM-SS>.log`) the **first**
   time it is called in a process, writes the absolute path back to
   `LOG_FILE_NAME` via `std::env::set_var`, and returns the path on
   every subsequent call (idempotent within a process). Mirrors
   Python's parent-writes-env / children-inherit behaviour.
3. `cleanup_old_logs(dir: &Path, max_files: usize)` — glob `*.log` in
   `dir`, sort by mtime descending, unlink everything past index
   `max_files - 1`. Logs errors at `warn!` level but never propagates
   them (cleanup failure must not break startup).

All three are pure file-system helpers — no `tracing::subscriber`
involvement. Task 06-05 wires them together inside `init_logging`.

## 2. Rationale

- Splitting these helpers out of `init_logging` keeps each one
  unit-testable against a `tempfile::TempDir` without installing a
  global subscriber.
- Python's resolution priority falls back to `/tmp/cognee_logs` when
  the primary location is unwritable. Edge devices (Android) hit
  this path because `$HOME` resolves to a read-only location
  (`/root` on adb shell). The fallback is the difference between
  "no logs ever" and "logs in `/tmp`".
- Decision 5 explicitly chose to replicate Python's `LOG_FILE_NAME`
  multi-process behaviour. The race in concurrent rotation is
  documented here (and again in the parent doc's "Risks" section).
- Decision 11 chose startup-only cleanup. A periodic task would
  require a background thread + shutdown handshake — not worth the
  complexity for a one-shot disk-bound prune.

## 3. Pre-conditions

- Task 06-02 committed: `crates/logging/` exists with
  `LoggingConfig`.
- `crates/logging/src/paths.rs` does not exist yet.
- The Python reference at `/tmp/cognee-python/cognee/shared/logging_utils.py`
  is cloned (see CLAUDE.md). If missing, clone it first.

## 4. Step-by-step

### 4.1 Create `crates/logging/src/paths.rs`

Public API:

```rust
use std::path::{Path, PathBuf};

/// Resolve the directory for log files, with fallback.
///
/// Priority:
/// 1. `cfg.logs_dir_override` (set from `COGNEE_LOGS_DIR`).
/// 2. `~/.cognee/logs` (default).
/// 3. `/tmp/cognee_logs` (last-resort fallback).
/// 4. `None` — file logging is silently skipped.
///
/// Each candidate is `mkdir -p`'d and tested with a write probe.
pub fn resolve_logs_dir(cfg: &crate::LoggingConfig) -> Option<PathBuf> {
    // Reference: cognee/shared/logging_utils.py:103-132.
    todo!()
}

/// Resolve the active log file path for this process.
///
/// On first call within a process: generates
/// `<dir>/<YYYY-MM-DD_HH-MM-SS>.log`, writes the absolute path to
/// `LOG_FILE_NAME` via `std::env::set_var`, and returns it. On
/// subsequent calls, reads `LOG_FILE_NAME` back and returns the
/// stored path (idempotent).
///
/// **Multi-process note**: child processes that inherit
/// `LOG_FILE_NAME` from the parent will return the parent's filename
/// on first call, so all processes append to one file. There is
/// **no** rotation lock — concurrent rotation by `tracing-appender`
/// from multiple processes may corrupt files. This is intentional
/// Python parity (see [`06-file-logging-rotation.md`](../06-file-logging-rotation.md)
/// "Multi-Process Coordination").
pub fn propagate_log_file_name(dir: &Path) -> PathBuf {
    // Reference: cognee/shared/logging_utils.py:511-519.
    todo!()
}

/// Delete `*.log` files in `dir` beyond `max_files`, oldest first.
///
/// Errors are logged at `warn!` and swallowed; never propagated.
/// Called exactly once at startup from `init_logging` (decision 11).
pub fn cleanup_old_logs(dir: &Path, max_files: usize) {
    // Reference: cognee/shared/logging_utils.py:271-308.
    todo!()
}
```

### 4.2 Implementation hints

`resolve_logs_dir`:

- Use `dirs::home_dir()` for the `~/.cognee/logs` candidate (the
  `dirs` crate is already in the workspace — verify with `grep '^dirs' Cargo.toml`;
  if not, add it).
- Write probe: create-and-delete a `.cognee_write_probe` file rather
  than relying on `unix::PermissionsExt` (Android's `/data/local/tmp`
  has weird perm bits that confuse `os.access` equivalents).
- Return `None` only when all three candidates fail. Emit a
  `tracing::warn!` once in that case — but **not** here. Add a comment
  saying "caller (init_logging) is responsible for warning the user";
  this keeps `paths.rs` free of subscriber interaction (allowing
  tests to run before any subscriber is installed).

`propagate_log_file_name`:

- Timestamp format: `%Y-%m-%d_%H-%M-%S` (matches
  `logging_utils.py:512`). Use `chrono::Local::now().format(...)`.
- Resolve to absolute via `dunce::canonicalize` or `Path::canonicalize`.
  Note that `canonicalize` requires the file to exist — call it after
  `std::fs::File::create_new(...).ok()` so the file is touched before
  canonicalisation. (This also mirrors the
  [Android-runtime note in MEMORY.md](file:///home/dmytro/.claude/projects/-home-dmytro-dev-cognee-cognee-rust/memory/MEMORY.md):
  `SqlitePool::connect()` requires pre-creating the file — same
  principle, different consumer.)
- `std::env::set_var("LOG_FILE_NAME", &path)` only if the env var is
  not already set. Idempotent: reading-then-writing means parent and
  children agree.

`cleanup_old_logs`:

- Use `std::fs::read_dir` rather than `glob` to avoid pulling another
  crate. Filter on `path.extension() == Some(OsStr::new("log"))`.
- Sort by `metadata().modified()` descending.
- `std::fs::remove_file` on each entry past `max_files - 1`. On error,
  `tracing::warn!(?path, ?err, "failed to delete old log file");` —
  this is the one place `tracing` is fine to use because cleanup runs
  after subscribers are installed (decision 11 says startup-only at
  end of `init_logging`).
- Never propagate the error.

### 4.3 Tests in `paths.rs`

`#[cfg(test)] mod tests` with:

1. **`resolve_logs_dir_uses_override`** — `cfg.logs_dir_override =
   Some(temp_dir)`; returns `Some(temp_dir)`. Confirm directory is
   created (`mkdir -p` semantics).
2. **`resolve_logs_dir_falls_back_to_tmp`** — pass a path the test
   cannot write to (e.g. `/proc/some-fake`) as override; assert
   fallback to `/tmp/cognee_logs`. Skip this test on Windows
   (`#[cfg(unix)]`).
3. **`resolve_logs_dir_returns_none_when_all_fail`** — mock by
   pointing override at an unwritable path AND temporarily monkey-
   patching `tmp` to one too. Hard to do hermetically in unit tests;
   skip or accept relaxed coverage with a comment. (Integration
   tests in task 06-10 catch the realistic case.)
4. **`propagate_log_file_name_generates_timestamped_when_unset`**
   `#[serial_test::serial]` — unset `LOG_FILE_NAME`; call once;
   assert returned path matches
   `^\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}\.log$` regex on its filename
   and `LOG_FILE_NAME` is now set in env. Cleanup env var at
   end-of-test.
5. **`propagate_log_file_name_is_idempotent_when_set`**
   `#[serial_test::serial]` — pre-set `LOG_FILE_NAME=/tmp/x.log`;
   call; assert returned path equals the pre-set value; env var
   unchanged.
6. **`cleanup_old_logs_keeps_n_newest`** — create 15 `*.log` files
   in temp dir with `std::fs::File::create` then manually set mtimes
   via `filetime` crate (already a workspace dep — verify) so they
   are monotonically older; call `cleanup_old_logs(dir, 10)`; assert
   10 newest remain.
7. **`cleanup_old_logs_ignores_non_log_files`** — pre-create 12
   `*.log` and 3 `*.txt`; call `cleanup_old_logs(dir, 10)`; assert
   exactly 10 `*.log` files and all 3 `*.txt` files remain.

If `filetime` is not in the workspace, switch test 6 to creating
files with `std::thread::sleep(Duration::from_millis(10))` between
each `File::create` so mtimes naturally differ. The runbook test
will be slower (≈150 ms) but hermetic.

### 4.4 Wire the module into `lib.rs`

In `crates/logging/src/lib.rs`, replace the existing comment line:

```rust
// mod paths;        // 06-03: resolve_logs_dir + propagate_log_file_name + cleanup_old_logs
```

with:

```rust
mod paths;
pub use paths::{cleanup_old_logs, propagate_log_file_name, resolve_logs_dir};
```

## 5. Verification

```bash
# 1. Crate compiles.
cargo check -p cognee-logging --all-targets

# 2. Unit tests pass.
cargo test -p cognee-logging paths

# 3. Workspace still compiles.
cargo check --all-targets

# 4. Clippy.
cargo clippy -p cognee-logging --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- `crates/logging/src/paths.rs` — NEW.
- `crates/logging/src/lib.rs` — wire the new module + re-exports.
- `crates/logging/Cargo.toml` — add `dirs` (and `filetime` if used
  in tests) under `[dependencies]` / `[dev-dependencies]` if not
  already inherited from workspace.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Env-mutating tests for `propagate_log_file_name` flake under parallel `cargo test` | Medium | `#[serial_test::serial]`. |
| `Path::canonicalize` fails on Android paths with symlinks | Low | Use the absolute-path strategy from `crates/storage/src/local.rs::ensure_absolute_path` if it already exists, otherwise `dunce::canonicalize`. |
| Multi-process rotation race corrupts the shared log file | Documented (decision 5) | This is intentional Python parity. The parent doc has a "Multi-Process Coordination" warning; sub-doc 06-11 propagates it into `README.md`. |
| Cleanup runs **before** the `WorkerGuard` flushes, evicting the just-rotated file | Low — cleanup runs once at startup, not at every rotation | Order in `init_logging` is: (1) install subscriber, (2) `propagate_log_file_name`, (3) `cleanup_old_logs`. Cleanup acts on pre-existing files, never on the active file. |

## 8. Out of scope

- Per-rotation cleanup (decision 11 locked startup-only).
- Per-PID file naming (decision 5 chose `LOG_FILE_NAME` parity).
- Locking the shared file across processes — explicitly accepted as
  unsafe.
- `dirs::config_dir()` resolution — the Python equivalent is
  `~/.cognee/logs`, which is a logs-specific path, not the OS config
  dir. Mirror Python exactly.
