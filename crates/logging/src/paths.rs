//! File-system helpers for cognee logging.
//!
//! Three pure file-system helpers used by `init_logging` (task 06-05):
//!
//! 1. [`resolve_logs_dir`] picks a writable directory for log files,
//!    matching the Python priority list and falling back to
//!    `/tmp/cognee_logs` on edge devices where `$HOME` is read-only.
//! 2. [`propagate_log_file_name`] generates a timestamped filename
//!    on first call, stashes it in `LOG_FILE_NAME`, and returns the
//!    same path on every subsequent call (or inherited child process)
//!    so multiple processes append to one file (decision 5).
//! 3. [`cleanup_old_logs`] deletes `*.log` files in the logs
//!    directory beyond `max_files`, oldest first. Runs once at
//!    startup (decision 11).
//!
//! None of these touch `tracing::subscriber`; tests can exercise them
//! without installing a global subscriber.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Resolve the directory for log files, with fallback.
///
/// Priority:
/// 1. `cfg.logs_dir_override` (set from `COGNEE_LOGS_DIR`).
/// 2. `~/.cognee/logs` (default).
/// 3. `/tmp/cognee_logs` (last-resort fallback).
/// 4. `None` — file logging is silently skipped.
///
/// Each candidate is `mkdir -p`'d and tested with a write probe
/// (create-and-delete `.cognee_write_probe`). The probe is more
/// reliable than `unix::PermissionsExt` on Android's
/// `/data/local/tmp`, which has unusual perm bits.
///
/// The caller (`init_logging`) is responsible for warning the user
/// when all candidates fail; this function stays free of subscriber
/// interaction so tests can run before any subscriber is installed.
///
/// Reference: `cognee/shared/logging_utils.py:103-132`.
pub fn resolve_logs_dir(cfg: &crate::LoggingConfig) -> Option<PathBuf> {
    // 1. Explicit override (COGNEE_LOGS_DIR).
    if let Some(ref override_dir) = cfg.logs_dir_override
        && let Some(p) = try_candidate(override_dir)
    {
        return Some(p);
    }

    // 2. ~/.cognee/logs.
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".cognee").join("logs");
        if let Some(p) = try_candidate(&candidate) {
            return Some(p);
        }
    }

    // 3. /tmp/cognee_logs.
    let tmp_candidate = PathBuf::from("/tmp").join("cognee_logs");
    if let Some(p) = try_candidate(&tmp_candidate) {
        return Some(p);
    }

    None
}

/// Try a single candidate directory: `mkdir -p` then a write probe.
/// Returns `Some(dir)` on success, `None` on any failure.
fn try_candidate(dir: &Path) -> Option<PathBuf> {
    if fs::create_dir_all(dir).is_err() {
        return None;
    }
    let probe = dir.join(".cognee_write_probe");
    match fs::File::create(&probe) {
        Ok(_) => {
            // Best-effort cleanup; ignore errors.
            let _ = fs::remove_file(&probe);
            Some(dir.to_path_buf())
        }
        Err(_) => None,
    }
}

/// Resolve the active log file path for this process.
///
/// On first call within a process: generates
/// `<dir>/<YYYY-MM-DD_HH-MM-SS>.log`, writes the absolute path to
/// `LOG_FILE_NAME` via [`std::env::set_var`], and returns it. On
/// subsequent calls (and in child processes that inherit
/// `LOG_FILE_NAME`), reads the env var back and returns the stored
/// path — making the function idempotent within and across
/// processes.
///
/// # Multi-process note
///
/// Child processes that inherit `LOG_FILE_NAME` from the parent will
/// return the parent's filename on first call, so all processes
/// append to a single file. There is **no** rotation lock —
/// concurrent rotation by `tracing-appender` from multiple processes
/// may corrupt files. This is intentional Python parity (see
/// `docs/telemetry/06-file-logging-rotation.md` "Multi-Process
/// Coordination").
///
/// Reference: `cognee/shared/logging_utils.py:511-519`.
pub fn propagate_log_file_name(dir: &Path) -> PathBuf {
    // Idempotent: if LOG_FILE_NAME is already set (this process or a
    // parent), reuse it verbatim.
    if let Ok(existing) = std::env::var("LOG_FILE_NAME")
        && !existing.is_empty()
    {
        return PathBuf::from(existing);
    }

    // Generate a fresh timestamped filename matching Python's
    // `%Y-%m-%d_%H-%M-%S` strftime format.
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut path = dir.join(format!("{stamp}.log"));

    // Touch the file so canonicalize() can resolve it. Failure is
    // not fatal — we still publish the non-canonical path so the
    // appender has a target.
    let _ = fs::OpenOptions::new().create(true).append(true).open(&path);

    // Canonicalize to an absolute path so child processes inheriting
    // LOG_FILE_NAME do not need to know the parent's CWD.
    if let Ok(canon) = fs::canonicalize(&path) {
        path = canon;
    }

    // Safety: env mutation is process-global. Callers that race two
    // `init_logging` invocations across threads must serialize, but
    // the runbook places this call exactly once at startup.
    // SAFETY: `std::env::set_var` is `unsafe` on edition 2024+; we
    // accept the documented multi-threaded risk here since
    // `init_logging` runs before any worker threads spawn.
    unsafe {
        std::env::set_var("LOG_FILE_NAME", &path);
    }

    path
}

