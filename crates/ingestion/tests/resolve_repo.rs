//! `resolve_repo` (SDK-1053 O7): ports of Python's `test_resolve_repo.py`,
//! `test_resolve_repo_branch.py`, `test_resolve_repo_credentials.py` and
//! `test_resolve_repo_real_git.py` (parity pin `a59433e6b`).
//!
//! The first three replace git with a recording transport, as Python patches
//! `_run_git`. The real-git cases run `git` against a bare repository on disk
//! that stands in for the remote (a `file://` URL, so the clone is shallow
//! like a real one); nothing touches the network. They skip when git is not
//! installed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use cognee_ingestion::resolve_repo::{
    CodeRepositoryError, GitOutput, RepoSourceSettings, RepoTransport, SystemGit, clone_dir,
    credential_env, resolve_repo_source,
};
use tempfile::TempDir;

const URL: &str = "https://github.com/org/repo";
const TOKEN_URL: &str = "https://x-access-token:tok-SECRET@github.com/org/repo";
const TOKEN_GIT_URL: &str = "https://x-access-token:tok-SECRET@github.com/org/repo.git";
const CLEAN_GIT_URL: &str = "https://github.com/org/repo.git";

type Call = (Vec<String>, Option<PathBuf>, Vec<(String, String)>);

/// Records every git command and answers it with `answer`; the outbound
/// check passes unless `outbound_refusal` is set.
struct Recording {
    calls: Mutex<Vec<Call>>,
    answer: fn(&[String]) -> GitOutput,
    outbound_checks: Mutex<Vec<String>>,
}

fn ok(_: &[String]) -> GitOutput {
    GitOutput {
        status: 0,
        stderr: String::new(),
    }
}

impl Recording {
    fn new(answer: fn(&[String]) -> GitOutput) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            answer,
            outbound_checks: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn args(&self) -> Vec<Vec<String>> {
        self.calls().into_iter().map(|(args, _, _)| args).collect()
    }
}

#[async_trait::async_trait]
impl RepoTransport for Recording {
    async fn run_git(
        &self,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
    ) -> Result<GitOutput, CodeRepositoryError> {
        self.calls
            .lock()
            .unwrap()
            .push((args.to_vec(), cwd.map(Path::to_path_buf), env.to_vec()));
        Ok((self.answer)(args))
    }

    async fn check_outbound_url(&self, url: &str) -> Result<(), String> {
        self.outbound_checks.lock().unwrap().push(url.to_string());
        Ok(())
    }
}

fn sdk(clones: &Path) -> RepoSourceSettings {
    RepoSourceSettings::new(&clones.to_string_lossy(), "", false)
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn existing_clone(clones: &Path, name: &str) -> PathBuf {
    let dir = clones.join(name);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    dir
}

// ─── test_resolve_repo.py ─────────────────────────────────────────────────────

#[tokio::test]
async fn a_local_path_is_returned_as_is() {
    let dir = TempDir::new().unwrap();
    let real = dir.path().canonicalize().unwrap();
    let git = Recording::new(ok);
    let resolved = resolve_repo_source(&real.to_string_lossy(), &sdk(dir.path()), None, None, &git)
        .await
        .unwrap();
    assert_eq!(resolved, real);
    assert!(git.calls().is_empty());
}

#[tokio::test]
async fn a_missing_local_path_raises() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("nope");
    let error = resolve_repo_source(
        &missing.to_string_lossy(),
        &sdk(dir.path()),
        None,
        None,
        &Recording::new(ok),
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "Repository path '{}' is not a directory. Pass a local repo path or a remote git URL.",
            missing.display()
        )
    );
    assert_eq!(error.status_code(), 500);
}

#[tokio::test]
async fn remote_repos_are_refused_when_http_is_disabled() {
    let clones = TempDir::new().unwrap();
    let settings = RepoSourceSettings::new(&clones.path().to_string_lossy(), "false", false);
    let error = resolve_repo_source(URL, &settings, None, None, &Recording::new(ok))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ALLOW_HTTP_REQUESTS"), "{error}");
}

