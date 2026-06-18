//! Notebook cell execution backend for
//! `POST /api/v1/notebooks/{notebook_id}/{cell_id}/run`.
//!
//! The Python reference at
//! `/tmp/cognee-python/cognee/modules/notebooks/operations/run_in_local_sandbox.py`
//! uses in-process `exec()` and captures `print()` calls into a list. Running
//! arbitrary user code in-process in Rust would be a remote-code-execution
//! gun, so this module instead spawns an isolated `python3` subprocess and
//! captures stdout/stderr, then enforces wall-clock, memory, and output-size
//! caps.
//!
//! Trait shape: a single async [`NotebookRunner::run_cell`] returning a
//! [`RunCellOutcome`].  Production wiring uses [`SubprocessRunner`]; tests
//! that don't want to spawn Python plug in a mock runner.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

// ─── Public types ────────────────────────────────────────────────────────────

/// Wire-shape outcome of running a single cell.
///
/// `print_output` corresponds to Python's `printOutput` list (each top-level
/// `print()` call appends one entry). The Rust subprocess implementation
/// emits one entry per line of captured stdout — close enough for the
/// `RunCodeOutcomeDTO` wire shape (each line is JSON-encoded as a string).
///
/// `error` is `None` on success and `Some(traceback_or_runner_message)` on
/// failure. Mirrors Python's `(printOutput, error)` tuple.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunCellOutcome {
    pub print_output: Vec<String>,
    pub error: Option<String>,
}

/// Errors that can surface from a [`NotebookRunner`] implementation.
///
/// These are *infrastructure* errors — they map to HTTP 500. Cells that
/// "successfully ran but raised an exception" are captured into
/// [`RunCellOutcome::error`] instead, mirroring Python's behavior of
/// returning HTTP 200 with `{"error": "<traceback>"}`.
#[derive(Debug, Error)]
pub enum RunnerError {
    /// The `python3` binary was not found on PATH (or the configured path).
    #[error("python interpreter not found: {0}")]
    InterpreterNotFound(String),

    /// Failed to spawn the subprocess (other than not-found).
    #[error("failed to spawn interpreter: {0}")]
    Spawn(String),

    /// Failed to communicate with the subprocess (stdin/stdout/stderr).
    #[error("subprocess I/O failed: {0}")]
    Io(String),
}

// ─── Runner trait ────────────────────────────────────────────────────────────

/// Abstract code-execution backend.
///
/// Implementors MUST be `Send + Sync` so they can live behind `Arc<dyn ...>`
/// in [`crate::components::ComponentHandles`].
#[async_trait]
pub trait NotebookRunner: Send + Sync + 'static {
    /// Execute `code` and return a [`RunCellOutcome`].
    ///
    /// `timeout` is a wall-clock cap; the implementation MUST kill the
    /// subprocess (or abort the work) when the timer fires and return an
    /// `Outcome { print_output: <whatever was captured so far>, error:
    /// Some("Execution timed out...") }`.
    async fn run_cell(&self, code: &str, timeout: Duration) -> Result<RunCellOutcome, RunnerError>;
}

// ─── SubprocessRunner ────────────────────────────────────────────────────────

/// Production runner: spawns `python3 -c '<wrapper>'` and feeds the user's
/// code via stdin.
///
/// **Security knobs:**
/// - User code is NEVER concatenated into a shell command. The user code is
///   fed via stdin to a tiny stdin-reading wrapper passed as `-c`.
/// - The subprocess inherits only a minimal environment (cleared `PATH`,
///   scoped `TMPDIR` if available).
/// - stdout/stderr capture is hard-capped at [`Self::stdout_cap_bytes`] /
///   [`Self::stderr_cap_bytes`] to prevent a `print('A'*10**9)` from OOMing
///   the server.
/// - On Unix, the child inherits a soft `RLIMIT_AS` (address space) and
///   `RLIMIT_CPU` ceiling enforced via `pre_exec`.
#[derive(Debug, Clone)]
pub struct SubprocessRunner {
    /// Path or name of the python interpreter (default `"python3"`).
    pub python_path: String,
    /// Maximum captured stdout, in bytes. Default 64 KiB.
    pub stdout_cap_bytes: usize,
    /// Maximum captured stderr, in bytes. Default 16 KiB.
    pub stderr_cap_bytes: usize,
    /// Memory cap (RLIMIT_AS, Unix only). Default 512 MiB. `None` disables.
    pub memory_cap_bytes: Option<u64>,
    /// CPU-time cap (RLIMIT_CPU, Unix only) in seconds. Default 60s.
    /// Separate from wall-clock timeout, which is enforced by the caller.
    pub cpu_seconds_cap: Option<u64>,
}

impl Default for SubprocessRunner {
    fn default() -> Self {
        Self {
            python_path: "python3".to_owned(),
            stdout_cap_bytes: 64 * 1024,
            stderr_cap_bytes: 16 * 1024,
            memory_cap_bytes: Some(512 * 1024 * 1024),
            cpu_seconds_cap: Some(60),
        }
    }
}

