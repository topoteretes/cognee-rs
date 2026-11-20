//! Run the enola binary on a repository.
//!
//! Port of the runner half of Python's `cognee/tasks/code_graph/enola.py`
//! (`find_enola_binary`, `_run_enola`, `run_enola_generate`). enola is an
//! external Go CLI; `enola --generate <repo>` writes the snapshot into
//! `<repo>/.enola/`, which [`crate::parse_enola_snapshot`] reads.
//!
//! This is a production subprocess: the child is spawned with
//! `kill_on_drop`, so a run whose future is dropped (a timeout, a cancelled
//! request) never outlives it.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::config::CodeGraphConfig;
use crate::error::{CodeGraphError, CodeGraphResult};
use crate::pyjson::float_repr;

/// The message of an [`CodeGraphError::EnolaNotInstalled`] error. Python's
/// `INSTALL_HINT`.
pub const INSTALL_HINT: &str = "The code graph feature needs the `enola-cli` package, which is \
     included with cognee. Reinstall cognee to restore it.";

/// Environment the generate subprocess gets on top of the parent's.
/// cognee pins the enola release it runs, so enola's own release check (a
/// GitHub API call) is noise and the run must stay offline-safe; prompts and
/// terminal hints assume an interactive shell. Python's
/// `_SUBPROCESS_ENV_OVERRIDES`.
pub const SUBPROCESS_ENV_OVERRIDES: &[(&str, &str)] =
    &[("ENOLA_NO_UPDATE_CHECK", "1"), ("ENOLA_NO_PROMPTS", "1")];

/// The program name looked up on `PATH` when no path is configured.
const ENOLA_PROGRAM: &str = "enola";

/// How much of enola's stderr a failure message keeps (characters).
const STDERR_TAIL_CHARS: usize = 2000;

/// The enola binary to run. Python's `find_enola_binary`.
///
/// `code_graph.enola_path` (Python's `ENOLA_PATH`) wins and must name an
/// existing file. A relative path is made absolute against the process's
/// working directory first, so the file checked is the file spawned: enola
/// runs with the repository as its working directory, which would otherwise
/// resolve a relative program path against the repository. Otherwise the bare program name `enola` is returned and
/// the operating system resolves it on `PATH` when the process is spawned:
/// this crate never reads the environment itself, and a missing binary then
/// surfaces from [`run_enola_generate`] as the same
/// [`CodeGraphError::EnolaNotInstalled`].
///
/// # Errors
///
/// [`CodeGraphError::EnolaNotInstalled`] when the configured path names no
/// file.
pub fn find_enola_binary(config: &CodeGraphConfig) -> CodeGraphResult<PathBuf> {
    let configured = config.enola_path.as_str();
    if !configured.is_empty() {
        // `absolute` fails only for an empty path or an unreadable working
        // directory; the configured text is then checked as given.
        let path = std::path::absolute(configured).unwrap_or_else(|_| PathBuf::from(configured));
        if path.is_file() {
            return Ok(path);
        }
        return Err(CodeGraphError::EnolaNotInstalled(format!(
            "ENOLA_PATH is set to '{configured}' but no file exists there. {INSTALL_HINT}"
        )));
    }
    Ok(PathBuf::from(ENOLA_PROGRAM))
}

/// The last `max_chars` characters of `text` (Python's `text[-max_chars:]`).
fn tail_chars(text: &str, max_chars: usize) -> &str {
    let count = text.chars().count();
    if count <= max_chars {
        return text;
    }
    let start = text
        .char_indices()
        .nth(count - max_chars)
        .map_or(0, |(index, _)| index);
    &text[start..]
}

/// A process exit as Python's `returncode`: the exit code, or minus the
/// signal number for a process a signal ended.
fn return_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -signal;
        }
    }
    -1
}

/// Run one enola command to completion. Python's `_run_enola`.
///
/// `subject` names what was run in error messages. Lines enola writes to
/// stderr starting with `enola:` (the configuration it resolved) are logged.
async fn run_enola(
    binary: &Path,
    args: &[&std::ffi::OsStr],
    cwd: &Path,
    timeout: std::time::Duration,
    subject: &str,
) -> CodeGraphResult<()> {
    tracing::info!(
        "Running enola: {} {} (cwd={})",
        binary.display(),
        args.iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        cwd.display()
    );

    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(cwd)
        .envs(SUBPROCESS_ENV_OVERRIDES.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CodeGraphError::EnolaNotInstalled(INSTALL_HINT.to_string())
        } else {
            CodeGraphError::EnolaSnapshot(format!(
                "Could not start enola ('{}') on {subject}: {error}",
                binary.display()
            ))
        }
    })?;

    // Dropping the `wait_with_output` future on timeout drops the child, and
    // `kill_on_drop` kills it; tokio reaps it in the background.
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(result) => result.map_err(|error| {
            CodeGraphError::EnolaSnapshot(format!("enola failed on {subject}: {error}"))
        })?,
        Err(_elapsed) => {
            return Err(CodeGraphError::EnolaSnapshot(format!(
                "enola timed out after {} seconds on {subject}.",
                float_repr(timeout.as_secs_f64())
            )));
        }
    };

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        // Artifacts already in .enola/ may be from an earlier run; the error
        // propagates before anything parses them.
        return Err(CodeGraphError::EnolaSnapshot(format!(
            "enola exited with code {} on {subject}. stderr tail: {}",
            return_code(output.status),
            tail_chars(&stderr, STDERR_TAIL_CHARS)
        )));
    }

    // enola reports the configuration it resolved on stderr; it decides which
    // extractors run, so it belongs in the log next to the snapshot.
    for line in stderr.lines() {
        if line.starts_with("enola:") {
            tracing::info!("{}", line.trim());
        }
    }
    Ok(())
}