#[tokio::test]
async fn the_api_default_refuses_remote_repos_before_git_including_cached_clones() {
    for url in [
        URL,
        "git@github.com:org/repo.git",
        "ssh://git@github.com/org/repo",
    ] {
        for setting in ["", "false", "flase"] {
            let clones = TempDir::new().unwrap();
            std::fs::create_dir_all(clone_dir(url, None, clones.path()).join(".git")).unwrap();
            let settings = RepoSourceSettings::new(&clones.path().to_string_lossy(), setting, true);
            let git = Recording::new(ok);
            let error = resolve_repo_source(url, &settings, None, None, &git)
                .await
                .unwrap_err();
            assert_eq!(error.status_code(), 403, "{url} {setting}");
            // The gate's text is forwarded as is.
            assert!(
                error
                    .to_string()
                    .starts_with("Outbound HTTP requests are disabled"),
                "{error}"
            );
            assert!(error.to_string().contains("ALLOW_HTTP_REQUESTS=true"));
            assert!(git.calls().is_empty(), "{url}: git ran");
            assert!(git.outbound_checks.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn an_explicit_opt_in_allows_api_repo_cloning() {
    let clones = TempDir::new().unwrap();
    let settings = RepoSourceSettings::new(&clones.path().to_string_lossy(), "true", true);
    let git = Recording::new(ok);
    let resolved = resolve_repo_source("https://8.8.8.8/org/repo.git", &settings, None, None, &git)
        .await
        .unwrap();
    assert_eq!(resolved.parent(), Some(clones.path()));
    assert_eq!(git.calls().len(), 1);
}

/// The real outbound check: a clone of an internal address is the caller's
/// request being refused (403), before git runs.
#[tokio::test]
async fn a_clone_of_an_internal_address_is_a_403() {
    struct NoGit;
    #[async_trait::async_trait]
    impl RepoTransport for NoGit {
        async fn run_git(
            &self,
            _: &[String],
            _: Option<&Path>,
            _: &[(String, String)],
        ) -> Result<GitOutput, CodeRepositoryError> {
            panic!("git must not run");
        }
    }
    let clones = TempDir::new().unwrap();
    let settings = RepoSourceSettings::new(&clones.path().to_string_lossy(), "true", false);
    for url in [
        "https://127.0.0.1/org/repo.git",
        "http://169.254.169.254/latest/meta-data.git",
        "https://x-access-token:tok-SECRET@[::1]/o/r.git",
    ] {
        let error = resolve_repo_source(url, &settings, None, None, &NoGit)
            .await
            .unwrap_err();
        assert_eq!(error.status_code(), 403, "{url}");
        assert!(
            error.to_string().starts_with("Refusing to clone '"),
            "{error}"
        );
        assert!(!error.to_string().contains("tok-SECRET"), "{error}");
    }
}

#[tokio::test]
async fn a_remote_is_shallow_cloned() {
    let clones = TempDir::new().unwrap();
    let git = Recording::new(ok);
    let resolved = resolve_repo_source(URL, &sdk(clones.path()), None, None, &git)
        .await
        .unwrap();
    assert_eq!(resolved, clones.path().join("github.com-org-repo"));
    let calls = git.args();
    assert_eq!(calls.len(), 1);
    // core.symlinks=false is a security requirement: without it a symlink
    // committed in the remote is materialized and later written through.
    assert_eq!(
        calls[0],
        strings(&[
            "clone",
            "-c",
            "core.symlinks=false",
            "--depth",
            "1",
            URL,
            &resolved.to_string_lossy(),
        ])
    );
}

#[tokio::test]
async fn an_existing_clone_is_reused_with_pull() {
    let clones = TempDir::new().unwrap();
    let dir = existing_clone(clones.path(), "github.com-org-repo");
    let git = Recording::new(ok);
    let resolved = resolve_repo_source(URL, &sdk(clones.path()), None, None, &git)
        .await
        .unwrap();
    assert_eq!(resolved, dir);
    assert_eq!(
        git.calls(),
        vec![(strings(&["pull", "--ff-only"]), Some(dir), vec![])]
    );
}

#[tokio::test]
async fn a_failed_pull_still_reuses_the_stale_clone() {
    let clones = TempDir::new().unwrap();
    let dir = existing_clone(clones.path(), "github.com-org-repo");
    let git = Recording::new(|_| GitOutput {
        status: 1,
        stderr: "fatal: not fast-forward".to_string(),
    });
    let resolved = resolve_repo_source(URL, &sdk(clones.path()), None, None, &git)
        .await
        .unwrap();
    assert_eq!(resolved, dir);
}

#[tokio::test]
async fn a_failed_clone_raises_with_stderr() {
    let clones = TempDir::new().unwrap();
    let git = Recording::new(|_| GitOutput {
        status: 128,
        stderr: "fatal: repository not found".to_string(),
    });
    let error = resolve_repo_source(
        "https://github.com/org/missing",
        &sdk(clones.path()),
        None,
        None,
        &git,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Failed to clone 'https://github.com/org/missing': fatal: repository not found"
    );
}

#[tokio::test]
async fn system_git_runs_every_command_with_lfs_downloads_off() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipped: git is not installed");
        return;
    }
    // `git config` prints to stdout, which `run_git` drops; `--get` exits 1
    // when the key is unset, so the status tells whether the `-c` option
    // reached the command.
    let output = SystemGit
        .run_git(
            &strings(&["config", "--get", "filter.lfs.required"]),
            None,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(output.status, 0, "{}", output.stderr);
    let output = SystemGit
        .run_git(
            &strings(&["config", "--get", "filter.lfs.smudge"]),
            None,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(output.status, 0, "{}", output.stderr);
    // The caller's environment still applies.
    let output = SystemGit
        .run_git(
            &strings(&["config", "--get", "cognee.test"]),
            None,
            &[
                ("GIT_CONFIG_COUNT".into(), "1".into()),
                ("GIT_CONFIG_KEY_0".into(), "cognee.test".into()),
                ("GIT_CONFIG_VALUE_0".into(), "yes".into()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(output.status, 0, "{}", output.stderr);
}

#[test]
fn git_applies_the_scoped_header_to_the_repository_url() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipped: git is not installed");
        return;
    }
    for url in [
        "https://gitlab.example.com:8443/org/repo.git",
        "https://[2001:db8::1]:8443/o/r.git",
    ] {
        let env = credential_env("tok", url);
        let output = Command::new("git")
            .args(["config", "--get-urlmatch", "http.extraHeader", url])
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output()
            .unwrap();
        assert!(output.status.success(), "no config entry matches {url}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            env[2].1,
            "{url}"
        );
    }
}

// ─── test_resolve_repo_branch.py ──────────────────────────────────────────────

#[tokio::test]
async fn a_local_path_takes_no_branch() {
    let dir = TempDir::new().unwrap();
    let git = Recording::new(ok);
    let error = resolve_repo_source(
        &dir.path().to_string_lossy(),
        &sdk(dir.path()),
        None,
        Some("dev"),
        &git,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("branch is only supported for remote repositories"),
        "{error}"
    );
    assert!(git.calls().is_empty());
}

#[test]
fn each_branch_has_a_directory_of_its_own() {
    let base = Path::new("/clones");
    let default = clone_dir(URL, None, base);
    // The directory every clone of the URL had before a branch could be chosen.
    assert_eq!(default, base.join("github.com-org-repo"));

    let long = format!("release/{}", "x".repeat(300));
    let branches = ["dev", "Dev", "feature/x", "feature-x", long.as_str()];
    let dirs: Vec<PathBuf> = branches
        .iter()
        .map(|branch| clone_dir(URL, Some(branch), base))
        .collect();
    // Apart even where the file system folds case or the name was rewritten.
    let mut lowered: Vec<String> = std::iter::once(&default)
        .chain(dirs.iter())
        .map(|path| path.to_string_lossy().to_lowercase())
        .collect();
    lowered.sort();
    lowered.dedup();
    assert_eq!(lowered.len(), branches.len() + 1);
    assert!(
        dirs.iter()
            .all(|path| path.parent() == Some(base) && path.file_name().unwrap().len() < 100)
    );
}

#[tokio::test]
async fn a_missing_branch_is_reported_before_cloning_without_the_url_token() {
    let clones = TempDir::new().unwrap();
    let git = Recording::new(|args| GitOutput {
        status: if args[0] == "ls-remote" { 2 } else { 0 },
        stderr: String::new(),
    });
    let error = resolve_repo_source(TOKEN_URL, &sdk(clones.path()), None, Some("gone"), &git)
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("tok-SECRET"), "{error}");
    assert!(
        error
            .to_string()
            .contains(&format!("Branch 'gone' was not found in '{URL}'")),
        "{error}"
    );
    assert_eq!(
        git.args()
            .iter()
            .map(|args| args[0].clone())
            .collect::<Vec<_>>(),
        vec!["ls-remote"]
    );
}

// ─── test_resolve_repo_credentials.py ─────────────────────────────────────────

#[tokio::test]
async fn the_clone_slug_and_remote_never_carry_the_token() {
    let clones = TempDir::new().unwrap();
    let git = Recording::new(ok);
    let resolved = resolve_repo_source(TOKEN_GIT_URL, &sdk(clones.path()), None, None, &git)
        .await
        .unwrap();
    // The same directory a credential-free call (or a rotated token) uses.
    assert_eq!(resolved, clones.path().join("github.com-org-repo"));
    let calls = git.calls();
    assert_eq!(
        &calls[0].0[..5],
        &strings(&["clone", "-c", "core.symlinks=false", "--depth", "1"])[..]
    );
    assert!(calls[0].0.contains(&TOKEN_GIT_URL.to_string()));
    // The persisted remote is rewritten to the credential-free URL.
    assert_eq!(
        (calls[1].0.clone(), calls[1].1.clone()),
        (
            strings(&["remote", "set-url", "origin", CLEAN_GIT_URL]),
            Some(resolved)
        )
    );
}

#[tokio::test]
async fn an_existing_clone_pulls_from_the_credentialed_url() {
    let clones = TempDir::new().unwrap();
    let dir = existing_clone(clones.path(), "github.com-org-repo");
    let git = Recording::new(ok);
    let resolved = resolve_repo_source(TOKEN_GIT_URL, &sdk(clones.path()), None, None, &git)
        .await
        .unwrap();
    assert_eq!(resolved, dir);
    assert_eq!(
        git.calls(),
        vec![(
            strings(&["pull", "--ff-only", TOKEN_GIT_URL]),
            Some(dir),
            vec![]
        )]
    );
}

#[tokio::test]
async fn credential_free_urls_keep_the_bare_pull() {
    let clones = TempDir::new().unwrap();
    let dir = existing_clone(clones.path(), "github.com-org-repo");
    let git = Recording::new(ok);
    resolve_repo_source(CLEAN_GIT_URL, &sdk(clones.path()), None, None, &git)
        .await
        .unwrap();
    assert_eq!(
        git.calls(),
        vec![(strings(&["pull", "--ff-only"]), Some(dir), vec![])]
    );
}

#[tokio::test]
async fn out_of_band_credentials_ride_the_environment() {
    let clones = TempDir::new().unwrap();
    let git = Recording::new(ok);
    let resolved = resolve_repo_source(
        CLEAN_GIT_URL,
        &sdk(clones.path()),
        Some("tok-SECRET"),
        None,
        &git,
    )
    .await
    .unwrap();
    assert_eq!(resolved, clones.path().join("github.com-org-repo"));
    let calls = git.calls();
    assert_eq!(calls.len(), 1);
    let (args, _, env) = &calls[0];
    // argv carries only the clean URL.
    assert_eq!(
        args,
        &strings(&[
            "clone",
            "-c",
            "core.symlinks=false",
            "--depth",
            "1",
            CLEAN_GIT_URL,
            &resolved.to_string_lossy(),
        ])
    );
    assert!(!args.join(" ").contains("tok-SECRET"));
    // ...and the token rides GIT_CONFIG_* as a basic-auth header scoped to
    // the repository's origin.
    assert_eq!(
        env[1],
        (
            "GIT_CONFIG_KEY_0".to_string(),
            "http.https://github.com/.extraHeader".to_string()
        )
    );
    assert!(env[2].1.starts_with("Authorization: Basic "));
}

#[tokio::test]
async fn out_of_band_credentials_apply_to_pulls_too() {
    let clones = TempDir::new().unwrap();
    let dir = existing_clone(clones.path(), "github.com-org-repo");
    let git = Recording::new(ok);
    resolve_repo_source(
        CLEAN_GIT_URL,
        &sdk(clones.path()),
        Some("tok-SECRET"),
        None,
        &git,
    )
    .await
    .unwrap();
    let calls = git.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, strings(&["pull", "--ff-only"]));
    assert_eq!(calls[0].1.as_deref(), Some(dir.as_path()));
    assert_eq!(calls[0].2[1].1, "http.https://github.com/.extraHeader");
}

#[tokio::test]
async fn a_failed_clone_error_is_scrubbed() {
    let clones = TempDir::new().unwrap();
    let git = Recording::new(|_| GitOutput {
        status: 128,
        // git echoes the full URL, token included, in its error output.
        stderr: format!(
            "fatal: unable to access '{TOKEN_GIT_URL}': 403\n\
             redirected to https://user:other-SECRET@mirror.example.com/x"
        ),
    });
    let error = resolve_repo_source(TOKEN_GIT_URL, &sdk(clones.path()), None, None, &git)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(!message.contains("tok-SECRET"), "{message}");
    assert!(!message.contains("other-SECRET"), "{message}");
    assert!(message.contains(CLEAN_GIT_URL), "{message}");
}

// ─── test_resolve_repo_real_git.py ────────────────────────────────────────────

const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "t"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "t"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
];

/// Every git command of these tests, the test's own and `resolve_repo`'s,
/// runs on a bare configuration: the developer's global config
/// (`commit.gpgsign`, `core.hooksPath`, `init.templateDir`) would otherwise
/// fail the commits made here.
fn hermetic_env(root: &Path) -> Vec<(String, String)> {
    let config = root.join("gitconfig");
    if !config.exists() {
        std::fs::write(&config, "").unwrap();
    }
    vec![
        (
            "GIT_CONFIG_GLOBAL".to_string(),
            config.to_string_lossy().into_owned(),
        ),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
    ]
}

fn git(root: &Path, cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .envs(IDENTITY)
        .envs(hermetic_env(root))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A bare repository with a work tree to push from; every commit sets
/// BRANCH.txt.
struct Origin {
    root: PathBuf,
    bare: PathBuf,
    work: PathBuf,
}

impl Origin {
    fn new(root: &Path) -> Self {
        let remote = root.join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        let origin = Self {
            root: root.to_path_buf(),
            bare: remote.join("origin.git"),
            work: remote.join("work"),
        };
        let bare = origin.bare.to_string_lossy().into_owned();
        let work = origin.work.to_string_lossy().into_owned();
        git(
            root,
            &remote,
            &["init", "-q", "--bare", "-b", "main", &bare],
        );
        git(root, &remote, &["init", "-q", "-b", "main", &work]);
        git(root, &origin.work, &["remote", "add", "origin", &bare]);
        origin.commit_here("main-1");
        git(root, &origin.work, &["push", "-q", "origin", "main"]);
        origin
    }

    fn git(&self, args: &[&str]) {
        git(&self.root, &self.work, args);
    }

    fn commit_here(&self, text: &str) {
        std::fs::write(self.work.join("BRANCH.txt"), text).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", text]);
    }

    /// Commit on `branch` (created from main when new) and force-push it.
    fn commit(&self, branch: &str, text: &str) {
        let known = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ])
            .current_dir(&self.work)
            .envs(hermetic_env(&self.root))
            .output()
            .unwrap();
        if known.status.success() {
            self.git(&["switch", "-q", branch]);
        } else {
            self.git(&["switch", "-q", "-c", branch, "main"]);
        }
        self.commit_here(text);
        // Spelled out: a bare 'rel', 'tag' or '+dev' is what git misreads.
        self.git(&[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ]);
    }

    /// Replace the tip of `branch` with another commit and force-push it.
    fn rewrite(&self, branch: &str, text: &str) {
        self.git(&["switch", "-q", branch]);
        self.git(&["reset", "-q", "--hard", "HEAD~1"]);
        self.commit(branch, text);
    }

    /// Push a tag on a commit that no branch holds.
    fn tag(&self, name: &str, text: &str) {
        self.git(&["switch", "-q", "-c", "scratch", "main"]);
        self.commit_here(text);
        self.git(&["tag", name]);
        self.git(&[
            "push",
            "-q",
            "origin",
            &format!("refs/tags/{name}:refs/tags/{name}"),
        ]);
        self.git(&["switch", "-q", "main"]);
        self.git(&["branch", "-q", "-D", "scratch"]);
    }

    fn delete(&self, branch: &str) {
        self.git(&["push", "-q", "origin", &format!(":refs/heads/{branch}")]);
    }
}

/// Real git with the repository URL mapped to the bare origin (a `file://`
/// URL: only then is the clone shallow, like a real remote's), on the
/// hermetic configuration; the outbound check passes.
struct LocalOrigin {
    remote: String,
    env: Vec<(String, String)>,
}

#[async_trait::async_trait]
impl RepoTransport for LocalOrigin {
    async fn run_git(
        &self,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
    ) -> Result<GitOutput, CodeRepositoryError> {
        let args: Vec<String> = args
            .iter()
            .map(|arg| {
                if arg == URL {
                    self.remote.clone()
                } else {
                    arg.clone()
                }
            })
            .collect();
        let mut env = env.to_vec();
        env.extend(self.env.iter().cloned());
        SystemGit.run_git(&args, cwd, &env).await
    }

    async fn check_outbound_url(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
}

struct RealGit {
    _temp: TempDir,
    origin: Origin,
    transport: LocalOrigin,
    clones: PathBuf,
}

impl RealGit {
    fn new() -> Option<Self> {
        if Command::new("git").arg("--version").output().is_err() {
            eprintln!("skipped: git is required");
            return None;
        }
        let temp = TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let origin = Origin::new(&root);
        let transport = LocalOrigin {
            remote: format!("file://{}", origin.bare.to_string_lossy()),
            env: hermetic_env(&root),
        };
        Some(Self {
            clones: root.join("clones"),
            _temp: temp,
            origin,
            transport,
        })
    }

    async fn resolve(&self, branch: Option<&str>) -> Result<PathBuf, CodeRepositoryError> {
        resolve_repo_source(URL, &sdk(&self.clones), None, branch, &self.transport).await
    }

    fn clone_names(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.clones) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

fn content(path: &Path) -> String {
    std::fs::read_to_string(path.join("BRANCH.txt"))
        .unwrap()
        .trim()
        .to_string()
}

#[tokio::test]
async fn a_refresh_follows_the_branch_and_not_a_namesake() {
    // Each name is one git misreads when fetched bare: a tag is read before
    // the branch, `+dev` is a force refspec for `dev`, `tag` a keyword.
    for branch in ["rel", "+dev", "tag"] {
        let Some(real) = RealGit::new() else { return };
        match branch {
            "rel" => real.origin.tag("rel", "rel-tag"),
            "+dev" => real.origin.commit("dev", "dev-1"),
            _ => {}
        }
        real.origin.commit(branch, &format!("{branch}-1"));
        assert_eq!(
            content(&real.resolve(Some(branch)).await.unwrap()),
            format!("{branch}-1")
        );

        real.origin.commit(branch, &format!("{branch}-2"));
        assert_eq!(
            content(&real.resolve(Some(branch)).await.unwrap()),
            format!("{branch}-2"),
            "{branch}"
        );
    }
}

#[tokio::test]
async fn a_tag_is_not_taken_for_a_branch() {
    let Some(real) = RealGit::new() else { return };
    // `clone --branch v1` would take the tag and leave HEAD detached.
    real.origin.tag("v1", "v1-a");
    let error = real.resolve(Some("v1")).await.unwrap_err();
    assert!(
        error.to_string().starts_with("Branch 'v1' was not found"),
        "{error}"
    );
    assert!(real.clone_names().is_empty());
}

#[tokio::test]
async fn each_branch_is_a_clone_of_its_own_that_follows_force_pushes() {
    let Some(real) = RealGit::new() else { return };
    real.origin.commit("dev", "dev-1");
    real.origin.commit("feature/ü", "ü-1");

    let dev = real.resolve(Some("dev")).await.unwrap();
    let feature = real.resolve(Some("feature/ü")).await.unwrap();
    let default = real.resolve(None).await.unwrap();

    assert_eq!(
        [content(&dev), content(&feature), content(&default)],
        ["dev-1", "ü-1", "main-1"]
    );
    assert_eq!(default, real.clones.join("github.com-org-repo"));
    let mut expected: Vec<String> = [&dev, &feature, &default]
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    expected.sort();
    assert_eq!(real.clone_names(), expected);

    real.origin.commit("feature/ü", "ü-2");
    assert_eq!(
        content(&real.resolve(Some("feature/ü")).await.unwrap()),
        "ü-2"
    );
    // A force push is not a fast-forward.
    real.origin.rewrite("feature/ü", "ü-3");
    assert_eq!(
        content(&real.resolve(Some("feature/ü")).await.unwrap()),
        "ü-3"
    );
    // The other branches' clones are never touched.
    assert_eq!(content(&dev), "dev-1");
    let shallow = Command::new("git")
        .args(["rev-parse", "--is-shallow-repository"])
        .current_dir(&feature)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&shallow.stdout).trim(), "true");
    // core.symlinks=false persisted into the clone's own config.
    let symlinks = Command::new("git")
        .args(["config", "--local", "--get", "core.symlinks"])
        .current_dir(&default)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&symlinks.stdout).trim(), "false");
}

