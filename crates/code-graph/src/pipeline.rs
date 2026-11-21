//! The code graph load for one local repository: extract, write nodes,
//! write edges.
//!
//! Port of Python's `extract_code_graph` and `get_code_graph_tasks`
//! (`cognee/tasks/code_graph/extract_code_graph.py`). Python returns three
//! pipeline tasks; here they are three functions run in order by
//! [`load_code_graph`], which the cognify route calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use cognee_graph::GraphDBTrait;
use serde_json::{Map, Value};

use crate::config::CodeGraphConfig;
use crate::edges::{CodeGraphEdge, build_code_graph_edges};
use crate::error::{CodeGraphError, CodeGraphResult};
use crate::mapping::{CODE_GRAPH_MAPPING_VERSION, map_facts_to_data_points, resolve_fallback_repo};
use crate::models::CodeNode;
use crate::runner::run_enola_generate;
use crate::snapshot::ParsedSnapshot;
use crate::writer::{
    CodeGraphLedger, CodeGraphLoadState, StoredRepositoryState, add_code_graph_data_points,
    add_code_graph_edges, member_edges, record_snapshot_in_ledger, stored_repository_state,
    write_code_graph_edges,
};

/// One async lock per repository directory, so two loads of the same
/// repository in this process do not run enola into the same `.enola/` at
/// once or read a snapshot another run is rewriting. Entries hold the lock
/// weakly and are dropped once nobody holds or waits for it.
///
/// In-process only: two processes loading the same repository can still
/// race on `.enola/` (cross-process locking is out of scope).
static REPO_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Take the [`REPO_LOCKS`] lock of `repo_path`, keyed by its canonical path
/// (the absolute one when it cannot be canonicalised), so two spellings of
/// one directory share a lock.
async fn lock_repository(repo_path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
    let key = std::fs::canonicalize(repo_path)
        .or_else(|_| std::path::absolute(repo_path))
        .unwrap_or_else(|_| repo_path.to_path_buf());
    let lock = {
        // A panic while holding this map cannot leave it inconsistent (one
        // insert or prune at a time), so a poisoned lock is still usable.
        let mut locks = REPO_LOCKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.retain(|_, lock| lock.strong_count() > 0);
        match locks.get(&key).and_then(Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(key, Arc::downgrade(&lock));
                lock
            }
        }
    };
    lock.lock_owned().await
}

/// A repository's code graph, extracted: the snapshot it was read from and
/// the nodes it maps to.
#[derive(Debug, Clone)]
pub struct ExtractedCodeGraph {
    /// The parsed snapshot (`<repo>/.enola`, or the one given). Its
    /// [`edges`](ParsedSnapshot::edges) are resolved unless the load was
    /// skipped.
    pub snapshot: ParsedSnapshot,
    /// The mapped nodes, repositories first. Empty when the load was skipped.
    pub data_points: Vec<CodeNode>,
    /// The graph already holds this snapshot (see [`extract_code_graph`]):
    /// nothing was mapped and nothing is to be written.
    pub skipped: bool,
}

