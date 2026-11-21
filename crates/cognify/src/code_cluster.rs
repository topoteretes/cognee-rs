//! A dataset's code repositories as one enola cluster.
//!
//! Port of the relational half of Python's
//! `cognee/tasks/code_graph/cluster.py` (parity pin `a59433e6b`); the enola
//! run, the member labels and the member load are
//! [`cognee_code_graph::snapshot_dataset_code_repos`] and
//! [`cognee_code_graph::load_cluster_member`].
//!
//! A dataset is the cluster. When it holds more than one code-repo `Data`
//! row, enola runs once over all of them, and the snapshot is loaded one
//! member at a time, each under its own row, so deleting one row still
//! removes exactly one repository. A dataset with a single repository keeps
//! the per-repository run. The cluster's members are the dataset's code-repo
//! rows, whose `system_metadata.repo_path` names the directory; the config
//! enola needs is rendered from them at run time.
//!
//! The cluster is always refreshed as a whole: loading one member from an
//! older snapshot would keep cross-repository facts the current one no
//! longer holds. Three callers:
//!
//! - cognify's CODE_REPO route ([`load_dataset_cluster`]): the first repo
//!   item of a run loads every member;
//! - `remember(content_type="code")` ([`plan_dataset_refresh`], then
//!   [`refresh_dataset_code_graph`]): the dataset's other repositories are
//!   rebuilt with the new ones, up to
//!   [`CLUSTER_INLINE_REBUILD_LIMIT`](cognee_code_graph::CLUSTER_INLINE_REBUILD_LIMIT);
//! - a delete ([`mark_dataset_code_repos_for_rebuild`]): the remaining
//!   repositories are marked for the next cognify.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cognee_code_graph::{
    CLUSTER_INLINE_REBUILD_LIMIT, CodeGraphConfig, CodeGraphError, DatasetCodeSnapshot,
    ParsedSnapshot, snapshot_dataset_code_repos,
};
use cognee_core::CpuPool;
use cognee_database::{PipelineRunRepository, RelationalDb};
use cognee_graph::GraphDBTrait;
use cognee_models::Data;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::routing::{CognifyRoute, cognify_route_for};
use crate::tasks::{
    CODE_GRAPH_PIPELINE_NAME, CodeRepoLoadMode, CognifyInput, code_graph_ledger,
    run_code_graph_pipeline,
};

/// The `pipeline_status` entries a built repository is stamped with, and a
/// repository marked for rebuild loses (Python `pipeline_names=
/// ("cognify_pipeline", "code_graph_pipeline")`).
const REBUILD_PIPELINE_NAMES: &[&str] = &["cognify_pipeline", CODE_GRAPH_PIPELINE_NAME];

/// One member of a cluster snapshot, as a code graph pipeline run loads it
/// ([`CodeRepoLoadMode::Member`]).
#[derive(Debug, Clone)]
pub struct ClusterMemberLoad {
    /// The cluster snapshot, parsed once for every member.
    pub snapshot: Arc<ParsedSnapshot>,
    /// The member's directory.
    pub repo_path: PathBuf,
    /// The label the member's facts carry.
    pub label: String,
}

