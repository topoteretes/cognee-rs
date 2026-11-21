//! Resolve repository specs (local paths or remote git URLs) to local
//! directories, shallow-cloning remote ones.
//!
//! Port of Python's `cognee/tasks/code_graph/resolve_repo.py` (parity pin
//! `a59433e6b`). Used by `remember(content_type="code")`, which takes a
//! repository path or git URL per entry, and by `add()`, which recognises a
//! GitHub/GitLab repository URL ([`code_repo_clone_url`]) and clones it the
//! same way before resolving it like a local code project
//! ([`crate::code_repo`]).
//!
//! Clones live under the clones directory ([`RepoSourceSettings::clones_dir`],
//! `code_graph.clones_dir` / `COGNEE_REPOS_DIR`, default `~/.cognee/repos`)
//! and are reused across calls: an existing clone is refreshed best-effort.
//! A `branch` is cloned into a directory of its own.
//!
//! Secrets never reach a loggable string. Out-of-band credentials go to git
//! only through environment-level git config ([`credential_env`]); a token in
//! the URL's userinfo is stripped ([`redact_repo_spec`]) from the clone
//! directory name, the stored remote, results and errors. Git LFS downloads
//! are off for every git command, and a clone writes committed symlinks as
//! plain files.
//!
//! Remote resolution follows Python's `ALLOW_HTTP_REQUESTS` gate
//! ([`HttpRequestsGate`]) and, for an http(s) URL, the outbound-address check
//! web ingestion applies ([`validate_outbound_url`]).

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use base64::Engine as _;
use regex::Regex;
use sha2::{Digest, Sha256};

/// How long one git command may run (Python `_GIT_TIMEOUT_SECONDS`).
pub const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// The prefixes that make a repository spec a remote git URL (Python
/// `_REMOTE_PREFIXES`).
const REMOTE_PREFIXES: [&str; 4] = ["https://", "http://", "git@", "ssh://"];

/// Two-segment github.com paths that are site pages, not `<owner>/<repo>`
/// (Python `_GITHUB_RESERVED_OWNERS`).
const GITHUB_RESERVED_OWNERS: [&str; 20] = [
    "about",
    "apps",
    "collections",
    "enterprise",
    "explore",
    "features",
    "login",
    "marketplace",
    "notifications",
    "organizations",
    "orgs",
    "pricing",
    "search",
    "security",
    "settings",
    "site",
    "sponsors",
    "topics",
    "trending",
    "users",
];

/// gitlab.com groups that are site pages (Python `_GITLAB_RESERVED_GROUPS`).
const GITLAB_RESERVED_GROUPS: [&str; 11] = [
    "admin",
    "api",
    "dashboard",
    "explore",
    "groups",
    "help",
    "oauth",
    "profile",
    "projects",
    "search",
    "users",
];

/// Git LFS is disabled for every git command cognee runs: an LFS file is left
/// as its small pointer file (Python `_LFS_OFF_ARGS` / `_LFS_OFF_ENV`).
pub const LFS_OFF_ARGS: [&str; 6] = [
    "-c",
    "filter.lfs.smudge=",
    "-c",
    "filter.lfs.process=",
    "-c",
    "filter.lfs.required=false",
];

/// See [`LFS_OFF_ARGS`].
pub const LFS_OFF_ENV: [(&str, &str); 1] = [("GIT_LFS_SKIP_SMUDGE", "1")];

/// The values that turn an opt-in switch on (Python `_TRUTHY`).
const TRUTHY: [&str; 4] = ["true", "1", "yes", "on"];

#[allow(
    clippy::expect_used,
    reason = "constant regex literals; a typo fails every test that clones"
)]
static SLUG_UNSAFE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^A-Za-z0-9._-]+").expect("valid slug regex"));

#[allow(
    clippy::expect_used,
    reason = "constant regex literals; a typo fails every test that clones"
)]
static BRANCH_FORBIDDEN_CHARS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\x00-\x20\x7f~^:?*\[\\]").expect("valid branch regex"));

#[allow(
    clippy::expect_used,
    reason = "constant regex literals; a typo fails every test that clones"
)]
static URL_USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"://[^/\s@]+@").expect("valid userinfo regex"));

// ─── Errors ───────────────────────────────────────────────────────────────────

/// Python's `CodeRepositoryError` (a `CogneeSystemError`) and the
/// `ValueError` of [`check_branch_name`]. The message is passed through as
/// is; [`Self::status_code`] is the HTTP status Python attaches.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeRepositoryError {
    /// The caller's request is refused (HTTP 403): remote repositories while
    /// outbound HTTP requests are disabled, or a clone of an internal
    /// address.
    #[error("{0}")]
    Refused(String),
    /// The repository could not be resolved (HTTP 500): a missing local
    /// directory, a failed clone, a missing branch, no git.
    #[error("{0}")]
    Failed(String),
    /// A branch name git would misread or that is no branch name (Python's
    /// `ValueError`, HTTP 400).
    #[error("{0}")]
    InvalidBranch(String),
}

impl CodeRepositoryError {
    /// The HTTP status Python attaches to this error.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Refused(_) => 403,
            Self::Failed(_) => 500,
            Self::InvalidBranch(_) => 400,
        }
    }

    /// The Python exception class name this error stands for.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Refused(_) | Self::Failed(_) => "CodeRepositoryError",
            Self::InvalidBranch(_) => "ValueError",
        }
    }
}

// ─── Settings ─────────────────────────────────────────────────────────────────