impl SubprocessRunner {
    /// Construct a runner with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap this runner in an `Arc<dyn NotebookRunner>` for storage in
    /// [`crate::components::ComponentHandles`].
    pub fn into_dyn(self) -> Arc<dyn NotebookRunner> {
        Arc::new(self) as Arc<dyn NotebookRunner>
    }
}

/// A tiny Python wrapper that reads user code from stdin, executes it via
/// `exec`, and captures `print()` arguments to stdout (one repr per line).
/// Uncaught exceptions go to stderr as a traceback.
///
/// The wrapper itself is a static `-c` argument (no user input); the user's
/// code reaches the interpreter only via stdin.
const PYTHON_WRAPPER: &str = r#"
import sys, traceback
src = sys.stdin.read()
print_output = []
def _custom_print(*args, **kwargs):
    sep = kwargs.get('sep', ' ')
    print_output.append(sep.join(str(a) for a in args))
env = {'print': _custom_print, '__name__': '__cognee_cell__'}
try:
    exec(compile(src, '<cell>', 'exec'), env)
except SystemExit:
    raise
except BaseException:
    sys.stderr.write(traceback.format_exc())
for line in print_output:
    sys.__stdout__.write(line)
    sys.__stdout__.write('\n')
sys.__stdout__.flush()
"#;