/// The repository directory a code-repo row points at
/// (`system_metadata.repo_path`), or `None`. Python's `code_repo_path`.
pub fn code_repo_path(data: &Data) -> Option<PathBuf> {
    let metadata: serde_json::Value =
        serde_json::from_str(data.system_metadata.as_deref()?).ok()?;
    match metadata.get("repo_path")? {
        serde_json::Value::String(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => None,
    }
}

/// Every code-repo `Data` row of a dataset. Python's `dataset_code_repo_rows`.
///
/// # Errors
///
/// [`CognifyError::DatabaseError`] when the rows cannot be read.
pub async fn dataset_code_repo_rows(
    db: &dyn RelationalDb,
    dataset_id: Uuid,
) -> Result<Vec<Data>, CognifyError> {
    let rows = cognee_database::DeleteDb::get_dataset_data(db, dataset_id)
        .await
        .map_err(|e| CognifyError::DatabaseError(e.to_string()))?;
    Ok(rows
        .into_iter()
        .filter(|row| matches!(cognify_route_for(row), Ok(CognifyRoute::CodeRepo)))
        .collect())
}

/// The dataset's code-repo rows with their directories (rows without one are
/// left out).
async fn dataset_code_repos(
    db: &dyn RelationalDb,
    dataset_id: Uuid,
) -> Result<Vec<(Data, PathBuf)>, CognifyError> {
    Ok(dataset_code_repo_rows(db, dataset_id)
        .await?
        .into_iter()
        .filter_map(|row| code_repo_path(&row).map(|path| (row, path)))
        .collect())
}

/// What the cluster load of one cognify run decided, kept for the run's
/// later repo items. Python's `_ClusterRun` (here one per
/// [`run_code_routes`](crate::tasks::run_code_routes) call, which is one
/// pipeline run): never the snapshot, only the verdict.
#[derive(Debug, Default)]
pub(crate) enum ClusterVerdict {
    /// Nothing loaded yet in this run.
    #[default]
    Pending,
    /// Loaded: whether the dataset was a cluster.
    Done(bool),
    /// enola or a member load failed; the run's later repo items fail the
    /// same way instead of loading again.
    Failed(CognifyError),
}

/// The same failure again, for a later repo item of the run.
fn replay(error: &CognifyError) -> CognifyError {
    match error {
        CognifyError::CodeGraph(error) => CognifyError::CodeGraph(match error {
            CodeGraphError::EnolaNotInstalled(message) => {
                CodeGraphError::EnolaNotInstalled(message.clone())
            }
            CodeGraphError::EnolaSnapshot(message) => {
                CodeGraphError::EnolaSnapshot(message.clone())
            }
            CodeGraphError::InvalidConfig(message) => {
                CodeGraphError::InvalidConfig(message.clone())
            }
            CodeGraphError::InvalidRepoItem(message) => {
                CodeGraphError::InvalidRepoItem(message.clone())
            }
            CodeGraphError::Graph(error) => {
                return CognifyError::GraphDatabaseError(error.to_string());
            }
            CodeGraphError::Ledger(error) => {
                return CognifyError::DatabaseError(error.to_string());
            }
        }),
        CognifyError::Route(error) => CognifyError::Route(error.clone()),
        CognifyError::DatabaseError(message) => CognifyError::DatabaseError(message.clone()),
        CognifyError::StorageError(message) => CognifyError::StorageError(message.clone()),
        other => CognifyError::Execute(other.to_string()),
    }
}

/// Load the whole cluster of the dataset a CODE_REPO item belongs to.
/// Python's `load_dataset_cluster`.
///
/// Returns `false` when the dataset is not a cluster (fewer than two
/// repositories whose directory exists, or members that cannot be told
/// apart), and the caller then loads its one repository on its own.
/// Otherwise every member is loaded from one cluster snapshot, one at a
/// time, each under its own row (its own ledger owner and node set), and
/// `true` is returned: for the item that did the loading and for the run's
/// other repo items, which have nothing left to do. Members outside the run
/// (already built) are loaded too, as the whole cluster is refreshed.
///
/// # Errors
///
/// The enola or member-load failure, for the item that hit it and again for
/// every later item of the run.
pub(crate) async fn load_dataset_cluster(
    verdict: &mut ClusterVerdict,
    input: &CognifyInput,
    graph_db: &dyn GraphDBTrait,
    db: &dyn RelationalDb,
    code_graph: &CodeGraphConfig,
    pipeline_run_id: Option<Uuid>,
) -> Result<bool, CognifyError> {
    match verdict {
        ClusterVerdict::Done(is_cluster) => return Ok(*is_cluster),
        ClusterVerdict::Failed(error) => return Err(replay(error)),
        ClusterVerdict::Pending => {}
    }
    match load_cluster(input, graph_db, db, code_graph, pipeline_run_id).await {
        Ok(is_cluster) => {
            *verdict = ClusterVerdict::Done(is_cluster);
            Ok(is_cluster)
        }
        Err(error) => {
            let again = replay(&error);
            *verdict = ClusterVerdict::Failed(error);
            Err(again)
        }
    }
}

/// [`load_dataset_cluster`]'s one load. Python's `snapshot_dataset_code_repos`
/// then `_load_cluster_members`.
async fn load_cluster(
    input: &CognifyInput,
    graph_db: &dyn GraphDBTrait,
    db: &dyn RelationalDb,
    code_graph: &CodeGraphConfig,
    pipeline_run_id: Option<Uuid>,
) -> Result<bool, CognifyError> {
    let repos = dataset_code_repos(db, input.dataset_id).await?;
    let Some(snapshot) = snapshot_dataset_code_repos(code_graph, input.dataset_id, repos).await?
    else {
        return Ok(false);
    };
    for member in &snapshot.members {
        // Each row's own add() tag, read as the per-repository route reads
        // it, so a member's code nodes carry its node set.
        let node_set = crate::routing::code_repo_node_set(&member.data)?;
        let ledger = code_graph_ledger(db, input, member.data.id, pipeline_run_id);
        cognee_code_graph::load_cluster_member(
            graph_db,
            Some(&ledger),
            &snapshot.parsed,
            &member.repo_path,
            &member.label,
            node_set.as_deref(),
        )
        .await?;
        info!(
            "Code repo graph extracted for {} ({}).",
            member.repo_path.display(),
            member.label
        );
    }
    Ok(true)
}

/// What rebuilding a dataset's code graph for new repositories will do.
/// Python's `RefreshPlan`.
///
/// Decided, and the enola run taken, under the dataset lock before the new
/// repositories' rows are stored. Members planned for a new repository carry
/// no row yet (`data` is `None`); [`refresh_dataset_code_graph`] binds them
/// to the rows the caller stored, by directory.
#[derive(Debug, Default)]
pub struct RefreshPlan {
    /// The cluster snapshot, or `None` when the repositories are not a
    /// cluster.
    pub snapshot: Option<DatasetCodeSnapshot<Option<Data>>>,
    /// The dataset's other rows rebuilt inline.
    pub existing: Vec<(Data, PathBuf)>,
    /// The dataset's other rows left to the next `cognify()`, because the
    /// dataset is large.
    pub deferred: Vec<Data>,
}

/// Take the cluster snapshot for adding `new_repo_paths` to a dataset.
/// Python's `plan_dataset_refresh`.
///
/// The dataset's other code-repo rows join the run (so the new repositories
/// get their edges to them) up to
/// [`CLUSTER_INLINE_REBUILD_LIMIT`](cognee_code_graph::CLUSTER_INLINE_REBUILD_LIMIT);
/// beyond it they are deferred to the next `cognify()` of the dataset, which
/// rebuilds the whole cluster. A row whose directory is one of the new paths
/// is the same repository remembered again, and counts as new. The new
/// repositories come first in the cluster.
///
/// # Errors
///
/// The rows cannot be read, or enola fails on the cluster.
pub async fn plan_dataset_refresh(
    db: &dyn RelationalDb,
    code_graph: &CodeGraphConfig,
    dataset_id: Uuid,
    new_repo_paths: &[PathBuf],
) -> Result<RefreshPlan, CognifyError> {
    let is_new = |path: &Path| new_repo_paths.iter().any(|new| new.as_path() == path);
    let mut existing: Vec<(Data, PathBuf)> = dataset_code_repos(db, dataset_id)
        .await?
        .into_iter()
        .filter(|(_, path)| !is_new(path))
        .collect();
    let mut deferred = Vec::new();
    if !new_repo_paths.is_empty() && existing.len() > CLUSTER_INLINE_REBUILD_LIMIT {
        info!(
            "Dataset {dataset_id} holds {} other code repositories (limit \
             {CLUSTER_INLINE_REBUILD_LIMIT} for an inline rebuild): building the {} new one(s) \
             now and marking the rest for the next cognify().",
            existing.len(),
            new_repo_paths.len()
        );
        deferred = existing.drain(..).map(|(row, _)| row).collect();
    }

    let repos: Vec<(Option<Data>, PathBuf)> = new_repo_paths
        .iter()
        .map(|path| (None, path.clone()))
        .chain(
            existing
                .iter()
                .map(|(row, path)| (Some(row.clone()), path.clone())),
        )
        .collect();
    let snapshot = snapshot_dataset_code_repos(code_graph, dataset_id, repos).await?;
    Ok(RefreshPlan {
        snapshot,
        existing,
        deferred,
    })
}

/// Where and as whom a dataset's code graph pipeline runs: everything
/// [`run_code_graph_pipeline`] takes besides its rows and load mode.
#[derive(Clone)]
pub struct CodeGraphRunTarget {
    pub dataset_id: Uuid,
    pub user_id: Option<Uuid>,
    pub user_email: Option<String>,
    pub tenant_id: Option<Uuid>,
    pub storage: Arc<dyn StorageTrait>,
    pub graph_db: Arc<dyn GraphDBTrait>,
    pub vector_db: Arc<dyn VectorDB>,
    pub database: Arc<dyn RelationalDb>,
    pub pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    pub thread_pool: Arc<dyn CpuPool>,
    pub config: Arc<CognifyConfig>,
}

/// What refreshing one repository of a dataset produced. Python's
/// `MemberRun`.
#[derive(Debug)]
pub struct MemberRun {
    /// The repository's row.
    pub data: Data,
    /// Its directory.
    pub repo_path: PathBuf,
    /// Its cluster label, `None` outside a cluster.
    pub label: Option<String>,
    /// The `code_graph_pipeline` run's id, or why it failed.
    pub result: Result<Uuid, CognifyError>,
}

/// Rebuild the code graph of every repository in a dataset. Python's
/// `refresh_dataset_code_graph` (graph-only: `index_vectors` comes later).
///
/// One `code_graph_pipeline` run per repository, over that repository's
/// row, so each row owns the nodes of its own repository; with several
/// repositories they all read one cluster snapshot. `known_repos` are rows
/// the caller just stored, with their directories; `plan` is the snapshot
/// already taken for them ([`plan_dataset_refresh`], taken here when `None`),
/// whose members are bound to these rows by directory. The dataset's other
/// rows come from the plan; its deferred rows are marked for the next
/// `cognify()` ([`mark_code_repos_for_rebuild`]).
///
/// A repository whose run fails is reported on its [`MemberRun`] and does
/// not stop the others; each successful run stamps its row complete for
/// `cognify_pipeline` and `code_graph_pipeline`. A planned member without a
/// row (its add failed) is not loaded, and outside a cluster a row whose
/// directory is gone is left out, both with a warning.
///
/// # Errors
///
/// Only when the plan has to be taken here and that fails (an enola failure
/// on the cluster).
pub async fn refresh_dataset_code_graph(
    target: &CodeGraphRunTarget,
    known_repos: Vec<(Data, PathBuf)>,
    plan: Option<RefreshPlan>,
) -> Result<Vec<MemberRun>, CognifyError> {
    let plan = match plan {
        Some(plan) => plan,
        None => {
            let paths: Vec<PathBuf> = known_repos.iter().map(|(_, path)| path.clone()).collect();
            plan_dataset_refresh(
                &*target.database,
                &target.config.code_graph,
                target.dataset_id,
                &paths,
            )
            .await?
        }
    };
    if !plan.deferred.is_empty() {
        mark_code_repos_for_rebuild(&*target.database, target.dataset_id, &plan.deferred).await;
    }

    let mut runs: Vec<(MemberRun, CodeRepoLoadMode)> = Vec::new();
    if let Some(snapshot) = plan.snapshot {
        for member in snapshot.members {
            let data = member.data.or_else(|| {
                known_repos
                    .iter()
                    .find(|(_, path)| *path == member.repo_path)
                    .map(|(row, _)| row.clone())
            });
            let Some(data) = data else {
                // Planned as new, but the caller stored no row for it: nothing
                // owns its nodes, so it is not loaded.
                warn!(
                    "Code repository '{}' has no Data row in dataset {}; not loading it.",
                    member.repo_path.display(),
                    target.dataset_id
                );
                continue;
            };
            let load = ClusterMemberLoad {
                snapshot: Arc::clone(&snapshot.parsed),
                repo_path: member.repo_path.clone(),
                label: member.label.clone(),
            };
            runs.push((
                MemberRun {
                    data,
                    repo_path: member.repo_path,
                    label: Some(member.label),
                    result: Err(CognifyError::Execute("not run".to_string())),
                },
                CodeRepoLoadMode::Member(load),
            ));
        }
    } else {
        // Not a cluster: each repository on its own. A row whose directory
        // is gone is left out, so one stale row does not fail every later
        // remember() into the dataset.
        for (data, repo_path) in known_repos.into_iter().chain(plan.existing) {
            if !repo_path.is_dir() {
                warn!(
                    "Code repository '{}' no longer exists; not rebuilding it.",
                    repo_path.display()
                );
                continue;
            }
            runs.push((
                MemberRun {
                    data,
                    repo_path,
                    label: None,
                    result: Err(CognifyError::Execute("not run".to_string())),
                },
                CodeRepoLoadMode::Single,
            ));
        }
    }

    let mut finished = Vec::with_capacity(runs.len());
    for (mut run, mode) in runs {
        run.result = Box::pin(run_code_graph_pipeline(
            vec![run.data.clone()],
            target.dataset_id,
            target.user_id,
            target.user_email.clone(),
            target.tenant_id,
            Arc::clone(&target.storage),
            Arc::clone(&target.graph_db),
            Arc::clone(&target.vector_db),
            Arc::clone(&target.database),
            Arc::clone(&target.pipeline_run_repo),
            Arc::clone(&target.thread_pool),
            &target.config,
            mode,
        ))
        .await;
        if let Err(error) = &run.result {
            warn!(
                "Code-graph run failed for '{}': {error}",
                run.repo_path.display()
            );
        }
        finished.push(run);
    }
    Ok(finished)
}

/// Drop these code-repo rows' completion stamps (`cognify_pipeline` and
/// `code_graph_pipeline`) so the next `cognify()` rebuilds them. Python's
/// `_mark_rows_for_rebuild`. A row that cannot be marked is logged: it costs
/// only a stale cross-repository edge until the dataset is cognified.
pub async fn mark_code_repos_for_rebuild(db: &dyn RelationalDb, dataset_id: Uuid, rows: &[Data]) {
    for row in rows {
        if let Err(error) = cognee_database::ProvenanceDb::clear_pipeline_status_for_data(
            db,
            row.id,
            dataset_id,
            REBUILD_PIPELINE_NAMES,
        )
        .await
        {
            warn!(
                data_id = %row.id,
                dataset_id = %dataset_id,
                "Could not mark the code repository for re-extraction: {error}"
            );
        }
    }
}

/// Mark the remaining repositories of a dataset for re-extraction after one
/// was deleted. Python's `mark_dataset_code_repos_for_rebuild`.
///
/// The deleted repository's nodes are gone, and its edges with them. What
/// is left are the other members' facts about it, which only a fresh
/// snapshot without it corrects. That snapshot is not taken here (a delete
/// must not turn into an enola run over the whole dataset) but on the next
/// `cognify()`: the survivors' completion stamps are dropped so cognify
/// processes them again. Never fails: the delete has succeeded.
///
/// This SDK's data-item delete already clears every row's `pipeline_status`
/// entries for the dataset (`cognee-delete`), which covers this; the
/// function is for callers that delete otherwise.
pub async fn mark_dataset_code_repos_for_rebuild(db: &dyn RelationalDb, dataset_id: Uuid) {
    let rows = match dataset_code_repo_rows(db, dataset_id).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(
                "Could not mark the code repositories of dataset {dataset_id} for re-extraction \
                 after a repository was deleted ({error}); cognify() the dataset to rebuild \
                 their graph."
            );
            return;
        }
    };
    mark_code_repos_for_rebuild(db, dataset_id, &rows).await;
    if !rows.is_empty() {
        info!(
            "Marked {} code repositor{} of dataset {dataset_id} for re-extraction on the next \
             cognify().",
            rows.len(),
            if rows.len() == 1 { "y" } else { "ies" }
        );
    }
}