/// Python's `ALLOW_HTTP_REQUESTS` gate for remote repositories
/// (`ssrf_protection.is_http_requests_allowed`).
///
/// A set value applies on every surface: only `true`, `1`, `yes` or `on`
/// (any case) enable remote repositories, any other value (a misspelling
/// included) disables them. Unset or blank, API requests are refused and
/// SDK/CLI calls are allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpRequestsGate {
    /// The configured `ALLOW_HTTP_REQUESTS` value, as written (blank is
    /// unset).
    pub configured: String,
    /// Whether the call is an API (HTTP server) request, where unset means
    /// refused (Python's `ORIGIN_API`).
    pub api_request: bool,
}

impl HttpRequestsGate {
    /// The gate of an SDK, CLI or binding call: allowed unless the setting
    /// disables it.
    pub fn sdk(configured: impl Into<String>) -> Self {
        Self {
            configured: configured.into(),
            api_request: false,
        }
    }

    /// The gate of an HTTP API request: refused unless the setting enables
    /// it.
    pub fn api(configured: impl Into<String>) -> Self {
        Self {
            configured: configured.into(),
            api_request: true,
        }
    }

    /// The setting as an opt-in switch (Python `opt_in_setting`): `None`
    /// when unset or blank, else whether it is on.
    pub fn setting(&self) -> Option<bool> {
        let value = self.configured.trim().to_lowercase();
        (!value.is_empty()).then(|| TRUTHY.contains(&value.as_str()))
    }

    /// Whether remote repositories may be resolved.
    pub fn allowed(&self) -> bool {
        self.setting().unwrap_or(!self.api_request)
    }

    /// Python `require_http_requests_allowed`.
    ///
    /// # Errors
    ///
    /// [`CodeRepositoryError::Refused`] naming the setting that enables
    /// remote repositories.
    pub fn require_allowed(&self) -> Result<(), CodeRepositoryError> {
        if self.allowed() {
            return Ok(());
        }
        let configured = self.configured.trim();
        let reason = if configured.is_empty() {
            "Outbound HTTP requests are disabled by default for API requests \
             because ALLOW_HTTP_REQUESTS is unset."
                .to_string()
        } else {
            format!(
                "Outbound HTTP requests are disabled (ALLOW_HTTP_REQUESTS={configured}; \
                 only true, 1, yes or on enable them)."
            )
        };
        Err(CodeRepositoryError::Refused(format!(
            "{reason} Set ALLOW_HTTP_REQUESTS=true in the Cognee environment \
             (on the server for API calls) to enable URL ingestion and remote repository cloning."
        )))
    }
}

/// Where remote repositories are cloned and whether they may be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSourceSettings {
    /// The clones directory (Python `BaseConfig.repos_root_directory`).
    pub clones_dir: PathBuf,
    /// The `ALLOW_HTTP_REQUESTS` gate.
    pub http_requests: HttpRequestsGate,
}

impl RepoSourceSettings {
    /// Settings from the configured values: `clones_dir` (empty is
    /// [`default_clones_dir`]) and `allow_http_requests` (the raw
    /// `ALLOW_HTTP_REQUESTS` value), for an API request or not.
    pub fn new(clones_dir: &str, allow_http_requests: &str, api_request: bool) -> Self {
        let clones_dir = if clones_dir.trim().is_empty() {
            default_clones_dir()
        } else {
            PathBuf::from(clones_dir)
        };
        Self {
            clones_dir,
            http_requests: HttpRequestsGate {
                configured: allow_http_requests.to_string(),
                api_request,
            },
        }
    }

    /// The fail-closed default of a host that configured nothing: the
    /// default clones directory, and remote repositories refused unless the
    /// gate is opened (an API request with the setting unset).
    pub fn closed() -> Self {
        Self::new("", "", true)
    }
}

/// The default clones directory: `~/.cognee/repos` (Python's
/// `repos_root_directory` default), or `.cognee/repos` under the working
/// directory when there is no home directory.
pub fn default_clones_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".cognee")
        .join("repos")
}

// ─── Spec helpers ─────────────────────────────────────────────────────────────

/// Whether a repository spec is a remote git URL rather than a local path
/// (Python `is_remote_repo`).
pub fn is_remote_repo(spec: &str) -> bool {
    REMOTE_PREFIXES
        .iter()
        .any(|prefix| spec.starts_with(prefix))
}

/// Python's `urllib.parse.urlsplit`, for the parts this module reads:
/// `(scheme, netloc, path, query, fragment)`. The scheme is lowercased, as
/// Python's is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SplitUrl<'a> {
    scheme: String,
    netloc: &'a str,
    path: &'a str,
    query: &'a str,
    fragment: &'a str,
}

fn split_url(url: &str) -> SplitUrl<'_> {
    // A scheme is letters, digits, `+`, `-` and `.` after a leading letter,
    // followed by `:`.
    let (scheme, rest) = match url.find(':') {
        Some(colon)
            if colon > 0
                && url[..colon]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                && url[..colon]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) =>
        {
            (url[..colon].to_ascii_lowercase(), &url[colon + 1..])
        }
        _ => (String::new(), url),
    };
    let (netloc, rest) = match rest.strip_prefix("//") {
        Some(after) => {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            (&after[..end], &after[end..])
        }
        None => ("", rest),
    };
    let (rest, fragment) = match rest.find('#') {
        Some(hash) => (&rest[..hash], &rest[hash + 1..]),
        None => (rest, ""),
    };
    let (path, query) = match rest.find('?') {
        Some(question) => (&rest[..question], &rest[question + 1..]),
        None => (rest, ""),
    };
    SplitUrl {
        scheme,
        netloc,
        path,
        query,
        fragment,
    }
}

