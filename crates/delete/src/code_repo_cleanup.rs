//! Working-copy cleanup for code-repository rows (SDK-1701, Rust-only:
//! Python's delete leaves both behind).
//!
//! A code repository is ingested as one `Data` row stamped
//! `{"source": "code_repo", "repo_path": …[, "repo_url": …]}`. Two things
//! on disk outlive such a row unless the delete removes them:
//!
//! - **`<repo_path>/.enola-<dataset_id>`**, the dataset's cluster snapshot
//!   that the code graph writes into every member repository
//!   (`cognee_code_graph::cluster::cluster_output_dir`). It is removed for
//!   each `(row, dataset)` pair the delete detaches, once no row of that
//!   dataset still names the repository. A single-repository run's `.enola`
//!   is never touched.
//! - **The clone itself**, for a stamp with `repo_url` (cognee cloned it).
//!   It is removed once no `Data` row in any dataset names its `repo_path`.
//!   A stamp without `repo_url` is the user's own directory and is never
//!   removed.
//! - **The URL's other branch clones** (SDK-1709). Every branch of a URL
//!   pins to one row, keyed on the default-branch clone, so a re-add on
//!   another branch moves the row's `repo_path` to that branch's clone and
//!   no row names the clone it moved off. A delete that releases a clone
//!   therefore also removes the clones beside it of the same repository
//!   (`<slug>` and `<slug>@<branch>-<hash>`, see
//!   `cognee_ingestion::resolve_repo::default_branch_clone_dir`) that no row
//!   names. A re-add never removes a clone itself: a cognify or another add
//!   may still be reading the one the row moved off, so it stays until a
//!   delete releases the repository.
//!
//! Everything here is best effort and runs after the relational delete has
//! committed: a failure is logged and never fails the delete.
//!
//! Known limitation: the "no row references the clone" check and the removal
//! are not atomic with ingestion. An add of the same URL by the same caller
//! (any branch), running concurrently with the delete, may find the clone on disk
//! before this removes it: its git or file reads then fail (the add errors),
//! or, when it had already read the files, the add succeeds and leaves a
//! row whose `repo_path` no longer exists. Re-adding the URL re-clones it.
//! A per-clone lock shared with ingestion would close this window.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path};

use cognee_models::code_repo::SOURCE_CODE_REPO;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::DeleteService;

/// One detached `(row, dataset)` pair whose row is a code-repository stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodeRepoLink {
    pub(crate) dataset_id: Uuid,
    /// The stamp's `repo_path`, exactly as stamped.
    pub(crate) repo_path: String,
    /// The stamp has a `repo_url`: the directory is a cognee clone.
    pub(crate) cloned: bool,
}

/// `(repo_path, cloned)` of a code-repository stamp, or `None` for any
/// other `system_metadata`.
pub(crate) fn parse_code_repo_stamp(system_metadata: Option<&str>) -> Option<(String, bool)> {
    let stamp: serde_json::Value = serde_json::from_str(system_metadata?).ok()?;
    let stamp = stamp.as_object()?;
    if stamp.get("source").and_then(serde_json::Value::as_str) != Some(SOURCE_CODE_REPO) {
        return None;
    }
    let repo_path = stamp.get("repo_path")?.as_str()?.to_string();
    let cloned = stamp
        .get("repo_url")
        .and_then(serde_json::Value::as_str)
        .is_some();
    Some((repo_path, cloned))
}

/// The name of a dataset's cluster snapshot directory inside a member
/// repository — `cognee_code_graph::cluster::cluster_output_dir`, restated
/// because this crate does not depend on the code graph.
fn enola_dir_name(dataset_id: Uuid) -> String {
    format!(".enola-{dataset_id}")
}

/// What a removal did.
#[derive(Debug, PartialEq, Eq)]
enum Removal {
    Removed,
    /// Nothing there (already gone).
    Absent,
    /// Left in place for the given reason.
    Skipped(&'static str),
}

/// Remove `<repo_path>/.enola-<dataset_id>` when it is a real directory (a
/// symlink or a file of that name is left alone).
fn remove_enola_dir(repo_path: &Path, dataset_id: Uuid) -> io::Result<Removal> {
    let dir = repo_path.join(enola_dir_name(dataset_id));
    let meta = match std::fs::symlink_metadata(&dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(e) => return Err(e),
    };
    // `symlink_metadata` does not follow a link: a symlink is never a dir.
    if !meta.is_dir() {
        return Ok(Removal::Skipped("not a directory"));
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(Removal::Removed)
}

/// Remove the cognee clone at `clone_dir`, only when every guard holds: its
/// last component is a plain name (a clone slug: not `.`, `..` or a root,
/// no separator), it is a real directory (not a symlink), and it holds
/// `.git`.
fn remove_clone_dir(clone_dir: &Path) -> io::Result<Removal> {
    let Some(Component::Normal(name)) = clone_dir.components().next_back() else {
        return Ok(Removal::Skipped("not a clone slug"));
    };
    let name = name.to_string_lossy();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Ok(Removal::Skipped("not a clone slug"));
    }
    let meta = match std::fs::symlink_metadata(clone_dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        return Ok(Removal::Skipped("not a directory"));
    }
    match std::fs::symlink_metadata(clone_dir.join(".git")) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(Removal::Skipped("no .git inside"));
        }
        Err(e) => return Err(e),
    }
    std::fs::remove_dir_all(clone_dir)?;
    Ok(Removal::Removed)
}