/// Run enola on `repo_path` (or read an existing `snapshot_dir`) and map the
/// snapshot's facts to code nodes. Python's `extract_code_graph`.
///
/// `node_set` tags every node (see
/// [`map_facts_to_data_points`](crate::map_facts_to_data_points)).
///
/// An unchanged snapshot is skipped: when the repository node in `graph`
/// already carries this snapshot's id and the current mapping version, the
/// snapshot is not mapped and [`ExtractedCodeGraph::skipped`] is set. Only an
/// untagged repository is skipped: a tagged call, or a repository whose node
/// still carries a tag, always loads, so a tag change (or a run that crashed
/// mid-retag) is never skipped past. A failed read of the stored stamp is
/// logged and the snapshot loaded in full: the skip is an optimisation.
///
/// The enola run and the snapshot read hold a per-repository lock, so
/// concurrent loads of one repository in this process take turns (see
/// `REPO_LOCKS`).
///
/// # Errors
///
/// What [`run_enola_generate`] and
/// [`parse_enola_snapshot`](crate::parse_enola_snapshot) raise.
pub async fn extract_code_graph(
    graph: &dyn GraphDBTrait,
    config: &CodeGraphConfig,
    repo_path: &Path,
    snapshot_dir: Option<&Path>,
    node_set: Option<&[String]>,
) -> CodeGraphResult<ExtractedCodeGraph> {
    let guard = lock_repository(repo_path).await;
    let snapshot_dir = match snapshot_dir {
        Some(dir) => dir.to_path_buf(),
        None => run_enola_generate(config, repo_path).await?,
    };
    // Parsing is file work: off the async runtime.
    let snapshot = blocking(move || read_snapshot(&snapshot_dir)).await?;
    drop(guard);

    let repo_path_text = repo_path.to_string_lossy().into_owned();
    let fallback_repo = resolve_fallback_repo(&snapshot.facts, Some(&repo_path_text));
    if already_loaded(graph, &snapshot, &fallback_repo, node_set).await {
        return Ok(ExtractedCodeGraph {
            snapshot,
            data_points: Vec::new(),
            skipped: true,
        });
    }

    // Mapping and edge resolution are CPU work: off the async runtime.
    let node_set = node_set.map(<[String]>::to_vec);
    let (snapshot, data_points) = blocking(move || {
        let mut snapshot = snapshot;
        let (data_points, edges) = map_snapshot(&snapshot, &repo_path_text, node_set.as_deref());
        snapshot.edges = Some(edges);
        Ok((snapshot, data_points))
    })
    .await?;
    Ok(ExtractedCodeGraph {
        snapshot,
        data_points,
        skipped: false,
    })
}

/// Whether the graph already holds this snapshot for `repo`: its repository
/// node carries the snapshot's id and the current mapping version, and
/// neither the call nor the node is tagged. Python's skip check in
/// `extract_code_graph`. A failed read of the stamp is logged and answers no:
/// the skip is an optimisation.
async fn already_loaded(
    graph: &dyn GraphDBTrait,
    snapshot: &ParsedSnapshot,
    repo: &str,
    node_set: Option<&[String]>,
) -> bool {
    let Some(snapshot_id) = snapshot.identity() else {
        return false;
    };
    let stored = match stored_repository_state(graph, repo).await {
        Ok(stored) => stored,
        Err(error) => {
            tracing::warn!("Could not read the stored snapshot id ({error}); loading fully.");
            StoredRepositoryState::default()
        }
    };
    let tagged_call = node_set.is_some_and(|names| !names.is_empty());
    let skip = stored.snapshot_id.as_deref() == Some(snapshot_id.as_str())
        && stored.mapping_version == Some(CODE_GRAPH_MAPPING_VERSION)
        && !tagged_call
        && !stored.tagged;
    if skip {
        tracing::info!(
            "Code graph for '{repo}' already matches snapshot {snapshot_id}; skipping load."
        );
    }
    skip
}

/// Run `work` on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> CodeGraphResult<T> + Send + 'static,
) -> CodeGraphResult<T> {
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        CodeGraphError::EnolaSnapshot(format!("reading the enola snapshot failed: {error}"))
    })?
}

/// Read the snapshot and log its provenance. Blocking.
fn read_snapshot(snapshot_dir: &Path) -> CodeGraphResult<ParsedSnapshot> {
    let snapshot = ParsedSnapshot::read(snapshot_dir)?;
    if let Some(receipt) = snapshot.receipt.as_ref() {
        let field = |key: &str| {
            receipt
                .get(key)
                .map_or("None".to_string(), crate::pyjson::py_str)
        };
        tracing::info!(
            "enola snapshot provenance: version={} format_version={} snapshot_id={} facts={}",
            field("enola_version"),
            field("format_version"),
            field("snapshot_id"),
            field("fact_count")
        );
    }
    Ok(snapshot)
}

