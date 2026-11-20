//! The code graph load for one local repository: extract, write nodes,
//! write edges.
//!
//! Port of Python's `extract_code_graph` and `get_code_graph_tasks`
//! (`cognee/tasks/code_graph/extract_code_graph.py`). Python returns three
//! pipeline tasks; here they are three functions run in order by
//! [`load_code_graph`], which the cognify route calls.

use std::path::Path;

use cognee_graph::GraphDBTrait;
use serde_json::{Map, Value};

use crate::config::CodeGraphConfig;
use crate::error::CodeGraphResult;
use crate::mapping::map_facts_to_data_points;
use crate::models::CodeNode;
use crate::runner::run_enola_generate;
use crate::snapshot::ParsedSnapshot;
use crate::writer::{
    CodeGraphLedger, CodeGraphLoadState, add_code_graph_data_points, add_code_graph_edges,
};

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
    let snapshot_dir = match snapshot_dir {
        Some(dir) => dir.to_path_buf(),
        None => run_enola_generate(config, repo_path).await?,
    };
    let snapshot = ParsedSnapshot::read(&snapshot_dir)?;

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

    let repo_path_text = repo_path.to_string_lossy();
    let data_points =
        map_facts_to_data_points(&snapshot.facts, Some(&repo_path_text), None, node_set);
    tracing::info!(
        "Mapped {} enola fact(s) to {} data point(s).",
        snapshot.facts.len(),
        data_points.len()
    );
    Ok(ExtractedCodeGraph {
        snapshot,
        data_points,
    })
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