/// Python `urlsplit(...).hostname`: the netloc without userinfo and port,
/// brackets of an IPv6 literal stripped, lowercased; `None` when empty.
fn hostname(netloc: &str) -> Option<String> {
    let host_port = netloc.rsplit_once('@').map_or(netloc, |(_, host)| host);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed
            .split_once(']')
            .map_or(bracketed, |(host, _)| host)
    } else {
        host_port
            .split_once(':')
            .map_or(host_port, |(host, _)| host)
    };
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// Python `urlunsplit` for the parts this module writes.
fn join_url(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    let mut url = String::new();
    if !scheme.is_empty() {
        url.push_str(scheme);
        url.push(':');
    }
    if !netloc.is_empty() || scheme_uses_netloc(scheme) {
        url.push_str("//");
        url.push_str(netloc);
    }
    url.push_str(path);
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    if !fragment.is_empty() {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

fn scheme_uses_netloc(scheme: &str) -> bool {
    matches!(
        scheme,
        "http" | "https" | "ssh" | "git" | "git+ssh" | "ftp" | "file" | "svn" | "svn+ssh"
    )
}

/// The clone URL when an http(s) string names a whole git repository, else
/// `None` (Python `code_repo_clone_url`).
///
/// Detected: repository roots on github.com (`<owner>/<repo>`) and
/// gitlab.com (`<group>/<repo>`, nested groups included), with an optional
/// `.git` suffix, trailing slash, `www.` prefix, query or fragment — and any
/// http(s) URL ending in `.git` on any host. Deeper forge URLs (`/blob/`,
/// `/tree/`, `/issues`, GitLab's `/-/` pages) and forge site pages
/// (`/topics/x`, `/explore`) are pages, not repositories. `git@` and
/// `ssh://` specs are not detected (they stay explicit through
/// `remember(content_type="code")`).
///
/// The returned URL is normalised for `git clone`: query and fragment
/// dropped, trailing slash removed, userinfo (a token) kept — redact it with
/// [`redact_repo_spec`] before persisting or logging.
pub fn code_repo_clone_url(spec: &str) -> Option<String> {
    let parts = split_url(spec.trim());
    if !matches!(parts.scheme.as_str(), "http" | "https") {
        return None;
    }
    let host = hostname(parts.netloc)?;
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let segments: Vec<&str> = parts.path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return None;
    }
    let is_repo = if segments.last().is_some_and(|last| last.ends_with(".git")) {
        true
    } else if host == "github.com" {
        segments.len() == 2 && !GITHUB_RESERVED_OWNERS.contains(&segments[0])
    } else if host == "gitlab.com" {
        !segments.contains(&"-") && !GITLAB_RESERVED_GROUPS.contains(&segments[0])
    } else {
        false
    };
    is_repo.then(|| {
        join_url(
            &parts.scheme,
            parts.netloc,
            &format!("/{}", segments.join("/")),
            "",
            "",
        )
    })
}

/// The spec with any URL-embedded credentials removed (Python
/// `redact_repo_spec`): `https://user:token@host/x` becomes `https://host/x`.
/// Anything persisted or logged — clone directory names, the stored git
/// remote, result items, error messages — uses this form.
pub fn redact_repo_spec(spec: &str) -> String {
    if !spec.contains("://") {
        return spec.to_string();
    }
    let parts = split_url(spec);
    let Some((_, host)) = parts.netloc.rsplit_once('@') else {
        return spec.to_string();
    };
    join_url(&parts.scheme, host, parts.path, parts.query, parts.fragment)
}

/// A stable directory name for a remote URL, e.g. `github.com-org-repo`
/// (Python `_clone_slug`).
///
/// A chosen branch gets its own directory beside it, e.g.
/// `github.com-org-repo@dev-<hash>`, so no clone is ever replaced under
/// another caller. The hash of the exact name keeps apart branches the
/// readable part merges (case-insensitive file systems, replaced
/// characters).
pub fn clone_slug(url: &str, branch: Option<&str>) -> String {
    let tail = url.rsplit("://").next().unwrap_or(url).replace(':', "/");
    let tail = tail.trim_end_matches('/');
    let tail = tail.strip_suffix(".git").unwrap_or(tail);
    let slug = SLUG_UNSAFE
        .replace_all(tail, "-")
        .trim_matches(['-', '.'])
        .to_string();
    let Some(branch) = branch else {
        return slug;
    };
    let readable: String = SLUG_UNSAFE
        .replace_all(branch, "-")
        .chars()
        .take(40)
        .collect();
    let digest = hex::encode(Sha256::digest(branch.as_bytes()));
    format!("{slug}@{readable}-{}", &digest[..8])
}

/// Where [`resolve_repo_source`] clones the credential-free `url` on
/// `branch` (`None`: the remote's default branch) under `clones_dir`
/// (Python `clone_dir`).
pub fn clone_dir(url: &str, branch: Option<&str>, clones_dir: &Path) -> PathBuf {
    clones_dir.join(clone_slug(url, branch))
}

/// Python's `repr()` of a string, for messages that quote a caller's value.
fn py_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Why `branch` cannot be a branch name, or `None` (Python
/// `_invalid_branch_reason`: `git check-ref-format --branch` semantics, with
/// a bare `@` refused), so a name git would read as an option never reaches
/// it.
fn invalid_branch_reason(branch: &str) -> Option<String> {
    if branch.is_empty() {
        return Some("it must be a non-empty string".to_string());
    }
    if branch.starts_with('-') {
        return Some("it must not start with '-'".to_string());
    }
    if branch == "HEAD" || branch == "@" {
        return Some(format!("'{branch}' is not a branch name"));
    }
    if BRANCH_FORBIDDEN_CHARS.is_match(branch) {
        return Some("it contains a space, control character or one of ~ ^ : ? * [ \\".to_string());
    }
    if branch.contains("..") || branch.contains("@{") || branch.contains("//") {
        return Some("it contains '..', '@{' or '//'".to_string());
    }
    if branch.starts_with('/') || branch.ends_with('/') || branch.ends_with('.') {
        return Some("it must not start with '/' or end with '/' or '.'".to_string());
    }
    if branch
        .split('/')
        .any(|part| part.starts_with('.') || part.ends_with(".lock"))
    {
        return Some("a path component must not start with '.' or end with '.lock'".to_string());
    }
    None
}

/// Check that `branch` is a usable branch name (Python `check_branch_name`).
///
/// # Errors
///
/// [`CodeRepositoryError::InvalidBranch`]: `Invalid branch name '<name>':
/// <reason>.`
pub fn check_branch_name(branch: &str) -> Result<(), CodeRepositoryError> {
    match invalid_branch_reason(branch) {
        None => Ok(()),
        Some(reason) => Err(CodeRepositoryError::InvalidBranch(format!(
            "Invalid branch name {}: {reason}.",
            py_repr(branch)
        ))),
    }
}

/// Auth for git over https, as environment-level git config (Python
/// `_credential_env`).
///
/// The environment channel (git >= 2.31) keeps the token out of argv (which
/// any local process can read from the process listing), out of every
/// URL-shaped string, and out of the on-disk remote config. The header is
/// scoped to the origin of the credential-free `url`
/// (`http.<origin>/.extraHeader`), so git and git-lfs send it only to that
/// host.
pub fn credential_env(token: &str, url: &str) -> Vec<(String, String)> {
    let parts = split_url(url);
    // The netloc, not the hostname: an IPv6 literal keeps its brackets,
    // which git needs.
    let host = parts
        .netloc
        .rsplit_once('@')
        .map_or(parts.netloc, |(_, h)| h);
    let origin = format!("{}://{host}", parts.scheme);
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
    vec![
        ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        (
            "GIT_CONFIG_KEY_0".to_string(),
            format!("http.{origin}/.extraHeader"),
        ),
        (
            "GIT_CONFIG_VALUE_0".to_string(),
            format!("Authorization: Basic {basic}"),
        ),
    ]
}

// ─── Outbound address check ───────────────────────────────────────────────────

/// Whether `ip` is not a routable, public address (Python `_is_blocked_ip`):
/// private, loopback, link-local, reserved, multicast, unspecified, shared
/// (100.64/10), or an IPv4-mapped IPv6 address that decodes to one.
pub(crate) fn is_blocked_ip(ip: IpAddr) -> bool {
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // Shared address space (100.64/10), "this network" (0/8) and
                // the reserved 240/4.
                || (a == 100 && (64..=127).contains(&b))
                || a == 0
                || a >= 240
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                // Deprecated site-local (fec0::/10) and documentation
                // (2001:db8::/32).
                || (first & 0xffc0) == 0xfec0
                || (first == 0x2001 && v6.segments()[1] == 0x0db8)
        }
    }
}

