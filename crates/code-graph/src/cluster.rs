//! One enola run per dataset: a dataset's code repositories as a cluster.
//!
//! Port of the enola half of Python's `cognee/tasks/code_graph/cluster.py`
//! (parity pin `a59433e6b`). A repository extracted on its own cannot see the
//! others: a frontend's HTTP call names a path, and only a snapshot that also
//! covers the backend can say which route serves it. enola links
//! repositories when it is given a cluster config listing them
//! ([`run_enola_cluster`](crate::run_enola_cluster)), and writes the whole
//! linked graph into every member, under a directory of the dataset's own
//! ([`cluster_output_dir`]) rather than the shared `.enola`.
//!
//! [`snapshot_dataset_code_repos`] takes that snapshot and tells which label
//! each member's facts carry ([`resolve_member_labels`]); each member is then
//! loaded on its own ([`load_cluster_member`](crate::load_cluster_member)),
//! under its own `Data` row, so deleting one row still removes exactly one
//! repository. Which rows form a dataset's cluster, and when it is rebuilt,
//! is decided by the callers (`cognee-cognify`'s `code_cluster`), which own
//! the relational side.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::config::CodeGraphConfig;
use crate::edges::build_code_graph_edges;
use crate::error::{CodeGraphError, CodeGraphResult};
use crate::runner::run_enola_cluster;
use crate::snapshot::ParsedSnapshot;

/// The ceiling of one cluster run's enola budget, in seconds. The budget is
/// the per-repository timeout times the members, up to this, so a dataset of
/// many repositories cannot hold its lock for hours on one run. Python's
/// `CLUSTER_ENOLA_TIMEOUT_CAP_SECONDS`.
pub const CLUSTER_ENOLA_TIMEOUT_CAP_SECS: f64 = 3600.0;

/// How many of a dataset's other repositories `remember(content_type="code")`
/// rebuilds together with the new ones. Beyond it only the new ones are
/// built inline and the rest are marked for the next `cognify()` of the
/// dataset, so one call stays bounded however large the dataset is. Python's
/// `CLUSTER_INLINE_REBUILD_LIMIT`.
pub const CLUSTER_INLINE_REBUILD_LIMIT: usize = 5;

/// One repository of a dataset's cluster: its owner (`data`, a `Data` row or
/// a placeholder for one not stored yet), directory and enola label. Python's
/// `ClusterMember`.
#[derive(Debug, Clone)]
pub struct ClusterMember<T> {
    /// What owns the member's nodes.
    pub data: T,
    /// The member's directory.
    pub repo_path: PathBuf,
    /// The repository label the member's facts carry.
    pub label: String,
}

/// The cluster snapshot of a dataset: where it is, who its members are and
/// what it holds. Python's `DatasetCodeSnapshot`.
#[derive(Debug, Clone)]
pub struct DatasetCodeSnapshot<T> {
    /// The first member's cluster output directory.
    pub snapshot_dir: PathBuf,
    /// The members, in cluster order.
    pub members: Vec<ClusterMember<T>>,
    /// The snapshot read once, its edges resolved once, for every member's
    /// load.
    pub parsed: Arc<ParsedSnapshot>,
}

/// Where, inside each member repository, a dataset's cluster snapshot is
/// written: `.enola-<dataset_id>`. Python's `cluster_output_dir`.
///
/// A remote clone is shared by every dataset that adds its URL, and a
/// cluster snapshot holds the facts of all of a dataset's repositories: one
/// directory per dataset keeps a cluster run from overwriting the snapshot
/// another dataset is reading. enola takes the name relative to each member.
pub fn cluster_output_dir(dataset_id: Uuid) -> String {
    format!(".enola-{dataset_id}")
}