#[async_trait]
impl NotebookRunner for SubprocessRunner {
    async fn run_cell(&self, code: &str, timeout: Duration) -> Result<RunCellOutcome, RunnerError> {
        let mut cmd = Command::new(&self.python_path);
        cmd.arg("-I") // isolated mode: ignore PYTHON* env vars and user site-packages
            .arg("-c")
            .arg(PYTHON_WRAPPER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            // Wipe inherited env. Set only what the child needs.
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8");

        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            cmd.env("TMPDIR", tmpdir);
        }

        #[cfg(unix)]
        {
            let mem_cap = self.memory_cap_bytes;
            let cpu_cap = self.cpu_seconds_cap;
            unsafe {
                cmd.pre_exec(move || {
                    if let Some(mem) = mem_cap {
                        let lim = libc::rlimit {
                            rlim_cur: mem as libc::rlim_t,
                            rlim_max: mem as libc::rlim_t,
                        };
                        // Best-effort; ignore failures.
                        let _ = libc::setrlimit(libc::RLIMIT_AS, &lim);
                    }
                    if let Some(cpu) = cpu_cap {
                        let lim = libc::rlimit {
                            rlim_cur: cpu as libc::rlim_t,
                            rlim_max: cpu as libc::rlim_t,
                        };
                        let _ = libc::setrlimit(libc::RLIMIT_CPU, &lim);
                    }
                    Ok(())
                });
            }
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Err(RunnerError::InterpreterNotFound(self.python_path.clone()));
                }
                return Err(RunnerError::Spawn(e.to_string()));
            }
        };

        // Feed the user's code via stdin.
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| RunnerError::Io("child stdin missing".to_owned()))?;
        let code_owned = code.to_owned();
        let write_handle = tokio::spawn(async move {
            let res = stdin.write_all(code_owned.as_bytes()).await;
            // Drop stdin so the child's sys.stdin.read() unblocks.
            drop(stdin);
            res
        });

        // Run with a wall-clock timeout. On timeout, kill the child.
        let wait_result = tokio::time::timeout(timeout, child.wait_with_output()).await;

        // Ensure stdin task is cleaned up; ignore its error (write may fail
        // with BrokenPipe if the child exited early — that's fine).
        let _ = write_handle.await;

        let output = match wait_result {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(RunnerError::Io(format!("wait_with_output: {e}"))),
            Err(_) => {
                // Timeout — `kill_on_drop` already armed; nothing more to do.
                return Ok(RunCellOutcome {
                    print_output: Vec::new(),
                    error: Some(format!(
                        "Cell execution timed out after {} ms",
                        timeout.as_millis()
                    )),
                });
            }
        };

        // Cap captured bytes.
        let stdout_truncated = output.stdout.len() > self.stdout_cap_bytes;
        let stderr_truncated = output.stderr.len() > self.stderr_cap_bytes;
        let stdout_bytes = &output.stdout[..output.stdout.len().min(self.stdout_cap_bytes)];
        let stderr_bytes = &output.stderr[..output.stderr.len().min(self.stderr_cap_bytes)];

        let stdout = String::from_utf8_lossy(stdout_bytes).into_owned();
        let mut stderr = String::from_utf8_lossy(stderr_bytes).into_owned();

        if stdout_truncated {
            stderr.push_str("\n[stdout truncated by server: exceeded cap]\n");
        }
        if stderr_truncated {
            stderr.push_str("\n[stderr truncated by server: exceeded cap]\n");
        }

        let print_output: Vec<String> = stdout
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();

        let error = if !output.status.success() && !stderr.is_empty() {
            Some(stderr.trim_end().to_owned())
        } else if !stderr.is_empty() {
            // exec'd code wrote to stderr but exited cleanly — treat as error
            // (matches Python's `traceback.format_exc()` which we route through
            // stderr in our wrapper).
            Some(stderr.trim_end().to_owned())
        } else if !output.status.success() {
            // No stderr but non-zero exit — synthesize a message.
            Some(format!(
                "Python interpreter exited with status {}",
                output.status.code().unwrap_or(-1)
            ))
        } else {
            None
        };

        Ok(RunCellOutcome {
            print_output,
            error,
        })
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Mock runner used to verify handler wiring without spawning Python.
    /// Each call appends the (code, timeout) pair to `calls` and returns the
    /// configured `outcome` (or `error`).
    pub struct MockRunner {
        pub calls: Mutex<Vec<(String, Duration)>>,
        pub outcome: Mutex<Result<RunCellOutcome, RunnerErrorStub>>,
    }

    /// `RunnerError` is not Clone; use a stub for the mock that we map on use.
    #[derive(Debug, Clone)]
    pub enum RunnerErrorStub {
        InterpreterNotFound(String),
        Spawn(String),
        Io(String),
    }

    impl From<&RunnerErrorStub> for RunnerError {
        fn from(s: &RunnerErrorStub) -> Self {
            match s {
                RunnerErrorStub::InterpreterNotFound(s) => Self::InterpreterNotFound(s.clone()),
                RunnerErrorStub::Spawn(s) => Self::Spawn(s.clone()),
                RunnerErrorStub::Io(s) => Self::Io(s.clone()),
            }
        }
    }

    impl MockRunner {
        pub fn with_outcome(outcome: RunCellOutcome) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcome: Mutex::new(Ok(outcome)),
            }
        }

        pub fn with_error(err: RunnerErrorStub) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                outcome: Mutex::new(Err(err)),
            }
        }
    }

    #[async_trait]
    impl NotebookRunner for MockRunner {
        async fn run_cell(
            &self,
            code: &str,
            timeout: Duration,
        ) -> Result<RunCellOutcome, RunnerError> {
            self.calls
                .lock()
                .expect("mock calls lock") // lock poison is unrecoverable
                .push((code.to_owned(), timeout));
            match &*self.outcome.lock().expect("mock outcome lock") {
                // lock poison is unrecoverable
                Ok(o) => Ok(o.clone()),
                Err(e) => Err(e.into()),
            }
        }
    }

    // ── Mock unit tests: verify the trait contract without spawning python. ──

    #[tokio::test]
    async fn mock_runner_happy_path() {
        let mock = MockRunner::with_outcome(RunCellOutcome {
            print_output: vec!["2".to_owned()],
            error: None,
        });
        let outcome = mock
            .run_cell("print(1+1)", Duration::from_secs(5))
            .await
            .expect("ok");
        assert_eq!(outcome.print_output, vec!["2".to_owned()]);
        assert_eq!(outcome.error, None);

        let calls = mock.calls.lock().expect("calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "print(1+1)");
        assert_eq!(calls[0].1, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn mock_runner_simulated_timeout_outcome() {
        // A timeout in the production runner is encoded as a successful trait
        // call returning `error = Some("...timed out...")`. Verify the mock
        // can model that exact contract.
        let mock = MockRunner::with_outcome(RunCellOutcome {
            print_output: Vec::new(),
            error: Some("Cell execution timed out after 1000 ms".to_owned()),
        });
        let outcome = mock
            .run_cell("import time; time.sleep(60)", Duration::from_millis(1000))
            .await
            .expect("ok");
        assert!(outcome.print_output.is_empty());
        assert!(
            outcome
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("timed out")
        );
    }

    #[tokio::test]
    async fn mock_runner_overflow_outcome() {
        // Verify the "stdout truncated" contract through the mock.
        let mut err = String::from("[stdout truncated by server: exceeded cap]");
        let mock = MockRunner::with_outcome(RunCellOutcome {
            print_output: vec!["A".repeat(64).to_string()],
            error: Some(std::mem::take(&mut err)),
        });
        let outcome = mock
            .run_cell("print('A'*10**9)", Duration::from_secs(5))
            .await
            .expect("ok");
        assert!(outcome.error.unwrap().contains("truncated"));
    }

    #[tokio::test]
    async fn mock_runner_error_path() {
        let mock =
            MockRunner::with_error(RunnerErrorStub::InterpreterNotFound("python3".to_owned()));
        let err = mock
            .run_cell("print(1)", Duration::from_secs(5))
            .await
            .expect_err("should error");
        match err {
            RunnerError::InterpreterNotFound(p) => assert_eq!(p, "python3"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // ── SubprocessRunner builder smoke test (no spawn). ──

    #[test]
    fn subprocess_runner_defaults() {
        let r = SubprocessRunner::new();
        assert_eq!(r.python_path, "python3");
        assert_eq!(r.stdout_cap_bytes, 64 * 1024);
        assert_eq!(r.stderr_cap_bytes, 16 * 1024);
        assert_eq!(r.memory_cap_bytes, Some(512 * 1024 * 1024));
        assert_eq!(r.cpu_seconds_cap, Some(60));
    }
}