/// Run `enola --generate <repo_path>` and return the snapshot directory
/// (`<repo_path>/.enola`). Python's `run_enola_generate`.
///
/// The repository is both the argument and the working directory: enola
/// resolves an optional `mcp-arch.yaml` from its working directory, so a
/// repo-local config is honoured and the caller's is never picked up. The
/// timeout is `code_graph.enola_timeout_secs`.
///
/// # Errors
///
/// * [`CodeGraphError::EnolaNotInstalled`] when no binary is found.
/// * [`CodeGraphError::EnolaSnapshot`] when `repo_path` is not a directory,
///   enola times out or exits non-zero, or it leaves no `facts.jsonl`.
/// * [`CodeGraphError::InvalidConfig`] for an unusable timeout.
pub async fn run_enola_generate(
    config: &CodeGraphConfig,
    repo_path: impl AsRef<Path>,
) -> CodeGraphResult<PathBuf> {
    let binary = find_enola_binary(config)?;
    let timeout = config.enola_timeout()?;
    // Absolute for the same reason as the binary: it is both the argument
    // and the working directory.
    let repo_path =
        std::path::absolute(repo_path.as_ref()).unwrap_or_else(|_| repo_path.as_ref().into());
    let repo_path = repo_path.as_path();

    if !repo_path.is_dir() {
        return Err(CodeGraphError::EnolaSnapshot(format!(
            "Repository path '{}' is not a directory.",
            repo_path.display()
        )));
    }

    let snapshot_dir = repo_path.join(".enola");
    run_enola(
        &binary,
        &["--generate".as_ref(), repo_path.as_os_str()],
        repo_path,
        timeout,
        &format!("'{}'", repo_path.display()),
    )
    .await?;

    if !snapshot_dir.join("facts.jsonl").is_file() {
        return Err(CodeGraphError::EnolaSnapshot(format!(
            "enola completed but no facts.jsonl was found in '{}'.",
            snapshot_dir.display()
        )));
    }
    Ok(snapshot_dir)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_path_must_exist() {
        let config = CodeGraphConfig {
            enola_path: "/definitely/not/here/enola".to_string(),
            ..CodeGraphConfig::default()
        };
        let error = find_enola_binary(&config).unwrap_err();
        assert_eq!(error.name(), "EnolaNotInstalledError");
        assert_eq!(
            error.to_string(),
            format!(
                "ENOLA_PATH is set to '/definitely/not/here/enola' but no file exists there. \
                 {INSTALL_HINT}"
            )
        );
    }

    #[test]
    fn a_relative_configured_path_is_made_absolute() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("enola"), "").unwrap();
        // A path relative to this process's working directory.
        let cwd = std::env::current_dir().unwrap();
        let relative = pathdiff(dir.path(), &cwd);
        assert!(relative.is_relative());
        let config = CodeGraphConfig {
            enola_path: relative.join("enola").to_string_lossy().into_owned(),
            ..CodeGraphConfig::default()
        };
        let found = find_enola_binary(&config).unwrap();
        assert!(found.is_absolute(), "{}", found.display());
        assert_eq!(
            found.canonicalize().unwrap(),
            dir.path().join("enola").canonicalize().unwrap()
        );
    }

    /// `target` relative to `base` (both absolute): `..` up to the common
    /// prefix, then down.
    fn pathdiff(target: &Path, base: &Path) -> PathBuf {
        let target = target.canonicalize().unwrap();
        let base = base.canonicalize().unwrap();
        let common = target
            .components()
            .zip(base.components())
            .take_while(|(left, right)| left == right)
            .count();
        let mut relative = PathBuf::new();
        for _ in base.components().skip(common) {
            relative.push("..");
        }
        for component in target.components().skip(common) {
            relative.push(component);
        }
        relative
    }

    #[test]
    fn no_configured_path_defers_to_path_lookup() {
        assert_eq!(
            find_enola_binary(&CodeGraphConfig::default()).unwrap(),
            PathBuf::from("enola")
        );
    }

    #[test]
    fn stderr_tail_counts_characters() {
        assert_eq!(tail_chars("abc", 5), "abc");
        assert_eq!(tail_chars("ééééé", 2), "éé");
    }
}
