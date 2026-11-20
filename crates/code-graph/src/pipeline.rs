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
use crate::edges::build_code_graph_edges;
use crate::error::{CodeGraphError, CodeGraphResult};
use crate::mapping::map_facts_to_data_points;
use crate::models::CodeNode;
use crate::runner::run_enola_generate;
use crate::snapshot::ParsedSnapshot;
use crate::writer::{
    CodeGraphLedger, CodeGraphLoadState, add_code_graph_data_points, add_code_graph_edges,
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
    /// The parsed snapshot (`<repo>/.enola`, or the one given).
    pub snapshot: ParsedSnapshot,
    /// The mapped nodes, repositories first.
    pub data_points: Vec<CodeNode>,
}

/// Run enola on `repo_path` (or read an existing `snapshot_dir`) and map the
/// snapshot's facts to code nodes. Python's `extract_code_graph`.
///
/// `node_set` tags every node (see
/// [`map_facts_to_data_points`](crate::map_facts_to_data_points)).
///
/// The unchanged-snapshot skip Python does here comes with SDK-1053 O6; this
/// always maps the whole snapshot.
///
/// The enola run and the snapshot read hold a per-repository lock, so
/// concurrent loads of one repository in this process take turns (see
/// [`REPO_LOCKS`]).
///
/// # Errors
///
/// What [`run_enola_generate`] and
/// [`parse_enola_snapshot`](crate::parse_enola_snapshot) raise.
pub async fn extract_code_graph(
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
    // Parsing, mapping and edge resolution are CPU and file work: off the
    // async runtime.
    let repo_path_text = repo_path.to_string_lossy().into_owned();
    let node_set = node_set.map(<[String]>::to_vec);
    let (snapshot, data_points) = tokio::task::spawn_blocking(move || {
        parse_and_map(&snapshot_dir, &repo_path_text, node_set.as_deref())
    })
    .await
    .map_err(|error| {
        CodeGraphError::EnolaSnapshot(format!("reading the enola snapshot failed: {error}"))
    })??;
    drop(guard);
    Ok(ExtractedCodeGraph {
        snapshot,
        data_points,
    })
}

/// Read the snapshot, map its facts to code nodes and resolve its typed
/// edges (stored on [`ParsedSnapshot::edges`]). Blocking.
fn parse_and_map(
    snapshot_dir: &Path,
    repo_path_text: &str,
    node_set: Option<&[String]>,
) -> CodeGraphResult<(ParsedSnapshot, Vec<CodeNode>)> {
    let mut snapshot = ParsedSnapshot::read(snapshot_dir)?;

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
    snapshot.edges = Some(edges);
    Ok((snapshot, data_points))
}

/// What one load did.
#[derive(Debug, Clone)]
pub struct CodeGraphLoad {
    /// The node write's result (the data points and the pre-write graph).
    pub state: CodeGraphLoadState,
    /// The delta stamped on the repository node (`last_delta`), `None` when
    /// nothing was stamped.
    pub delta: Option<Map<String, Value>>,
}

/// Load the code graph of the repository at `repo_path` into `graph`: run
/// enola, map its snapshot, write the nodes, then the typed edges, then the
/// repository stamp. Python's `get_code_graph_tasks` run as a pipeline,
/// graph-only (`index_vectors=False`, the opt-in comes later).
///
/// With a `ledger`, every node and edge of the snapshot (written or already
/// present) is recorded under its owner (the repository's `Data` row), so
/// deleting the row finds them and deleting another owner keeps them.
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
    } = extract_code_graph(config, repo_path, None, node_set).await?;
    let state = add_code_graph_data_points(graph, ledger, data_points).await?;
    let repo_path_text = repo_path.to_string_lossy();
    let delta = add_code_graph_edges(
        graph,
        ledger,
        &state,
        &snapshot,
        Some(&repo_path_text),
        node_set,
    )
    .await?;
    Ok(CodeGraphLoad { state, delta })
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
