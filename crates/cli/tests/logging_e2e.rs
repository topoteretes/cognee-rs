#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test: invoking `cognee-cli` writes a `*.log` file
//! under `COGNEE_LOGS_DIR` and the file contains the
//! `"Logging initialized"` anchor emitted by `cognee_logging::init_logging`.
//!
//! Covers task 06-10 §4.1 and decision 5 (log file appearance under the
//! resolved logs directory), plus the default-filter behaviour that
//! keeps the anchor line visible at the default level (decision 6).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Concatenate the contents of every `*.log` file under `dir`, polling until
/// `pred` is satisfied or `timeout` elapses.
///
/// The callers invoke `cognee-cli config get …`, which returns through `main`'s
/// `ExitCode` path and drops `LogGuards`, flushing the log deterministically —
/// so the anchor is already on disk when `.output()` returns. The short poll
/// here just absorbs filesystem-visibility latency on a loaded runner; it is no
/// longer covering a flush race (the earlier `--help` invocation skipped the
/// guard drop via `std::process::exit`, which made the flush unreliable). The
/// directory is a per-test tempdir, so this only observes this run's output.
fn read_logs_until(dir: &Path, timeout: Duration, pred: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let mut combined = String::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|s| s.to_str()) == Some("log")
                    && let Ok(body) = std::fs::read_to_string(entry.path())
                {
                    combined.push_str(&body);
                }
            }
        }
        if pred(&combined) || Instant::now() >= deadline {
            return combined;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `config get` is a cheap real subcommand that drives the full `main()` path
/// (including `init_logging`) and returns via `ExitCode`, so `LogGuards` drop
/// and flush the log deterministically before the process exits — unlike
/// `--help`, whose clap handler calls `std::process::exit` and skips the flush.
#[test]
fn cli_creates_log_file_in_cognee_logs_dir() {
    let dir = tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_cognee-cli");

    let output = Command::new(bin)
        .env("COGNEE_LOGS_DIR", dir.path())
        // Isolate the config dir too (absolute temp path) so `config get` reads
        // a clean default config and never touches a real user config.
        .env("COGNEE_CONFIG_HOME", dir.path())
        // Defensive: ensure the parent shell's env does not steer the
        // child CLI into appending to a pre-existing log file owned by
        // a different process.
        .env_remove("LOG_FILE_NAME")
        .env_remove("RUST_LOG")
        .env_remove("LOG_LEVEL")
        // Disable telemetry so the test does not attempt to contact an
        // OTEL collector — see decision 13 in 06-file-logging-rotation.md.
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        // Use a real subcommand (not `--help`): clap's `--help` calls
        // `std::process::exit` internally, which skips `main`'s `ExitCode`
        // return and therefore the `LogGuards` drop that flushes the log — so
        // the anchor only ever landed via an unreliable teardown race. `config
        // get` returns through `main()`, dropping the guards and flushing
        // deterministically, so the log is fully written by the time `.output()`
        // returns. `config get` is read-only and exits 0 on a valid key.
        .args(["config", "get", "default_user_id"])
        .output()
        .expect("spawn cognee-cli");

    assert!(
        output.status.success(),
        "cognee-cli config get should succeed; stderr=\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Poll for the non-blocking writer to flush the anchor line. With
    // `Rotation::Daily` (the default) the file name is
    // `<timestamp>.<YYYY-MM-DD>` — `tracing-appender` appends a date stem to
    // the prefix, but we wrote the prefix via the file stem of the propagated
    // `LOG_FILE_NAME` so the `.log` extension ends up as the suffix.
    let combined = read_logs_until(dir.path(), Duration::from_secs(30), |s| {
        s.contains("Logging initialized")
    });
    assert!(
        combined.contains("Logging initialized"),
        "expected 'Logging initialized' anchor line in a *.log file under {}; got:\n{combined}",
        dir.path().display()
    );
}

/// Default filter (decision 6) keeps `cognee_*` at `info` while
/// dropping verbose `hyper`/`reqwest`/etc. noise. We assert the
/// positive half — the anchor `info!` is preserved — directly above.
/// This second test asserts the negative half: with `RUST_LOG` unset,
/// the file does not contain `hyper=info` or `h2=info` style noise.
#[test]
fn cli_default_filter_suppresses_library_noise_in_log_file() {
    let dir = tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_cognee-cli");

    let output = Command::new(bin)
        .env("COGNEE_LOGS_DIR", dir.path())
        // Isolate the config dir too (absolute temp path) so `config get` reads
        // a clean default config and never touches a real user config.
        .env("COGNEE_CONFIG_HOME", dir.path())
        .env_remove("LOG_FILE_NAME")
        .env_remove("RUST_LOG")
        .env_remove("LOG_LEVEL")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        // Use a real subcommand (not `--help`): clap's `--help` calls
        // `std::process::exit` internally, which skips `main`'s `ExitCode`
        // return and therefore the `LogGuards` drop that flushes the log — so
        // the anchor only ever landed via an unreliable teardown race. `config
        // get` returns through `main()`, dropping the guards and flushing
        // deterministically, so the log is fully written by the time `.output()`
        // returns. `config get` is read-only and exits 0 on a valid key.
        .args(["config", "get", "default_user_id"])
        .output()
        .expect("spawn cognee-cli");
    assert!(output.status.success());

    // Poll until logging has flushed (anchor present), then assert the
    // suppressed targets are absent. Anchoring the wait on the anchor line
    // avoids reading a half-flushed file under parallel CI load.
    let combined = read_logs_until(dir.path(), Duration::from_secs(30), |s| {
        s.contains("Logging initialized")
    });

    // `--help` should not produce any HTTP/hyper traffic, so the test
    // only checks that the *target* prefixes for the suppressed crates
    // do not show up at INFO. Anchored on the `[<target>]` bracket the
    // PythonPlainFormatter writes at the end of each line.
    let forbidden = ["[hyper]", "[h2]", "[reqwest]", "[rustls]"];
    for needle in &forbidden {
        assert!(
            !combined.contains(needle),
            "default filter should suppress {needle} at INFO; got log body:\n{combined}"
        );
    }
}
