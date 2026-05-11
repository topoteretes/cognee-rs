//! Multi-process `LOG_FILE_NAME` inheritance test (decision 5).
//!
//! When a parent calls `init_logging` and then spawns child processes
//! with the inherited environment, every child must reuse the same
//! `LOG_FILE_NAME` so all output streams into a single file. Without
//! this contract, multi-process deployments (CLI → spawned worker,
//! Python binding spawning a Rust helper, …) would produce per-PID
//! files and break log discoverability.
//!
//! The test:
//! 1. Cleans the relevant env vars.
//! 2. Picks an isolated `COGNEE_LOGS_DIR` tempdir.
//! 3. Calls the parent's `init_logging`, capturing the propagated
//!    `LOG_FILE_NAME`.
//! 4. Spawns the `logging_child_smoke` helper binary twice; each
//!    inherits `LOG_FILE_NAME` and re-runs `init_logging`.
//! 5. Asserts the parent file on disk contains the child line at
//!    least twice (one per spawn).
//!
//! `LOG_FILE_NAME` is process-global env state, so this test must be
//! `#[serial_test::serial]` to avoid clobbering parallel tests that
//! also touch the logging env surface.

use std::ffi::OsString;
use std::process::Command;
use std::time::Duration;

use serial_test::serial;
use tempfile::tempdir;

const TRACKED_VARS: &[&str] = &[
    "COGNEE_LOG_FILE",
    "COGNEE_LOGS_DIR",
    "LOG_FILE_NAME",
    "COGNEE_LOG_ROTATION",
    "COGNEE_LOG_FORMAT",
    "COGNEE_LOG_BACKUP_COUNT",
    "COGNEE_LOG_MAX_FILES",
    "RUST_LOG",
    "LOG_LEVEL",
];

struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new() -> Self {
        let saved: Vec<_> = TRACKED_VARS
            .iter()
            .map(|n| (*n, std::env::var_os(n)))
            .collect();
        for n in TRACKED_VARS {
            // SAFETY: serial test owns env state for its duration.
            unsafe {
                std::env::remove_var(n);
            }
        }
        Self { saved }
    }

    fn set(&self, name: &'static str, value: &str) {
        assert!(TRACKED_VARS.contains(&name), "untracked env var {name}");
        // SAFETY: see `new`.
        unsafe {
            std::env::set_var(name, value);
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (n, v) in &self.saved {
            // SAFETY: see `new`.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(n, v),
                    None => std::env::remove_var(n),
                }
            }
        }
    }
}

#[test]
#[serial]
fn children_inherit_log_file_name_from_parent() {
    let dir = tempdir().expect("tempdir creates");
    let guard = EnvGuard::new();
    guard.set(
        "COGNEE_LOGS_DIR",
        dir.path().to_str().expect("utf-8 tmp path"),
    );
    // Force NEVER rotation so the resolved filename does not include a
    // date stem suffix — keeps the read-back path stable across the
    // 200ms test window.
    guard.set("COGNEE_LOG_ROTATION", "never");

    // Parent init: propagates a fresh `LOG_FILE_NAME` into the env.
    let cfg = cognee_logging::LoggingConfig::from_env().expect("parent config parses");
    let parent_guards =
        cognee_logging::init_logging(cfg, std::iter::empty::<cognee_logging::BoxedLayer>());
    tracing::info!("parent emitted");

    let parent_filename = std::env::var("LOG_FILE_NAME")
        .expect("init_logging must propagate LOG_FILE_NAME for child inheritance");

    // Spawn two children with the inherited env. `Command::new` copies
    // the current env by default, including the LOG_FILE_NAME the
    // parent just wrote — that is the exact inheritance path decision 5
    // requires.
    let child_bin = env!("CARGO_BIN_EXE_logging_child_smoke");
    for i in 0..2 {
        let status = Command::new(child_bin)
            .status()
            .unwrap_or_else(|err| panic!("spawn child #{i}: {err}"));
        assert!(status.success(), "child #{i} exited non-zero: {status:?}");
    }

    // Flush the parent's non-blocking worker before reading the file.
    drop(parent_guards);
    std::thread::sleep(Duration::from_millis(300));

    // The parent should observe all three (parent + 2 children) lines
    // in the single inherited file. `tracing-appender` with
    // `Rotation::NEVER` writes to `<prefix>.log`; the file_stem of
    // `parent_filename` is the `<prefix>`. The actual file on disk
    // therefore lives next to `parent_filename` but with a `.log`
    // suffix appended after the prefix.
    let prefix_path = std::path::PathBuf::from(&parent_filename);
    let prefix_stem = prefix_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cognee");
    let dir_path = prefix_path.parent().unwrap_or_else(|| dir.path());

    // Read every `*.log` file in the dir and union their bodies. With
    // NEVER rotation a single file exists per process tree, but the
    // safety margin guards against any future change in how
    // `tracing-appender` names a `Rotation::NEVER` target.
    let mut combined = String::new();
    let mut matched_file: Option<std::path::PathBuf> = None;
    for entry in std::fs::read_dir(dir_path)
        .expect("read logs dir")
        .flatten()
    {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("log") {
            continue;
        }
        // Filter to files whose stem starts with the propagated
        // prefix so we don't accidentally count a leftover from a
        // previous serial test that escaped cleanup.
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if !stem.starts_with(prefix_stem) {
            continue;
        }
        if let Ok(body) = std::fs::read_to_string(&p) {
            combined.push_str(&body);
            matched_file.get_or_insert(p);
        }
    }

    assert!(
        matched_file.is_some(),
        "expected a `*.log` file matching prefix `{prefix_stem}` in `{}`, found none",
        dir_path.display()
    );

    assert!(
        combined.contains("parent emitted"),
        "parent line missing from `{}`; body:\n{combined}",
        matched_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    let child_count = combined.matches("child emitted").count();
    assert!(
        child_count >= 2,
        "expected at least 2 `child emitted` lines (one per spawn), found {child_count}; body:\n{combined}"
    );
}