#[tokio::test]
async fn a_missing_branch_is_reported_and_never_replaces_a_clone() {
    let Some(real) = RealGit::new() else { return };
    let error = real.resolve(Some("nope")).await.unwrap_err();
    assert!(
        error.to_string().starts_with("Branch 'nope' was not found"),
        "{error}"
    );
    assert!(real.clone_names().is_empty());

    real.origin.commit("dev", "dev-1");
    let dev = real.resolve(Some("dev")).await.unwrap();
    real.origin.delete("dev");
    // A bare 'dev' would still match the tag, and hide the deletion.
    real.origin.tag("dev", "dev-tag");

    let error = real.resolve(Some("dev")).await.unwrap_err();
    assert!(
        error.to_string().starts_with("Branch 'dev' was not found"),
        "{error}"
    );
    assert_eq!(content(&dev), "dev-1");
}

#[tokio::test]
async fn a_committed_symlink_is_written_as_a_plain_file() {
    let Some(real) = RealGit::new() else { return };
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/passwd", real.origin.work.join("leak")).unwrap();
        real.origin.commit_here("with-link");
        real.origin.git(&["push", "-q", "origin", "main"]);
        let clone = real.resolve(None).await.unwrap();
        let link = clone.join("leak");
        assert!(!link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(link).unwrap(), "/etc/passwd");
    }
}