/// Delete `*.log` files in `dir` beyond `max_files`, oldest first.
///
/// Errors are logged at `warn!` and swallowed; never propagated.
/// Called exactly once at startup from `init_logging` (decision 11).
///
/// Reference: `cognee/shared/logging_utils.py:271-308`.
pub fn cleanup_old_logs(dir: &Path, max_files: usize) {
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(err) => {
            tracing::warn!(?dir, ?err, "failed to read logs directory for cleanup");
            return;
        }
    };

    // Collect every `*.log` file together with its mtime.
    let mut log_files: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension() != Some(OsStr::new("log")) {
            continue;
        }
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(err) => {
                tracing::warn!(?path, ?err, "failed to stat log file; skipping in cleanup");
                continue;
            }
        };
        log_files.push((path, mtime));
    }

    // Sort by mtime descending — newest first.
    log_files.sort_by_key(|b| std::cmp::Reverse(b.1));

    if log_files.len() <= max_files {
        return;
    }

    for (path, _) in log_files.into_iter().skip(max_files) {
        if let Err(err) = fs::remove_file(&path) {
            tracing::warn!(?path, ?err, "failed to delete old log file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LogFormat, LogRotation, LoggingConfig};
    use std::time::Duration;
    use tempfile::tempdir;

    fn cfg_with_override(dir: Option<PathBuf>) -> LoggingConfig {
        LoggingConfig {
            file_enabled: true,
            logs_dir_override: dir,
            log_file_name: None,
            rotation: LogRotation::Daily,
            format: LogFormat::Plain,
            backup_count: 5,
            max_files: 10,
            level_filter: None,
        }
    }

    #[test]
    fn resolve_logs_dir_uses_override() {
        let tmp = tempdir().expect("tempdir creates");
        let sub = tmp.path().join("custom_logs");
        let cfg = cfg_with_override(Some(sub.clone()));

        let resolved = resolve_logs_dir(&cfg).expect("override resolves");
        assert_eq!(resolved, sub);
        assert!(sub.is_dir(), "override dir is created (mkdir -p semantics)");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_logs_dir_falls_back_to_tmp() {
        // /proc on Linux is virtual; attempting to create a directory
        // there fails with EACCES/ENOENT, so the override is rejected
        // and we fall back through ~/.cognee/logs to /tmp/cognee_logs.
        // The home candidate may succeed on a developer machine, so
        // we accept either ~/.cognee/logs or /tmp/cognee_logs as
        // valid fallbacks — both are post-override.
        let bogus = PathBuf::from("/proc/cognee-logs-does-not-exist");
        let cfg = cfg_with_override(Some(bogus));

        let resolved = resolve_logs_dir(&cfg).expect("fallback resolves");
        assert_ne!(
            resolved,
            PathBuf::from("/proc/cognee-logs-does-not-exist"),
            "override was correctly rejected"
        );
        assert!(
            resolved.is_dir(),
            "fallback candidate exists: {}",
            resolved.display()
        );
    }

    #[test]
    #[serial_test::serial]
    fn propagate_log_file_name_generates_timestamped_when_unset() {
        // SAFETY: serial test owns LOG_FILE_NAME for its duration.
        unsafe {
            std::env::remove_var("LOG_FILE_NAME");
        }
        let tmp = tempdir().expect("tempdir creates");

        let path = propagate_log_file_name(tmp.path());

        let file_name = path
            .file_name()
            .and_then(|f| f.to_str())
            .expect("file name is utf-8");
        // chrono `%Y-%m-%d_%H-%M-%S` always produces zero-padded
        // fixed-width fields, so a length check + regex-like manual
        // scan is enough without pulling `regex`.
        assert_eq!(
            file_name.len(),
            "YYYY-MM-DD_HH-MM-SS.log".len(),
            "file_name = {file_name}",
        );
        assert!(file_name.ends_with(".log"));
        let stem = &file_name[..file_name.len() - ".log".len()];
        let bytes = stem.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            let expect_sep = matches!(i, 4 | 7 | 10 | 13 | 16);
            let expect_underscore = i == 10;
            if expect_underscore {
                assert_eq!(*b, b'_', "underscore at {i} in {stem}");
            } else if expect_sep {
                assert_eq!(*b, b'-', "dash at {i} in {stem}");
            } else {
                assert!(
                    b.is_ascii_digit(),
                    "digit at {i} in {stem}, got {}",
                    *b as char
                );
            }
        }

        let env_value = std::env::var("LOG_FILE_NAME").expect("env var set");
        assert_eq!(PathBuf::from(env_value), path);

        // Cleanup so other serial tests start clean.
        // SAFETY: still inside the serial section.
        unsafe {
            std::env::remove_var("LOG_FILE_NAME");
        }
    }

    #[test]
    #[serial_test::serial]
    fn propagate_log_file_name_is_idempotent_when_set() {
        let preset = PathBuf::from("/tmp/cognee-test-preset.log");
        // SAFETY: serial test owns LOG_FILE_NAME for its duration.
        unsafe {
            std::env::set_var("LOG_FILE_NAME", &preset);
        }
        let tmp = tempdir().expect("tempdir creates");

        let path = propagate_log_file_name(tmp.path());
        assert_eq!(path, preset);
        assert_eq!(
            std::env::var("LOG_FILE_NAME").expect("env var preserved"),
            preset.to_string_lossy()
        );

        // SAFETY: still inside the serial section.
        unsafe {
            std::env::remove_var("LOG_FILE_NAME");
        }
    }

    #[test]
    fn cleanup_old_logs_keeps_n_newest() {
        let tmp = tempdir().expect("tempdir creates");

        // Create 15 log files with increasing mtimes via sleep
        // (filetime is not a workspace dep; sleep-between-creates
        // is the documented fallback in the sub-doc).
        let mut paths = Vec::new();
        for i in 0..15 {
            let p = tmp.path().join(format!("log_{i:02}.log"));
            fs::File::create(&p).expect("create log file");
            paths.push(p);
            // 10ms gap is enough on every filesystem we target;
            // mtimes always differ.
            std::thread::sleep(Duration::from_millis(10));
        }

        cleanup_old_logs(tmp.path(), 10);

        // The 10 most recent are paths[5..15]; the 5 oldest
        // (paths[0..5]) should be gone.
        let mut remaining: Vec<_> = fs::read_dir(tmp.path())
            .expect("read tempdir")
            .flatten()
            .map(|e| e.path())
            .collect();
        remaining.sort();
        assert_eq!(remaining.len(), 10, "ten newest remain: {remaining:?}");
        for p in &paths[..5] {
            assert!(!p.exists(), "oldest file {} was deleted", p.display());
        }
        for p in &paths[5..] {
            assert!(p.exists(), "newer file {} survives", p.display());
        }
    }

    #[test]
    fn cleanup_old_logs_ignores_non_log_files() {
        let tmp = tempdir().expect("tempdir creates");

        let mut log_paths = Vec::new();
        for i in 0..12 {
            let p = tmp.path().join(format!("log_{i:02}.log"));
            fs::File::create(&p).expect("create log");
            log_paths.push(p);
            std::thread::sleep(Duration::from_millis(10));
        }

        let mut txt_paths = Vec::new();
        for i in 0..3 {
            let p = tmp.path().join(format!("note_{i}.txt"));
            fs::File::create(&p).expect("create txt");
            txt_paths.push(p);
        }

        cleanup_old_logs(tmp.path(), 10);

        let log_count = fs::read_dir(tmp.path())
            .expect("read tempdir")
            .flatten()
            .filter(|e| e.path().extension() == Some(OsStr::new("log")))
            .count();
        let txt_count = fs::read_dir(tmp.path())
            .expect("read tempdir")
            .flatten()
            .filter(|e| e.path().extension() == Some(OsStr::new("txt")))
            .count();

        assert_eq!(log_count, 10, "exactly 10 *.log files remain");
        assert_eq!(txt_count, 3, "all 3 *.txt files survive");
    }

    #[test]
    fn cleanup_old_logs_below_threshold_is_noop() {
        let tmp = tempdir().expect("tempdir creates");
        for i in 0..3 {
            let p = tmp.path().join(format!("log_{i}.log"));
            fs::File::create(&p).expect("create log");
        }

        cleanup_old_logs(tmp.path(), 10);

        let count = fs::read_dir(tmp.path())
            .expect("read tempdir")
            .flatten()
            .count();
        assert_eq!(count, 3, "no files removed below threshold");
    }
}