/// Map the snapshot's facts to code nodes and resolve its typed edges.
/// Blocking.
fn map_snapshot(
    snapshot: &ParsedSnapshot,
    repo_path_text: &str,
    node_set: Option<&[String]>,
) -> (Vec<CodeNode>, Vec<CodeGraphEdge>) {
    let data_points =
        map_facts_to_data_points(&snapshot.facts, Some(repo_path_text), None, node_set);
    tracing::info!(
        "Mapped {} enola fact(s) to {} data point(s).",
        snapshot.facts.len(),
        data_points.len()
    );
    let (edges, skipped) = build_code_graph_edges(&snapshot.facts, Some(repo_path_text));
    tracing::info!(
        "Resolved {} code graph edge(s), skipped {}.",
        edges.len(),
        skipped
    );
    (data_points, edges)
}

/// What one load did.
#[derive(Debug, Clone)]
pub struct CodeGraphLoad {
    /// The node write's result (the data points and the pre-write graph);
    /// empty when the load was skipped.
    pub state: CodeGraphLoadState,
    /// The delta stamped on the repository node (`last_delta`), `None` when
    /// nothing was stamped (a skipped load included).
    pub delta: Option<Map<String, Value>>,
    /// The graph already held this snapshot and nothing was written (see
    /// [`extract_code_graph`]).
    pub skipped: bool,
}

/// Load the code graph of the repository at `repo_path` into `graph`: run
/// enola, map its snapshot, write the nodes, then the typed edges, sweep and
/// detag what the snapshot no longer derives, then stamp the repository.
/// Python's `get_code_graph_tasks` run as a pipeline, graph-only
/// (`index_vectors=False`, the opt-in comes later).
///
/// With a `ledger`, every node and edge of the snapshot (written or already
/// present) is recorded under its owner (the repository's `Data` row), so
/// deleting the row finds them and deleting another owner keeps them. A
/// skipped load records them too, without writing the graph: the owner
/// may differ from the one that loaded the snapshot (another dataset over the
/// same graph), and without its rows deleting that earlier owner would take
/// the graph this one relies on. Python records nothing on a skip.
///
/// # Errors
///
/// What [`extract_code_graph`], [`add_code_graph_data_points`] and
/// [`add_code_graph_edges`] raise.
pub async fn load_code_graph(
    graph: &dyn GraphDBTrait,
    ledger: Option<&CodeGraphLedger<'_>>,
    config: &CodeGraphConfig,
    repo_path: &Path,
    node_set: Option<&[String]>,
) -> CodeGraphResult<CodeGraphLoad> {
    let ExtractedCodeGraph {
        snapshot,
        data_points,
        skipped,
    } = extract_code_graph(graph, config, repo_path, None, node_set).await?;
    let repo_path_text = repo_path.to_string_lossy().into_owned();
    if skipped {
        if let Some(ledger) = ledger {
            let node_set = node_set.map(<[String]>::to_vec);
            let path = repo_path_text.clone();
            let (data_points, edges) =
                blocking(move || Ok(map_snapshot(&snapshot, &path, node_set.as_deref()))).await?;
            record_snapshot_in_ledger(ledger, &data_points, &edges).await?;
        }
        return Ok(CodeGraphLoad {
            state: CodeGraphLoadState::default(),
            delta: None,
            skipped: true,
        });
    }
    let state = add_code_graph_data_points(graph, ledger, data_points).await?;
    let delta = add_code_graph_edges(
        graph,
        ledger,
        &state,
        &snapshot,
        Some(&repo_path_text),
        node_set,
    )
    .await?;
    Ok(CodeGraphLoad {
        state,
        delta,
        skipped: false,
    })
}

