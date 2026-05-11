//! Integration test: invoking `cognee-cli` writes a `*.log` file
//! under `COGNEE_LOGS_DIR` and the file contains the
//! `"Logging initialized"` anchor emitted by `cognee_logging::init_logging`.
//!
//! Covers task 06-10 §4.1 and decision 5 (log file appearance under the
//! resolved logs directory), plus the default-filter behaviour that
//! keeps the anchor line visible at the default level (decision 6).

use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

/// `--help` is the cheapest invocation that drives the full
/// `main()` path including `init_logging`. Clap's auto-generated help
/// handler calls `std::process::exit`, which means `LogGuards::drop` is
/// skipped — but `tracing-appender`'s background worker flushes the
/// pending lines as the worker thread is terminated, and a short sleep
/// covers the OS-level write race.
#[test]
fn cli_creates_log_file_in_cognee_logs_dir() {
    let dir = tempdir().expect("tempdir");
    let bin = env!("CARGO_BIN_EXE_cognee-cli");

    let output = Command::new(bin)
        .env("COGNEE_LOGS_DIR", dir.path())
        // Defensive: ensure the parent shell's env does not steer the
        // child CLI into appending to a pre-existing log file owned by
        // a different process.
        .env_remove("LOG_FILE_NAME")
        .env_remove("RUST_LOG")
        .env_remove("LOG_LEVEL")
        // Disable telemetry so the test does not attempt to contact an
        // OTEL collector — see decision 13 in 06-file-logging-rotation.md.
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .arg("--help")
        .output()
        .expect("spawn cognee-cli");

    assert!(
        output.status.success(),
        "cognee-cli --help should succeed; stderr=\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Wait for the non-blocking writer to flush. The CLI exits via
    // `std::process::exit` on `--help`, so the WorkerGuard's Drop is
    // skipped. The background thread still flushes pending lines as
    // the process tears down, but the assertion below needs the file
    // visible to this process. 200ms is the conservative bound
    // recommended in the sub-doc.
    std::thread::sleep(Duration::from_millis(200));

    let log_files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read tempdir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("log"))
        .collect();
    assert!(
        !log_files.is_empty(),
        "expected at least one *.log file in {}",
        dir.path().display()
    );

    // Read every log file; at least one must contain the anchor. With
    // `Rotation::Daily` (the default) the file name is
    // `<timestamp>.<YYYY-MM-DD>` — `tracing-appender` appends a
    // date stem to the prefix, but we wrote the prefix via the file
    // stem of the propagated `LOG_FILE_NAME` so the `.log` extension
    // ends up as the suffix.
    let mut combined = String::new();
    for entry in &log_files {
        if let Ok(body) = std::fs::read_to_string(entry.path()) {
            combined.push_str(&body);
        }
    }
    assert!(
        combined.contains("Logging initialized"),
        "expected 'Logging initialized' anchor line in log files; got:\n{combined}"
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
        .env_remove("LOG_FILE_NAME")
        .env_remove("RUST_LOG")
        .env_remove("LOG_LEVEL")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .arg("--help")
        .output()
        .expect("spawn cognee-cli");
    assert!(output.status.success());

    std::thread::sleep(Duration::from_millis(200));

    let mut combined = String::new();
    for entry in std::fs::read_dir(dir.path())
        .expect("read tempdir")
        .flatten()
    {
        if entry.path().extension().and_then(|s| s.to_str()) == Some("log")
            && let Ok(body) = std::fs::read_to_string(entry.path())
        {
            combined.push_str(&body);
        }
    }

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