/// A JSON object file, or `None` when it is missing, unreadable or not an
/// object.
fn read_json_object(path: &Path) -> Option<serde_json::Map<String, Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// `pathlib.Path(path).name`.
fn dir_name(repo_path: &Path) -> String {
    repo_path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// The label enola gave a member in the snapshot just taken, or `None`.
/// Python's `_written_label`.
///
/// A cluster run writes `snapshot.meta.json` into every member's
/// `output_dir` with the member's own `repo_label`. It is enola's own
/// bookkeeping rather than part of the snapshot contract, so it is trusted
/// only when it describes this snapshot (same `snapshot_id`).
fn written_label(repo_path: &Path, output_dir: &str, snapshot_id: Option<&str>) -> Option<String> {
    let meta = read_json_object(&repo_path.join(output_dir).join("snapshot.meta.json"))?;
    if let Some(snapshot_id) = snapshot_id
        && meta.get("snapshot_id").and_then(Value::as_str) != Some(snapshot_id)
    {
        return None;
    }
    match meta.get("repo_label") {
        Some(Value::String(label)) if !label.is_empty() => Some(label.clone()),
        _ => None,
    }
}

/// The labels enola may have given a repository, most likely first: the
/// repository name of the git remote in the member's own receipt, then the
/// directory name. Python's `_label_candidates`.
fn label_candidates(repo_path: &Path, output_dir: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let remote =
        read_json_object(&repo_path.join(output_dir).join("receipt.json")).and_then(|receipt| {
            match receipt.get("git") {
                Some(Value::Object(git)) => git
                    .get("remote")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                _ => None,
            }
        });
    if let Some(remote) = remote {
        let remote = remote.trim_matches('/');
        if let Some((_, name)) = remote.rsplit_once('/') {
            candidates.push(name.to_string());
        }
    }
    let name = dir_name(repo_path);
    if !candidates.contains(&name) {
        candidates.push(name);
    }
    candidates
}

/// The enola label of each member, or `None` when they cannot be told apart.
/// Python's `resolve_member_labels`.
///
/// A member whose `output_dir` carries the label enola wrote for this
/// snapshot uses it as is. For the rest the label is derived: members are
/// taken in cluster order, as enola takes them; a label another member holds
/// is not available; a candidate the snapshot's facts carry (`fact_labels`)
/// wins; and a member that produced no facts keeps its first free candidate.
/// Members are loaded by label, so one left without a label of its own, or
/// two members claiming the same written label, means no cluster.
pub fn resolve_member_labels(
    repo_paths: &[PathBuf],
    fact_labels: &HashSet<String>,
    output_dir: &str,
    snapshot_id: Option<&str>,
) -> Option<Vec<String>> {
    let written: Vec<Option<String>> = repo_paths
        .iter()
        .map(|repo_path| written_label(repo_path, output_dir, snapshot_id))
        .collect();
    let mut taken: Vec<String> = written.iter().flatten().cloned().collect();
    if taken.iter().collect::<HashSet<_>>().len() != taken.len() {
        return None;
    }

    let mut labels = Vec::with_capacity(repo_paths.len());
    for (repo_path, written_label) in repo_paths.iter().zip(written) {
        if let Some(label) = written_label {
            labels.push(label);
            continue;
        }
        let free: Vec<String> = label_candidates(repo_path, output_dir)
            .into_iter()
            .filter(|label| !taken.contains(label))
            .collect();
        let label = free
            .iter()
            .find(|label| fact_labels.contains(*label))
            .or_else(|| free.first())?
            .clone();
        taken.push(label.clone());
        labels.push(label);
    }
    Some(labels)
}

/// The enola budget of a cluster run over `members` repositories: the
/// per-repository budget times the members, capped at
/// [`CLUSTER_ENOLA_TIMEOUT_CAP_SECS`] but never below one repository's
/// budget. Python's `max(timeout, min(timeout * len(members), CAP))`.
///
/// # Errors
///
/// [`CodeGraphError::InvalidConfig`] for an unusable per-repository timeout.
pub fn cluster_enola_timeout(
    config: &CodeGraphConfig,
    members: usize,
) -> CodeGraphResult<Duration> {
    let per_repository = config.enola_timeout()?.as_secs_f64();
    let secs =
        per_repository.max((per_repository * members as f64).min(CLUSTER_ENOLA_TIMEOUT_CAP_SECS));
    Duration::try_from_secs_f64(secs).map_err(|error| {
        CodeGraphError::InvalidConfig(format!(
            "the cluster enola timeout ({secs} s) is out of range: {error}"
        ))
    })
}

/// Run enola once over a dataset's repositories. Python's
/// `snapshot_dataset_code_repos`.
///
/// `repos` pairs each repository's owner (a `Data` row, or a placeholder for
/// one not stored yet) with its directory. Returns `None` when the dataset is
/// not a cluster: fewer than two repositories whose directory exists (a
/// missing one is left out with a warning), or labels that cannot be matched
/// to their repositories; the caller then loads each repository on its own.
/// The enola budget is [`cluster_enola_timeout`].
///
/// The snapshot is parsed here, once and off the async runtime, and its
/// edges resolved once (every cluster fact carries its repository, so the
/// first member's directory never names an endpoint); the parse travels with
/// the result to every member's load.
///
/// # Errors
///
/// What [`run_enola_cluster`] and
/// [`parse_enola_snapshot`](crate::parse_enola_snapshot) raise.
pub async fn snapshot_dataset_code_repos<T>(
    config: &CodeGraphConfig,
    dataset_id: Uuid,
    repos: Vec<(T, PathBuf)>,
) -> CodeGraphResult<Option<DatasetCodeSnapshot<T>>> {
    let mut members = Vec::with_capacity(repos.len());
    for (data, repo_path) in repos {
        if repo_path.is_dir() {
            members.push((data, repo_path));
        } else {
            tracing::warn!(
                "Code repository '{}' no longer exists; leaving it out of the dataset's cluster.",
                repo_path.display()
            );
        }
    }
    if members.len() < 2 {
        return Ok(None);
    }

    let repo_paths: Vec<PathBuf> = members.iter().map(|(_, path)| path.clone()).collect();
    let timeout = cluster_enola_timeout(config, members.len())?;
    let output_dir = cluster_output_dir(dataset_id);
    let snapshot_dir = run_enola_cluster(config, &repo_paths, &output_dir, timeout).await?;

    let parsed = {
        let repo_paths = repo_paths.clone();
        let output_dir = output_dir.clone();
        tokio::task::spawn_blocking(move || -> CodeGraphResult<_> {
            let mut snapshot = ParsedSnapshot::read(&snapshot_dir)?;
            let fact_labels: HashSet<String> = snapshot
                .facts
                .iter()
                .filter_map(|fact| fact.get("repo").and_then(Value::as_str).map(str::to_string))
                .collect();
            let labels = resolve_member_labels(
                &repo_paths,
                &fact_labels,
                &output_dir,
                snapshot.identity().as_deref(),
            );
            if labels.is_some() {
                let first = repo_paths[0].to_string_lossy().into_owned();
                let (edges, skipped) = build_code_graph_edges(&snapshot.facts, Some(&first));
                tracing::info!(
                    "Resolved {} code graph edge(s) across the cluster, skipped {}.",
                    edges.len(),
                    skipped
                );
                snapshot.edges = Some(edges);
            }
            Ok((snapshot, labels))
        })
        .await
        .map_err(|error| {
            CodeGraphError::EnolaSnapshot(format!("reading the enola snapshot failed: {error}"))
        })??
    };
    let (snapshot, labels) = parsed;
    let Some(labels) = labels else {
        tracing::warn!(
            "The repositories of dataset {dataset_id} do not have distinct names; building each \
             code graph on its own, without cross-repository links."
        );
        return Ok(None);
    };

    Ok(Some(DatasetCodeSnapshot {
        snapshot_dir: snapshot.snapshot_dir.clone(),
        members: members
            .into_iter()
            .zip(labels)
            .map(|((data, repo_path), label)| ClusterMember {
                data,
                repo_path,
                label,
            })
            .collect(),
        parsed: Arc::new(snapshot),
    }))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;

    const SNAPSHOT_ID: &str = "sha256:c1u5te4";

    fn output_dir() -> String {
        cluster_output_dir(Uuid::parse_str("7d4e1c2a-0000-4000-8000-000000000000").unwrap())
    }

    /// A member's cluster output directory as a cluster run leaves it;
    /// `label` adds enola's `snapshot.meta.json` (for `label_snapshot_id`,
    /// default this snapshot).
    fn write_member(
        directory: &Path,
        remote: Option<&str>,
        label: Option<&str>,
        label_snapshot_id: Option<&str>,
    ) {
        let snapshot_dir = directory.join(output_dir());
        std::fs::create_dir_all(&snapshot_dir).unwrap();
        std::fs::write(snapshot_dir.join("facts.jsonl"), "").unwrap();
        let mut receipt = json!({"snapshot_id": SNAPSHOT_ID});
        if let Some(remote) = remote {
            receipt["git"] = json!({"remote": remote});
        }
        std::fs::write(snapshot_dir.join("receipt.json"), receipt.to_string()).unwrap();
        if let Some(label) = label {
            let meta = json!({
                "repo_label": label,
                "snapshot_id": label_snapshot_id.unwrap_or(SNAPSHOT_ID),
            });
            std::fs::write(snapshot_dir.join("snapshot.meta.json"), meta.to_string()).unwrap();
        }
    }

    fn repos(root: &Path, names: &[&str]) -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| {
                let path = root.join(name);
                std::fs::create_dir_all(&path).unwrap();
                path
            })
            .collect()
    }

    fn labels(names: &[&str]) -> HashSet<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn each_dataset_has_its_own_cluster_output_dir() {
        let other =
            cluster_output_dir(Uuid::parse_str("7d4e1c2a-0000-4000-8000-000000000001").unwrap());
        assert_eq!(output_dir(), ".enola-7d4e1c2a-0000-4000-8000-000000000000");
        assert_ne!(output_dir(), other);
        assert_ne!(other, ".enola");
    }

    #[test]
    fn member_label_is_the_one_enola_wrote_for_this_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let paths = repos(root.path(), &["one", "two"]);
        write_member(
            &paths[0],
            Some("github.com/acme/golf"),
            Some("golf-api"),
            None,
        );
        write_member(&paths[1], None, Some("ui"), None);
        assert_eq!(
            resolve_member_labels(
                &paths,
                &labels(&["golf-api", "ui"]),
                &output_dir(),
                Some(SNAPSHOT_ID)
            ),
            Some(strings(&["golf-api", "ui"]))
        );
    }

    #[test]
    fn members_with_the_same_written_label_are_not_a_cluster() {
        let root = tempfile::tempdir().unwrap();
        let paths = repos(root.path(), &["one", "two"]);
        write_member(&paths[0], None, Some("web"), None);
        write_member(&paths[1], None, Some("web"), None);
        assert_eq!(
            resolve_member_labels(&paths, &labels(&["web"]), &output_dir(), Some(SNAPSHOT_ID)),
            None
        );
    }

    #[test]
    fn a_stale_written_label_is_ignored_and_derived_instead() {
        let root = tempfile::tempdir().unwrap();
        let paths = repos(root.path(), &["golf", "ui"]);
        write_member(&paths[0], None, Some("old-golf"), Some("sha256:older"));
        write_member(&paths[1], None, Some("ui"), None);
        assert_eq!(
            resolve_member_labels(
                &paths,
                &labels(&["golf", "ui"]),
                &output_dir(),
                Some(SNAPSHOT_ID)
            ),
            Some(strings(&["golf", "ui"]))
        );
    }

    #[test]
    fn a_derived_label_avoids_one_another_member_was_written() {
        let root = tempfile::tempdir().unwrap();
        let paths = repos(root.path(), &["web", "web-b"]);
        write_member(&paths[0], None, None, None);
        write_member(&paths[1], None, Some("web"), None);
        // "web" is taken and the directory name is the only candidate.
        assert_eq!(
            resolve_member_labels(
                &paths,
                &labels(&["web", "web-b"]),
                &output_dir(),
                Some(SNAPSHOT_ID)
            ),
            None
        );
    }

    #[test]
    fn member_label_is_the_remote_name_else_the_directory_name() {
        let root = tempfile::tempdir().unwrap();
        let paths = repos(root.path(), &["checkout-1", "ui"]);
        write_member(&paths[0], Some("github.com/acme/golf"), None, None);
        write_member(&paths[1], None, None, None);
        assert_eq!(
            resolve_member_labels(&paths, &labels(&["golf", "ui"]), &output_dir(), None),
            Some(strings(&["golf", "ui"]))
        );
    }

    #[test]
    fn member_label_follows_the_facts_when_enola_fell_back_to_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let paths = repos(root.path(), &["web-a", "web-b"]);
        write_member(&paths[0], Some("github.com/acme/web"), None, None);
        write_member(&paths[1], Some("github.com/other/web/"), None, None);
        assert_eq!(
            resolve_member_labels(&paths, &labels(&["web", "web-b"]), &output_dir(), None),
            Some(strings(&["web", "web-b"]))
        );
    }

    #[test]
    fn members_that_cannot_be_told_apart_are_not_a_cluster() {
        let root = tempfile::tempdir().unwrap();
        let paths = vec![root.path().join("one/web"), root.path().join("two/web")];
        write_member(&paths[0], Some("github.com/acme/web"), None, None);
        write_member(&paths[1], Some("github.com/other/web"), None, None);
        assert_eq!(
            resolve_member_labels(&paths, &labels(&["web"]), &output_dir(), None),
            None
        );
    }

    #[test]
    fn the_cluster_timeout_scales_with_the_members_up_to_the_cap() {
        let with = |secs: f64| CodeGraphConfig {
            enola_timeout_secs: secs,
            ..CodeGraphConfig::default()
        };
        assert_eq!(
            cluster_enola_timeout(&with(10.0), 2).unwrap(),
            Duration::from_secs(20)
        );
        // 2 x 3000 s would exceed the cap: the cluster gets the cap ...
        assert_eq!(
            cluster_enola_timeout(&with(3000.0), 2).unwrap(),
            Duration::from_secs(3600)
        );
        // ... but never less than one repository's own budget.
        assert_eq!(
            cluster_enola_timeout(&with(7200.0), 2).unwrap(),
            Duration::from_secs(7200)
        );
    }

    #[tokio::test]
    async fn a_single_repository_is_not_snapshotted_as_a_cluster() {
        let root = tempfile::tempdir().unwrap();
        let paths = repos(root.path(), &["golf"]);
        // A binary that does not exist: running it would fail the call.
        let config = CodeGraphConfig {
            enola_path: "/definitely/not/here/enola".to_string(),
            ..CodeGraphConfig::default()
        };
        let snapshot = snapshot_dataset_code_repos(
            &config,
            Uuid::new_v4(),
            vec![(1, paths[0].clone()), (2, root.path().join("gone"))],
        )
        .await
        .unwrap();
        assert!(snapshot.is_none());
    }
}