/// Load one member of a multi-repo snapshot (a dataset's enola cluster,
/// [`crate::snapshot_dataset_code_repos`]) into `graph`, under its own
/// `ledger` owner. Python's `get_code_graph_tasks(repo_path, snapshot=...,
/// repo_scope=label)` run as a pipeline, graph-only.
///
/// `snapshot` is the cluster's, parsed once for every member (its
/// [`edges`](ParsedSnapshot::edges) resolved once too); `repo_path` is the
/// member's directory and `repo_scope` the label its facts carry. Only the
/// member's facts are mapped, its repository node read for the
/// unchanged-snapshot skip and stamped, and only the edges touching it whose
/// other end is already in the graph are written (an edge into a member that
/// loads later is written by that member's load). The stale sweep covers the
/// whole snapshot, as Python's does. Members load one at a time.
///
/// A skipped member records its nodes and edges in the ledger without
/// writing the graph, as [`load_code_graph`] does.
///
/// # Errors
///
/// A graph read or write, or the ledger write, failed.
pub async fn load_cluster_member(
    graph: &dyn GraphDBTrait,
    ledger: Option<&CodeGraphLedger<'_>>,
    snapshot: &Arc<ParsedSnapshot>,
    repo_path: &Path,
    repo_scope: &str,
    node_set: Option<&[String]>,
) -> CodeGraphResult<CodeGraphLoad> {
    let repo_path_text = repo_path.to_string_lossy().into_owned();
    let skipped = already_loaded(graph, snapshot, repo_scope, node_set).await;
    if skipped && ledger.is_none() {
        return Ok(CodeGraphLoad {
            state: CodeGraphLoadState::default(),
            delta: None,
            skipped: true,
        });
    }

    // Mapping is CPU work: off the async runtime.
    let data_points = {
        let snapshot = Arc::clone(snapshot);
        let path = repo_path_text.clone();
        let scope = repo_scope.to_string();
        let node_set = node_set.map(<[String]>::to_vec);
        blocking(move || {
            let data_points = map_facts_to_data_points(
                &snapshot.facts,
                Some(&path),
                Some(&scope),
                node_set.as_deref(),
            );
            tracing::info!(
                "Mapped {} enola fact(s) to {} data point(s) for '{scope}'.",
                snapshot.facts.len(),
                data_points.len()
            );
            Ok(data_points)
        })
        .await?
    };

    if skipped {
        if let Some(ledger) = ledger {
            let resolved;
            let edges: &[CodeGraphEdge] = match snapshot.edges.as_deref() {
                Some(edges) => edges,
                None => {
                    resolved = build_code_graph_edges(&snapshot.facts, Some(&repo_path_text)).0;
                    &resolved
                }
            };
            let scope_ids: std::collections::HashSet<String> = data_points
                .iter()
                .map(|point| point.id().to_string())
                .collect();
            let edges: Vec<CodeGraphEdge> = member_edges(graph, edges, &scope_ids)
                .await?
                .into_iter()
                .cloned()
                .collect();
            record_snapshot_in_ledger(ledger, &data_points, &edges).await?;
        }
        return Ok(CodeGraphLoad {
            state: CodeGraphLoadState::default(),
            delta: None,
            skipped: true,
        });
    }

    let state = add_code_graph_data_points(graph, ledger, data_points).await?;
    let delta = write_code_graph_edges(
        graph,
        ledger,
        &state,
        snapshot,
        Some(&repo_path_text),
        Some(repo_scope),
        node_set,
    )
    .await?;
    Ok(CodeGraphLoad {
        state,
        delta,
        skipped: false,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_repository_is_locked_by_its_canonical_path() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let held = lock_repository(dir.path()).await;
        // Another spelling of the same directory waits.
        let spelled = dir.path().join("sub/..");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let waiting = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            lock_repository(&spelled),
        )
        .await;
        assert!(waiting.is_err(), "the same repository must wait");
        // A different repository does not.
        let _other = lock_repository(other.path()).await;
        drop(held);
        let _again =
            tokio::time::timeout(std::time::Duration::from_secs(5), lock_repository(&spelled))
                .await
                .unwrap();
    }
}
