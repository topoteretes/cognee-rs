# Task 06-10 — Tests for file logging + rotation + cross-SDK parity

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: 06-02 through 06-08 (every implementation task).
**Blocks**:
- [Task 06-11 — Docs + CI](11-docs-and-ci.md) (CI wires this task's test into the parity lane).

**Parent doc**: [06 — File-Based Logging with Rotation](../06-file-logging-rotation.md)
**Locked decisions**: 4 (Python-byte-exact format), 5 (`LOG_FILE_NAME` inheritance), 11 (cleanup is startup-only), 12 (loose filename / strict message cross-SDK).

---

## 1. Goal

Layered test coverage matching the gap-05 pattern:

1. **Unit tests** inside `crates/logging/` (already authored in
   tasks 02–05 as part of each component; this task adds the
   integration gaps not covered there).
2. **CLI integration test** in `crates/cli/tests/logging_e2e.rs`:
   run `cognee --help` with `COGNEE_LOGS_DIR=<temp>`, assert
   `*.log` exists and contains the known "Logging initialized" line.
3. **HTTP server integration test** in
   `crates/http-server/tests/logging_e2e.rs`: boot the server, hit
   `/healthz`, assert (a) log file exists, (b) the `SpanBuffer`
   captured the request (proving `extra_layers` composes).
4. **Multi-process `LOG_FILE_NAME` inheritance test** in
   `crates/logging/tests/multi_process_inheritance.rs`: parent
   calls `init_logging`, spawns two children via
   `std::process::Command`; assert all three appended to the same
   file. Children must be spawned with the inherited env so
   `LOG_FILE_NAME` propagates.
5. **Cross-SDK parity test** at
   `e2e-cross-sdk/harness/test_logging_parity.py`: share
   `COGNEE_LOGS_DIR` between the Python and Rust runs, assert per
   decision 12 — both create at least one `*.log` AND a known
   shared message has byte-equal body (between timestamp and
   logger bracket) in both files.

## 2. Rationale

- Unit tests live with the code (`paths.rs`, `config.rs`,
  `formatter.rs`, `init.rs` — each module owns its own). This task
  catches the integration concerns that span modules or binaries.
- The CLI / HTTP server tests are *behaviour* tests: do the
  binaries, end-to-end, write the file they're supposed to write?
- Multi-process inheritance is decision 5's contract; without a
  test it would silently regress to per-PID files.
- Cross-SDK parity is decision 12's contract; this is the only
  end-to-end signal that the formatter (decision 4) actually
  matches Python's output.

## 3. Pre-conditions

- Tasks 06-02 through 06-08 all committed.
- `e2e-cross-sdk` docker harness builds and runs locally; verify
  with `cd e2e-cross-sdk && docker compose build`.
- The Python `cognee` package is importable inside the harness's
  Python venv (it ships with the upstream package).

## 4. Step-by-step

### 4.1 CLI integration test

Create [`crates/cli/tests/logging_e2e.rs`](../../../crates/cli/tests/logging_e2e.rs):

```rust
use std::process::Command;
use tempfile::tempdir;

#[test]
fn cli_creates_log_file_in_cognee_logs_dir() {
    let dir = tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_cognee-cli");

    let output = Command::new(bin)
        .env("COGNEE_LOGS_DIR", dir.path())
        .env_remove("LOG_FILE_NAME")
        .env_remove("RUST_LOG")
        .arg("--help")
        .output()
        .expect("spawn cognee_cli");

    assert!(output.status.success(), "cognee --help should succeed");

    // Wait for non-blocking writer to flush (process exit drops
    // WorkerGuard which flushes; but stat-read race possible).
    std::thread::sleep(std::time::Duration::from_millis(200));

    let log_files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read tempdir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("log"))
        .collect();
    assert!(!log_files.is_empty(), "expected at least one *.log file");

    let contents = std::fs::read_to_string(&log_files[0].path()).expect("read log");
    assert!(
        contents.contains("Logging initialized"),
        "expected 'Logging initialized' anchor line, got: {contents}"
    );
}
```

The exact binary name comes from `CARGO_BIN_EXE_<binary>`. The
binary name in [`crates/cli/Cargo.toml`](../../../crates/cli/Cargo.toml)
is `cognee-cli` (`[[bin]] name = "cognee-cli"`), so the env var is
`CARGO_BIN_EXE_cognee-cli` (with the hyphen, not an underscore).

### 4.2 HTTP server integration test

Create [`crates/http-server/tests/logging_e2e.rs`](../../../crates/http-server/tests/logging_e2e.rs):

```rust
// Boot the server in-process (not as subprocess) so we can inspect
// AppState.spans after the request. Use a random port via tokio's
// bind-to-0 pattern.
//
// Implementor: see existing HTTP server integration tests in
// crates/http-server/tests/ for the in-process boot pattern. If
// none exist, the simplest approach is:
//
// 1. Build HttpServerConfig with port = 0.
// 2. Call cognee_logging::init_logging inside the test (saved
//    once-per-process via OnceLock to avoid double-init across
//    parallel tests).
// 3. tokio::spawn(cognee_http_server::run(...)).
// 4. Issue GET /healthz via reqwest.
// 5. Assert (a) log file exists in COGNEE_LOGS_DIR; (b) spans
//    buffer has at least one entry.

#[tokio::test]
async fn server_writes_to_file_and_buffers_spans() {
    // (See implementor notes above.)
}
```

If standing up the full server in tests is too heavyweight for this
gap, the alternative is a focused "layer composition" unit test:
construct a `Registry::default().with(env_filter).with(file_layer)
.with(span_buffer_layer).try_init()` directly, emit one event,
assert both the file and the buffer captured it. Either approach
satisfies decision 13.

### 4.3 Multi-process inheritance test

Create [`crates/logging/tests/multi_process_inheritance.rs`](../../../crates/logging/tests/multi_process_inheritance.rs):

```rust
use std::process::Command;
use tempfile::tempdir;

/// Helper binary: a tiny test bin that calls `init_logging` and
/// emits one info line, then exits. Define as a `[[bin]]` in
/// `crates/logging/Cargo.toml` named "logging_child_smoke":
///
/// [[bin]]
/// name = "logging_child_smoke"
/// path = "tests/bin/child_smoke.rs"
/// required-features = []
///
/// child_smoke.rs:
///   fn main() {
///       let cfg = cognee_logging::LoggingConfig::from_env().unwrap();
///       let _g = cognee_logging::init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
///       tracing::info!(pid = std::process::id(), "child emitted");
///   }
#[test]
fn children_inherit_log_file_name_from_parent() {
    let dir = tempdir().unwrap();
    std::env::set_var("COGNEE_LOGS_DIR", dir.path());
    std::env::remove_var("LOG_FILE_NAME");

    // Parent init: this writes LOG_FILE_NAME to env.
    let cfg = cognee_logging::LoggingConfig::from_env().unwrap();
    let _parent_guard = cognee_logging::init_logging(
        cfg,
        std::iter::empty::<cognee_logging::BoxedLayer>(),
    );
    tracing::info!("parent emitted");

    let parent_filename = std::env::var("LOG_FILE_NAME").expect("set by parent");

    // Spawn two children with inherited env.
    let child_bin = env!("CARGO_BIN_EXE_logging_child_smoke");
    for _ in 0..2 {
        let status = Command::new(child_bin).status().expect("spawn child");
        assert!(status.success());
    }

    std::thread::sleep(std::time::Duration::from_millis(300));

    // All three should have written to the same file.
    let parent_path = std::path::Path::new(&parent_filename);
    let contents = std::fs::read_to_string(parent_path).expect("read log");
    assert!(contents.contains("parent emitted"));
    assert!(contents.matches("child emitted").count() >= 2);

    std::env::remove_var("COGNEE_LOGS_DIR");
}
```

This test must be `#[serial_test::serial]` (already a dev-dep). The
helper binary `logging_child_smoke` lives at
`crates/logging/tests/bin/child_smoke.rs` per the bin layout above.

### 4.4 Cross-SDK parity test

Create [`e2e-cross-sdk/harness/test_logging_parity.py`](../../../e2e-cross-sdk/harness/test_logging_parity.py).
Pattern: mirror
[`e2e-cross-sdk/harness/test_provenance_parity.py`](../../../e2e-cross-sdk/harness/test_provenance_parity.py)
structure (helper fixtures for both SDKs, shared workspace, side-by-
side run).

```python
"""Cross-SDK file-logging parity test (gap 06).

Asserts:
  1. Both Python and Rust SDKs create at least one *.log file under
     a shared COGNEE_LOGS_DIR after invoking a known no-op command.
  2. For a shared synthetic message anchor ("Logging initialized"),
     the line body — content between the timestamp and the trailing
     `[<logger>]` bracket — is byte-equal across SDKs.

Per decision 12: loose at the filename level (separate files are OK
because each process picks its own LOG_FILE_NAME), strict at the
message-body level.
"""
import re
import subprocess
from pathlib import Path

from helpers import RUST_CLI, PYTHON_CLI  # existing harness helpers

LINE_RE = re.compile(
    r"^(?P<ts>\S+) \[(?P<level>[A-Z ]{8})\] (?P<body>.*) \[(?P<logger>[^\]]+)\]\s*$"
)


def _read_logs(dir_: Path) -> list[str]:
    return [
        line
        for p in sorted(dir_.glob("*.log"))
        for line in p.read_text().splitlines()
    ]


def _find_anchor(lines: list[str], substring: str) -> str | None:
    for line in lines:
        m = LINE_RE.match(line)
        if not m:
            continue
        if substring in m.group("body"):
            return m.group("body")
    return None


def test_both_sdks_create_log_files(tmp_path, python_env, rust_env):
    py_logs = tmp_path / "py_logs"
    rs_logs = tmp_path / "rs_logs"
    py_logs.mkdir()
    rs_logs.mkdir()

    subprocess.run(
        [PYTHON_CLI, "--help"],
        env={**python_env, "COGNEE_LOGS_DIR": str(py_logs)},
        check=True,
    )
    subprocess.run(
        [RUST_CLI, "--help"],
        env={**rust_env, "COGNEE_LOGS_DIR": str(rs_logs)},
        check=True,
    )

    assert any(py_logs.glob("*.log")), "Python SDK did not create any log file"
    assert any(rs_logs.glob("*.log")), "Rust SDK did not create any log file"

    py_anchor = _find_anchor(_read_logs(py_logs), "Logging initialized")
    rs_anchor = _find_anchor(_read_logs(rs_logs), "Logging initialized")

    assert py_anchor is not None, "Python anchor line not found"
    assert rs_anchor is not None, "Rust anchor line not found"

    # Decision 12 — per-message strict equality. Both SDKs emit the
    # same anchor string with no trailing structured fields.
    assert py_anchor == rs_anchor, (
        f"anchor body differs:\n  Python: {py_anchor!r}\n  Rust:   {rs_anchor!r}"
    )
```

Implementor cross-checks: the Python SDK emits its anchor line via
`logging_utils.py:setup_logging()`. Confirm Python's exact anchor
text and align the Rust `tracing::info!("Logging initialized", ...)`
to match it — or pick a different shared anchor that both SDKs can
emit identically. If the Python anchor includes a fields-style
suffix like `file=...`, normalise both sides by stripping
trailing-field tokens before comparison; document the
normalisation in the docstring.

Wire the test into the existing harness fixtures (`python_env`,
`rust_env`, `PYTHON_CLI`, `RUST_CLI` are already exported by
`e2e-cross-sdk/harness/helpers.py` based on the provenance parity
test).

### 4.5 Optional smoke for the JSON format

A small unit test in `crates/logging/src/init.rs` that flips
`COGNEE_LOG_FORMAT=json`, emits one event, and asserts each line
parses as a JSON object with `level`, `target`, `fields.message`
keys. Place in the existing `mod tests` block.

## 5. Verification

```bash
# 1. Unit tests inside cognee-logging.
cargo test -p cognee-logging

# 2. CLI integration test.
cargo test -p cognee-cli --test logging_e2e

# 3. HTTP server integration test.
cargo test -p cognee-http-server --test logging_e2e

# 4. Multi-process inheritance.
cargo test -p cognee-logging --test multi_process_inheritance

# 5. Cross-SDK parity (docker required).
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit \
    --exit-code-from harness 2>&1 | tee /tmp/parity.log
grep -E "test_logging_parity.*(PASSED|FAILED)" /tmp/parity.log

# 6. Full check.
scripts/check_all.sh
```

## 6. Files modified

- `crates/cli/tests/logging_e2e.rs` — NEW.
- `crates/http-server/tests/logging_e2e.rs` — NEW (or the in-place
  layer composition test if full-server is too heavyweight).
- `crates/logging/tests/multi_process_inheritance.rs` — NEW.
- `crates/logging/tests/bin/child_smoke.rs` — NEW (helper bin).
- `crates/logging/Cargo.toml` — declare `[[bin]] logging_child_smoke`.
- `e2e-cross-sdk/harness/test_logging_parity.py` — NEW.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Sleep-based synchronisation for `non_blocking` flush is flaky | Medium | The 200–300 ms sleeps are conservative; the `WorkerGuard` drop on subprocess exit is the actual flush trigger. If flakes appear, switch to polling for file size > 0 with a 2-second deadline. |
| `CARGO_BIN_EXE_*` env var name doesn't match the actual binary name | Medium | Verify against `crates/cli/Cargo.toml` and `crates/logging/Cargo.toml` `[[bin]]` declarations. |
| Python anchor differs from Rust anchor in details (capitalisation, trailing punctuation) | High at first integration | Align in this task: pick one canonical string ("Logging initialized" with no period) and ensure both SDKs emit exactly that. If Python's `setup_logging` doesn't emit such a line, accept a small Python-side patch in the harness `conftest.py` that wraps Python's `setup_logging` to also emit the anchor. |
| Docker harness changes break unrelated tests | Low | Re-running existing parity tests in CI catches it. |
| Multi-process test's `LOG_FILE_NAME` env leaks into other tests in the same `cargo test` invocation | Medium | `#[serial_test::serial]` plus explicit `remove_var` at start and end. |

## 8. Out of scope

- Property-based tests over arbitrary log lines. The formatter is
  simple enough that example-based unit tests cover the cases.
- Size-based rotation tests. Decision 1 deferred size-based
  rotation; tests follow.
- Performance benchmarks. The non-blocking writer is a black box;
  benchmarking it would test `tracing-appender`, not our code.
- Testing the warn-once branch for `COGNEE_LOG_MAX_BYTES`. A simple
  unit test in `init.rs` (mentioned in task 06-05) suffices.