/// Validate a caller-supplied http(s) URL before git fetches it server-side
/// (Python `validate_outbound_url`, past its `ALLOW_HTTP_REQUESTS` check,
/// which [`resolve_repo_source`] makes first): the scheme must be http(s),
/// the URL must have a host, and neither an IP-literal host nor any address
/// a host name resolves to may be internal or reserved.
///
/// # Errors
///
/// The refusal message (Python's `SSRFProtectionError` text).
pub async fn validate_outbound_url(url: &str) -> Result<(), String> {
    let parts = split_url(url);
    if !matches!(parts.scheme.as_str(), "http" | "https") {
        let scheme = if parts.scheme.is_empty() {
            "(none)"
        } else {
            parts.scheme.as_str()
        };
        return Err(format!(
            "URL scheme '{scheme}' is not allowed; only http/https are permitted."
        ));
    }
    let Some(host) = hostname(parts.netloc) else {
        return Err("URL does not contain a valid host.".to_string());
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_blocked_ip(ip) {
            return Err(format!(
                "URL host '{host}' points to a non-public address and is blocked."
            ));
        }
        return Ok(());
    }
    let resolved: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), 0))
        .await
        .map_err(|_| format!("Could not resolve URL host '{host}'."))?
        .map(|addr| addr.ip())
        .collect();
    if resolved.is_empty() {
        return Err(format!("Could not resolve URL host '{host}'."));
    }
    if let Some(blocked) = resolved.into_iter().find(|ip| is_blocked_ip(*ip)) {
        return Err(format!(
            "URL host '{host}' resolves to a non-public address ({blocked}) and is blocked."
        ));
    }
    Ok(())
}

// ─── Git ──────────────────────────────────────────────────────────────────────