/// The filesystem work for one repository path, decided by the caller.
#[derive(Debug)]
struct PathPlan {
    repo_path: String,
    enola_datasets: Vec<Uuid>,
    remove_clone: bool,
}

fn apply_plan(plan: &PathPlan) {
    let repo_path = Path::new(&plan.repo_path);
    for dataset_id in &plan.enola_datasets {
        match remove_enola_dir(repo_path, *dataset_id) {
            Ok(Removal::Removed) => info!(
                repo_path = %plan.repo_path,
                %dataset_id,
                "removed the dataset's code-graph snapshot directory"
            ),
            Ok(Removal::Absent) => {}
            Ok(Removal::Skipped(reason)) => warn!(
                repo_path = %plan.repo_path,
                %dataset_id,
                reason,
                "left the dataset's code-graph snapshot path in place"
            ),
            Err(error) => warn!(
                repo_path = %plan.repo_path,
                %dataset_id,
                %error,
                "could not remove the dataset's code-graph snapshot directory"
            ),
        }
    }
    if plan.remove_clone {
        match remove_clone_dir(repo_path) {
            Ok(Removal::Removed) => info!(
                repo_path = %plan.repo_path,
                "removed the repository clone no data item references any more"
            ),
            Ok(Removal::Absent) => {}
            Ok(Removal::Skipped(reason)) => warn!(
                repo_path = %plan.repo_path,
                reason,
                "left an unreferenced repository clone in place"
            ),
            Err(error) => warn!(
                repo_path = %plan.repo_path,
                %error,
                "could not remove the unreferenced repository clone"
            ),
        }
    }
}

/// The other clone directories of `clone`'s repository beside it: its
/// default-branch clone `<slug>` and every branch clone `<slug>@…`, as the
/// paths a stamp would name (`clone`'s parent joined with the entry name).
/// `clone` itself is not listed. Neither clone-name form puts an `@` in the
/// slug (`cognee_ingestion::resolve_repo::default_branch_clone_dir`,
/// restated because this crate does not depend on ingestion). A listing
/// failure yields none.
fn sibling_clones(clone: &Path) -> Vec<String> {
    let (Some(parent), Some(name)) = (clone.parent(), clone.file_name().and_then(|n| n.to_str()))
    else {
        return Vec::new();
    };
    let Some(slug) = name.split('@').next().filter(|slug| !slug.is_empty()) else {
        return Vec::new();
    };
    let branch_prefix = format!("{slug}@");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                warn!(parent = %parent.display(), %error, "could not list the clones directory");
            }
            return Vec::new();
        }
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|entry| entry != name && (entry == slug || entry.starts_with(&branch_prefix)))
        .map(|entry| parent.join(entry).to_string_lossy().into_owned())
        .collect()
}

impl DeleteService {
    /// The code-repository rows among `links` (`(dataset_id, data_id)`), read
    /// before the relational delete removes them. Best effort: a read failure
    /// is logged and yields fewer links (less cleanup, never a failed
    /// delete).
    pub(crate) async fn collect_code_repo_links(
        &self,
        links: &[(Uuid, Uuid)],
    ) -> Vec<CodeRepoLink> {
        let mut by_dataset: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
        for (dataset_id, data_id) in links {
            by_dataset.entry(*dataset_id).or_default().insert(*data_id);
        }
        let mut out = Vec::new();
        for (dataset_id, data_ids) in by_dataset {
            // One query per dataset for a dataset delete; a point lookup for
            // a single item.
            let rows = if data_ids.len() > 1 {
                match self.database.get_dataset_data(dataset_id).await {
                    Ok(rows) => rows
                        .into_iter()
                        .filter(|row| data_ids.contains(&row.id))
                        .collect::<Vec<_>>(),
                    Err(error) => {
                        warn!(%dataset_id, %error, "could not read rows for the code-repository cleanup");
                        continue;
                    }
                }
            } else {
                let mut rows = Vec::new();
                for data_id in &data_ids {
                    match self.database.get_data(*data_id).await {
                        Ok(Some(row)) => rows.push(row),
                        Ok(None) => {}
                        Err(error) => {
                            warn!(%data_id, %error, "could not read a row for the code-repository cleanup");
                        }
                    }
                }
                rows
            };
            for row in rows {
                if let Some((repo_path, cloned)) =
                    parse_code_repo_stamp(row.system_metadata.as_deref())
                {
                    out.push(CodeRepoLink {
                        dataset_id,
                        repo_path,
                        cloned,
                    });
                }
            }
        }
        out
    }

