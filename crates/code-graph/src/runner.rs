//! Run the enola binary on a repository.
//!
//! Port of the runner half of Python's `cognee/tasks/code_graph/enola.py`
//! (`find_enola_binary`, `_run_enola`, `run_enola_generate`,
//! `render_cluster_config`, `run_enola_cluster`). enola is an external Go
//! CLI; `enola --generate <repo>` writes the snapshot into `<repo>/.enola/`,
//! which [`crate::parse_enola_snapshot`] reads, and `enola --generate
//! <cluster config>` writes one linked snapshot of several repositories into
//! each of them.
//!
//! This is a production subprocess: the child is spawned with
//! `kill_on_drop`, so a run whose future is dropped (a timeout, a cancelled
//! request) never outlives it.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::CodeGraphConfig;
use crate::error::{CodeGraphError, CodeGraphResult};
use crate::pyjson::{dumps_str, float_repr};

/// Where enola reads a cluster config piped to it. Python's
/// `_STDIN_CONFIG_PATH`.
const STDIN_CONFIG_PATH: &str = "/dev/stdin";

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
/// `stdin_data` is written to the process's standard input (a cluster config
/// read as `/dev/stdin`); without it stdin is not connected.
async fn run_enola(
    binary: &Path,
    args: &[&std::ffi::OsStr],
    cwd: &Path,
    timeout: std::time::Duration,
    subject: &str,
    stdin_data: Option<&[u8]>,
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
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CodeGraphError::EnolaNotInstalled(INSTALL_HINT.to_string())
        } else {
            CodeGraphError::EnolaSnapshot(format!(
                "Could not start enola ('{}') on {subject}: {error}",
                binary.display()
            ))
        }
    })?;

    // The config is written, then stdin closed, while the output is read:
    // enola reads the whole config at start, before it writes anything.
    let stdin = child.stdin.take();
    let feed = async move {
        if let (Some(mut stdin), Some(data)) = (stdin, stdin_data) {
            // A child that exits without reading its input closes the pipe;
            // its exit status then tells what went wrong, not this write.
            if let Err(error) = stdin.write_all(data).await {
                tracing::debug!("writing enola's standard input failed: {error}");
            }
        }
    };
    // Dropping the `wait_with_output` future on timeout drops the child, and
    // `kill_on_drop` kills it; tokio reaps it in the background.
    let run = async {
        let ((), output) = tokio::join!(feed, child.wait_with_output());
        output
    };
    let output = match tokio::time::timeout(timeout, run).await {
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
        None,
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

/// The enola cluster config for these repositories, as YAML text. Python's
/// `render_cluster_config`.
///
/// enola accepts a cluster only as a `repos:` config whose entries are plain
/// path strings. They are written as JSON strings (valid YAML scalars that
/// quote any path safely), in cluster order. `output_dir` replaces `.enola`
/// as the snapshot directory; enola joins it to each repository and refuses
/// an absolute one. enola's own per-repository architecture history is
/// switched off: cognee keeps its own record of the snapshot it loaded.
pub fn render_cluster_config(repo_paths: &[PathBuf], output_dir: &str) -> String {
    let mut lines = vec![
        "output:".to_string(),
        format!("  dir: {}", dumps_str(output_dir)),
        "repos:".to_string(),
    ];
    lines.extend(
        repo_paths
            .iter()
            .map(|path| format!("  - {}", dumps_str(&path.to_string_lossy()))),
    );
    lines.extend(["history:".to_string(), "  enabled: false".to_string()]);
    lines.join("\n") + "\n"
}

/// Whether enola can read the cluster config from `/dev/stdin` here.
/// Windows has none, so the config goes through a temporary file there.
/// Python's `_config_via_stdin`.
fn config_via_stdin() -> bool {
    !cfg!(windows)
}

/// Run `enola --generate` over a cluster of repositories and return one
/// snapshot directory. Python's `run_enola_cluster`.
///
/// enola links the repositories of a cluster config into one graph and
/// writes that whole graph to every member's `output_dir`, so any member's
/// directory is the cluster's snapshot: the first member's is returned. The
/// config ([`render_cluster_config`], over the members' absolute paths) is
/// piped to enola on its standard input (`--generate /dev/stdin`), so nothing
/// is written to disk for it; on Windows it is a temporary file removed as
/// soon as enola exits. The working directory is the system temporary
/// directory, so no member's own `mcp-arch.yaml` narrows a run covering all
/// of them. `timeout` is the whole run's budget.
///
/// # Errors
///
/// * [`CodeGraphError::EnolaNotInstalled`] when no binary is found.
/// * [`CodeGraphError::EnolaSnapshot`] when there is no member, a member is
///   not a directory (before anything runs), enola times out or exits
///   non-zero, or it leaves no `facts.jsonl` in the first member's
///   `output_dir`.
pub async fn run_enola_cluster(
    config: &CodeGraphConfig,
    repo_paths: &[PathBuf],
    output_dir: &str,
    timeout: std::time::Duration,
) -> CodeGraphResult<PathBuf> {
    let binary = find_enola_binary(config)?;
    let repo_paths: Vec<PathBuf> = repo_paths
        .iter()
        .map(|path| std::path::absolute(path).unwrap_or_else(|_| path.clone()))
        .collect();
    let Some(first) = repo_paths.first() else {
        return Err(CodeGraphError::EnolaSnapshot(
            "An enola cluster needs at least one repository.".to_string(),
        ));
    };
    for repo_path in &repo_paths {
        if !repo_path.is_dir() {
            return Err(CodeGraphError::EnolaSnapshot(format!(
                "Repository path '{}' is not a directory.",
                repo_path.display()
            )));
        }
    }

    let config_text = render_cluster_config(&repo_paths, output_dir);
    let names: Vec<String> = repo_paths
        .iter()
        .map(|path| {
            path.file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
        })
        .collect();
    let subject = format!(
        "the {}-repository cluster [{}]",
        repo_paths.len(),
        names
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let cwd = std::env::temp_dir();

    if config_via_stdin() {
        run_enola(
            &binary,
            &["--generate".as_ref(), STDIN_CONFIG_PATH.as_ref()],
            &cwd,
            timeout,
            &subject,
            Some(config_text.as_bytes()),
        )
        .await?;
    } else {
        let config_path = cwd.join(format!(
            "cognee-enola-cluster-{}.yaml",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&config_path, &config_text).map_err(|error| {
            CodeGraphError::EnolaSnapshot(format!(
                "Could not write the enola cluster config '{}': {error}",
                config_path.display()
            ))
        })?;
        let ran = run_enola(
            &binary,
            &["--generate".as_ref(), config_path.as_os_str()],
            &cwd,
            timeout,
            &subject,
            None,
        )
        .await;
        // Removed whatever the run did; a leftover file is only clutter.
        let _ = std::fs::remove_file(&config_path);
        ran?;
    }

    let snapshot_dir = first.join(output_dir);
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