/// What one git command ended with: its exit status (`-1` when a signal
/// ended it) and its standard error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub status: i32,
    pub stderr: String,
}

impl GitOutput {
    fn ok(&self) -> bool {
        self.status == 0
    }
}

/// How [`resolve_repo_source`] reaches the outside world: git, and the
/// outbound-URL check made before an http(s) clone. [`SystemGit`] is the
/// real one; tests substitute either half (Python's tests patch `_run_git`
/// and `validate_outbound_url`).
#[async_trait::async_trait]
pub trait RepoTransport: Send + Sync {
    /// Run `git <args>` (Python `_run_git`): in `cwd`, with `env` added to
    /// the process environment. Git LFS is off for every command
    /// ([`LFS_OFF_ARGS`], [`LFS_OFF_ENV`]).
    ///
    /// # Errors
    ///
    /// [`CodeRepositoryError::Failed`] when git cannot be started or runs
    /// past [`GIT_TIMEOUT`].
    async fn run_git(
        &self,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
    ) -> Result<GitOutput, CodeRepositoryError>;

    /// The outbound check an http(s) URL clears before it is cloned
    /// ([`validate_outbound_url`]).
    ///
    /// # Errors
    ///
    /// The refusal message.
    async fn check_outbound_url(&self, url: &str) -> Result<(), String> {
        validate_outbound_url(url).await
    }
}

/// The `git` on `PATH`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGit;