    /// After the relational delete committed: remove each detached pair's
    /// `.enola-<dataset_id>` once no row of that dataset names the
    /// repository, and each cognee clone no row of any dataset names (see
    /// the module docs). Best effort, logged.
    pub(crate) async fn cleanup_code_repos(&self, links: Vec<CodeRepoLink>) {
        if links.is_empty() {
            return;
        }
        let mut by_path: BTreeMap<String, (BTreeSet<Uuid>, bool)> = BTreeMap::new();
        for link in links {
            let entry = by_path.entry(link.repo_path).or_default();
            entry.0.insert(link.dataset_id);
            entry.1 |= link.cloned;
        }
        // The other branch clones of each released clone (SDK-1709), listed
        // before the loop below consumes `by_path`. Each is planned like a
        // released path of no dataset: removed only when no row names it.
        let released: Vec<String> = by_path
            .iter()
            .filter(|(_, (_, cloned))| *cloned)
            .map(|(repo_path, _)| repo_path.clone())
            .collect();
        let siblings = match tokio::task::spawn_blocking(move || {
            let mut siblings = BTreeSet::new();
            for repo_path in &released {
                siblings.extend(sibling_clones(Path::new(repo_path)));
            }
            siblings
        })
        .await
        {
            Ok(siblings) => siblings,
            Err(error) => {
                warn!(%error, "could not list the repository's other branch clones");
                BTreeSet::new()
            }
        };
        for sibling in siblings {
            by_path
                .entry(sibling)
                .or_insert_with(|| (BTreeSet::new(), true));
        }
        let mut plans = Vec::new();
        for (repo_path, (datasets, cloned)) in by_path {
            let references = match self
                .database
                .list_code_repo_path_references(&repo_path)
                .await
            {
                Ok(references) => references,
                Err(error) => {
                    warn!(
                        %repo_path,
                        %error,
                        "could not count the rows still naming a code repository; leaving it on disk"
                    );
                    continue;
                }
            };
            let still_in: BTreeSet<Uuid> = references.iter().flatten().copied().collect();
            let enola_datasets: Vec<Uuid> = datasets
                .into_iter()
                .filter(|dataset_id| !still_in.contains(dataset_id))
                .collect();
            // Only a stamp with `repo_url` marks a cognee clone; a user's own
            // directory is never removed, whatever references it.
            let remove_clone = cloned && references.is_empty();
            if !remove_clone && enola_datasets.is_empty() {
                debug!(%repo_path, "code repository still referenced; nothing to remove");
                continue;
            }
            plans.push(PathPlan {
                repo_path,
                enola_datasets,
                remove_clone,
            });
        }
        if plans.is_empty() {
            return;
        }
        // A clone can be large: keep the blocking removal off the runtime.
        let work = tokio::task::spawn_blocking(move || {
            for plan in &plans {
                apply_plan(plan);
            }
        });
        if let Err(error) = work.await {
            warn!(%error, "the code-repository cleanup task failed");
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_code_repo_stamps() {
        assert_eq!(
            parse_code_repo_stamp(Some(
                r#"{"source":"code_repo","repo_path":"/r","file_count":1,"repo_url":"https://x/y"}"#
            )),
            Some(("/r".to_string(), true))
        );
        assert_eq!(
            parse_code_repo_stamp(Some(r#"{"source":"code_repo","repo_path":"/r"}"#)),
            Some(("/r".to_string(), false))
        );
        assert_eq!(
            parse_code_repo_stamp(Some(r#"{"source":"code","repo_path":"/r"}"#)),
            None
        );
        assert_eq!(parse_code_repo_stamp(Some("not json")), None);
        assert_eq!(parse_code_repo_stamp(None), None);
    }

    #[test]
    fn clone_removal_requires_a_git_dir() {
        let root = tempfile::tempdir().unwrap();
        let clone = root.path().join("repo-slug");
        std::fs::create_dir(&clone).unwrap();
        assert_eq!(
            remove_clone_dir(&clone).unwrap(),
            Removal::Skipped("no .git inside")
        );
        assert!(clone.exists());
        std::fs::create_dir(clone.join(".git")).unwrap();
        assert_eq!(remove_clone_dir(&clone).unwrap(), Removal::Removed);
        assert!(!clone.exists());
        assert_eq!(remove_clone_dir(&clone).unwrap(), Removal::Absent);
    }

    #[test]
    fn clone_removal_refuses_non_slug_paths() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        let dotdot = root.path().join("sub").join("..");
        assert_eq!(
            remove_clone_dir(&dotdot).unwrap(),
            Removal::Skipped("not a clone slug")
        );
        assert_eq!(
            remove_clone_dir(Path::new("/")).unwrap(),
            Removal::Skipped("not a clone slug")
        );
        assert!(root.path().join(".git").exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir_all(real.join(".git")).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            remove_clone_dir(&link).unwrap(),
            Removal::Skipped("not a directory")
        );
        let dataset_id = Uuid::new_v4();
        let target = root.path().join("snapshot");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, real.join(enola_dir_name(dataset_id))).unwrap();
        assert_eq!(
            remove_enola_dir(&real, dataset_id).unwrap(),
            Removal::Skipped("not a directory")
        );
        assert!(real.exists() && target.exists());
    }
}