#[async_trait::async_trait]
impl RepoTransport for SystemGit {
    async fn run_git(
        &self,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
    ) -> Result<GitOutput, CodeRepositoryError> {
        let mut command = tokio::process::Command::new("git");
        command
            .args(LFS_OFF_ARGS)
            .args(args)
            .envs(LFS_OFF_ENV)
            .envs(
                env.iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CodeRepositoryError::Failed(
                    "git is required to clone remote repositories but was not found on PATH."
                        .to_string(),
                )
            } else {
                CodeRepositoryError::Failed(format!("Could not run git: {error}"))
            }
        })?;
        let subcommand = args.first().map_or("", String::as_str);
        // Dropping the child on timeout kills it (`kill_on_drop`).
        let output = tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| {
                CodeRepositoryError::Failed(format!(
                    "git {subcommand} timed out after {}s.",
                    GIT_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|error| CodeRepositoryError::Failed(format!("Could not run git: {error}")))?;
        Ok(GitOutput {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// The shared [`SystemGit`].
pub fn system_git() -> Arc<dyn RepoTransport> {
    Arc::new(SystemGit)
}

impl fmt::Debug for dyn RepoTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RepoTransport")
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

/// Whether the remote answers and has no branch called `branch` (Python
/// `_remote_ref_missing`): `ls-remote --exit-code` ends with status 2
/// exactly when it reached the remote and nothing matched.
async fn remote_ref_missing(
    transport: &dyn RepoTransport,
    remote: &str,
    branch: &str,
    env: &[(String, String)],
) -> Result<bool, CodeRepositoryError> {
    let output = transport
        .run_git(
            &strings(&[
                "ls-remote",
                "--exit-code",
                remote,
                &format!("refs/heads/{branch}"),
            ]),
            None,
            env,
        )
        .await?;
    Ok(output.status == 2)
}

/// The last `n` characters of `text` (Python `text[-n:]`).
fn tail_chars(text: &str, n: usize) -> &str {
    let count = text.chars().count();
    if count <= n {
        return text;
    }
    let skip = count - n;
    text.char_indices()
        .nth(skip)
        .map_or(text, |(index, _)| &text[index..])
}

/// A local repository spec resolved to its directory: the real path, which
/// must be a directory.
///
/// # Errors
///
/// [`CodeRepositoryError::Failed`] when it is not an existing directory.
pub fn resolve_local_repo(spec: &str) -> Result<PathBuf, CodeRepositoryError> {
    let not_a_directory = || {
        CodeRepositoryError::Failed(format!(
            "Repository path '{spec}' is not a directory. \
             Pass a local repo path or a remote git URL."
        ))
    };
    let local = spec.strip_prefix("file://").unwrap_or(spec);
    let path = std::fs::canonicalize(Path::new(local)).map_err(|_| not_a_directory())?;
    if path.is_dir() {
        Ok(path)
    } else {
        Err(not_a_directory())
    }
}

/// Whether `target` stays inside `base` (Python's realpath containment
/// check): an existing target is compared by its real path; a new one by
/// its name, which must be a single plain component.
fn clone_target_is_contained(base: &Path, target: &Path) -> bool {
    let Some(name) = target.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.is_empty() || name == "." || name == ".." || target.parent() != Some(base) {
        return false;
    }
    if target.symlink_metadata().is_ok() {
        return match (std::fs::canonicalize(base), std::fs::canonicalize(target)) {
            (Ok(base), Ok(target)) => target.starts_with(&base) && target != base,
            _ => false,
        };
    }
    true
}

/// Return a local directory for a repository spec, shallow-cloning a remote
/// URL (Python `resolve_repo_source`).
///
/// A local path is resolved and must be a directory; it takes no `branch`.
/// A remote URL needs the `ALLOW_HTTP_REQUESTS` gate open
/// ([`RepoSourceSettings::http_requests`]) and, for http(s), the outbound
/// check passed; it is cloned with `--depth 1` into the clones directory,
/// and an existing clone is reused after a best-effort refresh (`git pull
/// --ff-only`, or for a branch a fetch and reset, which follows a force
/// push). A `branch` is cloned into a directory of its own; one the remote
/// does not have is an error.
///
/// `credentials` authenticates against a private https remote (e.g. a
/// GitHub App installation token) through environment-level git config only
/// ([`credential_env`]), so the URL and everything derived from it never
/// carries the secret. Credentials in the URL userinfo still work (the
/// legacy path) and are redacted from the directory name, the stored
/// remote, logs and errors.
///
/// # Errors
///
/// [`CodeRepositoryError::Refused`] when remote repositories are disabled
/// or the URL targets an internal address; [`CodeRepositoryError::Failed`]
/// for a missing local directory, a branch on a local path, a missing
/// branch, a failed clone or no git.
pub async fn resolve_repo_source(
    spec: &str,
    settings: &RepoSourceSettings,
    credentials: Option<&str>,
    branch: Option<&str>,
    transport: &dyn RepoTransport,
) -> Result<PathBuf, CodeRepositoryError> {
    if !is_remote_repo(spec) {
        if branch.is_some() {
            return Err(CodeRepositoryError::Failed(format!(
                "branch is only supported for remote repositories, but '{spec}' is a \
                 local path. Check the branch out in that directory instead."
            )));
        }
        return resolve_local_repo(spec);
    }

    settings.http_requests.require_allowed()?;

    let url = spec;
    // The URL is caller-supplied and git runs here, so an http(s) remote
    // must clear the same outbound check web ingestion's URLs clear. ssh://
    // and git@host: are not routed through it (internal git servers over
    // SSH are a legitimate setup).
    if url.to_lowercase().starts_with("http://") || url.to_lowercase().starts_with("https://") {
        transport.check_outbound_url(url).await.map_err(|message| {
            CodeRepositoryError::Refused(format!(
                "Refusing to clone '{}': {message}",
                redact_repo_spec(url)
            ))
        })?;
    }

    // Directory name, remote, logs and errors all use the credential-free
    // URL: tokens in the userinfo are short-lived, so persisting one would
    // both leak it and (through the directory name) clone anew per token.
    let clean_url = redact_repo_spec(url);
    let has_credentials = clean_url != url;
    let scrub = |text: &str| -> String {
        // git's error output often echoes the URL, token included: strip the
        // exact spec, then any other URL userinfo git may print.
        let text = if has_credentials {
            text.replace(url, &clean_url)
        } else {
            text.to_string()
        };
        URL_USERINFO.replace_all(&text, "://").into_owned()
    };
    let auth_env = credentials
        .map(|token| credential_env(token, &clean_url))
        .unwrap_or_default();

    let base = settings.clones_dir.as_path();
    let target = clone_dir(&clean_url, branch, base);
    if !clone_target_is_contained(base, &target) {
        return Err(CodeRepositoryError::Failed(format!(
            "Could not derive a safe clone name for {clean_url}."
        )));
    }
    let branch_not_found = |branch: &str| {
        CodeRepositoryError::Failed(format!("Branch '{branch}' was not found in '{clean_url}'."))
    };

    if target.join(".git").is_dir() {
        // Legacy URL-embedded credentials: fetch from the explicit URL, not
        // the (credential-free) stored remote. Out-of-band credentials ride
        // the environment.
        let mut refreshed = match branch {
            None => {
                let mut args = strings(&["pull", "--ff-only"]);
                if has_credentials {
                    args.push(url.to_string());
                }
                transport.run_git(&args, Some(&target), &auth_env).await?
            }
            Some(branch) => {
                // Fetch and reset, not pull: a rebased or force-pushed branch
                // is not a fast-forward. The ref is spelled out because git
                // resolves a bare name itself (a tag before a branch, `+x` as
                // a force refspec).
                let remote = if has_credentials { url } else { "origin" };
                let fetched = transport
                    .run_git(
                        &strings(&[
                            "fetch",
                            "--depth",
                            "1",
                            remote,
                            &format!("refs/heads/{branch}"),
                        ]),
                        Some(&target),
                        &auth_env,
                    )
                    .await?;
                if !fetched.ok() && remote_ref_missing(transport, url, branch, &auth_env).await? {
                    return Err(branch_not_found(branch));
                }
                if fetched.ok() {
                    transport
                        .run_git(
                            &strings(&["reset", "--hard", "FETCH_HEAD"]),
                            Some(&target),
                            &[],
                        )
                        .await?
                } else {
                    fetched
                }
            }
        };
        if !refreshed.ok() {
            // A stale clone is still indexable; the caller asked for the
            // repository, not for freshness guarantees.
            refreshed.stderr = scrub(tail_chars(&refreshed.stderr, 500));
            tracing::warn!(
                "Reusing existing clone at {} (refresh failed: {})",
                target.display(),
                refreshed.stderr
            );
        }
        return Ok(target);
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            CodeRepositoryError::Failed(format!(
                "Could not create the clones directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    // Checked first: `clone --branch` falls back to a tag of that name and
    // leaves HEAD detached, which no later fetch of the branch would follow.
    if let Some(branch) = branch
        && remote_ref_missing(transport, url, branch, &auth_env).await?
    {
        return Err(branch_not_found(branch));
    }
    tracing::info!("Cloning {clean_url} into {}", target.display());
    // core.symlinks=false: git would otherwise materialize symlinks committed
    // in the remote, and nothing re-validates paths under the clone (a
    // `.enola -> ~/.ssh` link would have the enola snapshot written through
    // it). Set on the clone, so it persists into the clone's config and later
    // refreshes honour it too.
    let mut args = strings(&["clone", "-c", "core.symlinks=false", "--depth", "1"]);
    if let Some(branch) = branch {
        args.push("--branch".to_string());
        args.push(branch.to_string());
    }
    args.push(url.to_string());
    args.push(target.to_string_lossy().into_owned());
    let cloned = transport.run_git(&args, None, &auth_env).await?;
    if !cloned.ok() {
        return Err(CodeRepositoryError::Failed(format!(
            "Failed to clone '{clean_url}': {}",
            scrub(tail_chars(&cloned.stderr, 1000))
        )));
    }
    if has_credentials {
        transport
            .run_git(
                &strings(&["remote", "set-url", "origin", &clean_url]),
                Some(&target),
                &[],
            )
            .await?;
    }
    Ok(target)
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
    fn remote_specs_are_detected_by_prefix() {
        assert!(is_remote_repo("https://github.com/org/repo"));
        assert!(is_remote_repo("http://gitlab.example.com/org/repo.git"));
        assert!(is_remote_repo("git@github.com:org/repo.git"));
        assert!(is_remote_repo("ssh://git@github.com/org/repo"));
        assert!(!is_remote_repo("/local/path/repo"));
        assert!(!is_remote_repo("relative/path"));
        assert!(!is_remote_repo("file:///tmp/repo"));
    }

    #[test]
    fn clone_slug_is_stable_and_filesystem_safe() {
        assert_eq!(
            clone_slug("https://github.com/org/repo.git", None),
            "github.com-org-repo"
        );
        assert_eq!(
            clone_slug("https://github.com/org/repo", None),
            "github.com-org-repo"
        );
        assert_eq!(
            clone_slug("git@github.com:org/repo.git", None),
            "git-github.com-org-repo"
        );
    }

    #[test]
    fn branch_slugs_match_python() {
        // Values Python's `_clone_slug` gives at a59433e6b.
        let url = "https://github.com/org/repo";
        assert_eq!(
            clone_slug(url, Some("dev")),
            "github.com-org-repo@dev-ef260e9a"
        );
        assert_eq!(
            clone_slug(url, Some("feature/ü")),
            "github.com-org-repo@feature--1363e351"
        );
        assert_eq!(
            clone_slug(url, Some(&format!("release/{}", "x".repeat(300)))),
            "github.com-org-repo@release-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-8292e71b"
        );
    }

    #[test]
    fn redact_strips_userinfo_only() {
        let token = "https://x-access-token:tok-SECRET@github.com/org/repo.git";
        let clean = "https://github.com/org/repo.git";
        assert_eq!(redact_repo_spec(token), clean);
        assert_eq!(
            redact_repo_spec("https://user@host/path"),
            "https://host/path"
        );
        assert_eq!(redact_repo_spec(clean), clean);
        assert_eq!(redact_repo_spec("/local/path/repo"), "/local/path/repo");
        assert_eq!(
            redact_repo_spec("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
        assert_eq!(
            redact_repo_spec("https://x-access-token:secret@github.com/o/r?x=1#f"),
            "https://github.com/o/r?x=1#f"
        );
        assert_eq!(
            redact_repo_spec("ssh://git@github.com/org/repo"),
            "ssh://github.com/org/repo"
        );
    }

    #[test]
    fn clone_urls_detect_repository_roots() {
        let cases = [
            ("https://github.com/org/repo", "https://github.com/org/repo"),
            (
                "https://github.com/org/repo.git",
                "https://github.com/org/repo.git",
            ),
            (
                "https://github.com/org/repo/",
                "https://github.com/org/repo",
            ),
            ("http://github.com/org/repo", "http://github.com/org/repo"),
            (
                "https://www.github.com/org/repo?tab=readme-ov-file#readme",
                "https://www.github.com/org/repo",
            ),
            (
                "  https://github.com/org/repo  ",
                "https://github.com/org/repo",
            ),
            (
                "https://gitlab.com/group/repo",
                "https://gitlab.com/group/repo",
            ),
            (
                "https://gitlab.com/group/subgroup/repo",
                "https://gitlab.com/group/subgroup/repo",
            ),
            (
                "https://git.example.com/team/repo.git",
                "https://git.example.com/team/repo.git",
            ),
            (
                "https://x-access-token:tok@github.com/org/repo",
                "https://x-access-token:tok@github.com/org/repo",
            ),
            ("HTTPS://GitHub.com/org/repo", "https://GitHub.com/org/repo"),
        ];
        for (spec, clone_url) in cases {
            assert_eq!(
                code_repo_clone_url(spec).as_deref(),
                Some(clone_url),
                "{spec}"
            );
        }
    }

    #[test]
    fn clone_urls_leave_pages_and_other_specs_alone() {
        for spec in [
            "https://github.com/org/repo/blob/main/README.md",
            "https://github.com/org/repo/tree/main/src",
            "https://github.com/org/repo/issues/12",
            "https://github.com/org/repo/pull/3",
            "https://gitlab.com/group/repo/-/blob/main/README.md",
            "https://gitlab.com/group/repo/-/merge_requests/1",
            "https://github.com/topics/python",
            "https://github.com/orgs/topoteretes",
            "https://gitlab.com/explore/projects",
            "https://github.com/org",
            "https://github.com",
            "https://git.example.com/team/repo",
            "https://example.com/some/page",
            "git@github.com:org/repo.git",
            "ssh://git@github.com/org/repo",
            "/local/path/repo",
            "Read https://github.com/org/repo tonight",
            "",
        ] {
            assert_eq!(code_repo_clone_url(spec), None, "{spec}");
        }
    }

    #[test]
    fn credentials_header_is_scoped_to_the_repository_origin() {
        for (url, origin) in [
            (
                "https://gitlab.example.com:8443/org/repo.git",
                "https://gitlab.example.com:8443",
            ),
            ("https://github.com/org/repo.git", "https://github.com"),
            (
                "https://[2001:db8::1]:8443/org/repo.git",
                "https://[2001:db8::1]:8443",
            ),
            ("https://[::1]/org/repo.git", "https://[::1]"),
        ] {
            let env = credential_env("tok", url);
            assert_eq!(env[0], ("GIT_CONFIG_COUNT".into(), "1".into()));
            assert_eq!(env[1].1, format!("http.{origin}/.extraHeader"));
            assert_eq!(
                env[2].1,
                format!(
                    "Authorization: Basic {}",
                    base64::engine::general_purpose::STANDARD.encode("x-access-token:tok")
                )
            );
        }
    }

    #[test]
    fn branch_names_follow_git() {
        let names_git_would_misread = ["", "-x", "--upload-pack=touch /tmp/x", "HEAD", "@", "a..b"];
        let names_that_are_not_refs = ["a b", "a~1", "a:b", "a/", "a//b", ".a", "a.lock", "a@{b"];
        for branch in names_git_would_misread
            .iter()
            .chain(names_that_are_not_refs.iter())
        {
            let error = check_branch_name(branch).unwrap_err();
            assert!(
                error.to_string().starts_with("Invalid branch name"),
                "{branch}: {error}"
            );
            assert_eq!(error.status_code(), 400);
        }
        for branch in [
            "main",
            "feature/x",
            "release/1.2.3",
            "v1.0",
            "+dev",
            "tag",
            "feature/ü",
        ] {
            check_branch_name(branch).unwrap();
        }
        assert_eq!(
            check_branch_name("-x").unwrap_err().to_string(),
            "Invalid branch name '-x': it must not start with '-'."
        );
        assert_eq!(
            check_branch_name("a\tb").unwrap_err().to_string(),
            "Invalid branch name 'a\\tb': it contains a space, control character or one of \
             ~ ^ : ? * [ \\."
        );
    }

    #[test]
    fn the_gate_follows_pythons_opt_in_rules() {
        assert!(HttpRequestsGate::sdk("").allowed());
        assert!(HttpRequestsGate::sdk("  ").allowed());
        assert!(!HttpRequestsGate::sdk("false").allowed());
        assert!(!HttpRequestsGate::sdk("flase").allowed());
        assert!(!HttpRequestsGate::api("").allowed());
        for on in ["true", "1", "yes", "ON", " True "] {
            assert!(HttpRequestsGate::api(on).allowed(), "{on}");
        }
        assert_eq!(
            HttpRequestsGate::api("")
                .require_allowed()
                .unwrap_err()
                .to_string(),
            "Outbound HTTP requests are disabled by default for API requests because \
             ALLOW_HTTP_REQUESTS is unset. Set ALLOW_HTTP_REQUESTS=true in the Cognee \
             environment (on the server for API calls) to enable URL ingestion and remote \
             repository cloning."
        );
        let refused = HttpRequestsGate::sdk(" flase ")
            .require_allowed()
            .unwrap_err();
        assert_eq!(refused.status_code(), 403);
        assert!(
            refused
                .to_string()
                .starts_with("Outbound HTTP requests are disabled (ALLOW_HTTP_REQUESTS=flase;"),
            "{refused}"
        );
    }

    #[test]
    fn blocked_addresses_cover_pythons_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_blocked_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "140.82.112.3", "2606:4700::1111"] {
            assert!(!is_blocked_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn outbound_check_refuses_internal_literals_and_bad_urls() {
        assert_eq!(
            validate_outbound_url("https://127.0.0.1/org/repo.git")
                .await
                .unwrap_err(),
            "URL host '127.0.0.1' points to a non-public address and is blocked."
        );
        assert_eq!(
            validate_outbound_url("https://[::1]/org/repo.git")
                .await
                .unwrap_err(),
            "URL host '::1' points to a non-public address and is blocked."
        );
        assert_eq!(
            validate_outbound_url("ftp://example.com/x")
                .await
                .unwrap_err(),
            "URL scheme 'ftp' is not allowed; only http/https are permitted."
        );
        assert_eq!(
            validate_outbound_url("https:///x").await.unwrap_err(),
            "URL does not contain a valid host."
        );
        validate_outbound_url("https://8.8.8.8/org/repo.git")
            .await
            .unwrap();
        assert!(
            validate_outbound_url("https://localhost/org/repo.git")
                .await
                .unwrap_err()
                .contains("non-public address")
        );
    }

    #[test]
    fn tails_count_characters() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("ab", 3), "ab");
        assert_eq!(tail_chars("üüü", 2), "üü");
    }

    #[test]
    fn default_clones_dir_is_under_the_home_directory() {
        assert!(default_clones_dir().ends_with(".cognee/repos"));
        let settings = RepoSourceSettings::new("/srv/repos", "", false);
        assert_eq!(settings.clones_dir, PathBuf::from("/srv/repos"));
        assert!(settings.http_requests.allowed());
        assert!(!RepoSourceSettings::closed().http_requests.allowed());
    }
}
