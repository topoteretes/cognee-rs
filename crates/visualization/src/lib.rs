//! Interactive HTML knowledge-graph visualization for Cognee-Rust.
//!
//! This crate ports two Python modules and vendors one frontend:
//!
//! * [`preprocessor.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/preprocessor.py)
//!   — turns raw `(nodes, edges)` into the renderer-facing snapshot: per-node
//!   `stage` / `visual_rank` / `degree` / `importance` / `label_priority` /
//!   `provenance`, per-link `edge_class` / `bundle_key`, the four provenance
//!   color maps, the derived type-schema graph and the Memory-tab payload.
//!   Ported in [`preprocessor`], which is public so callers can build the
//!   renderer snapshot themselves (and so the parity tests can pin it).
//! * [`cognee_network_visualization.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/cognee_network_visualization.py)
//!   — the orchestrator: reads the HTML shell and substitutes all 20
//!   `__TOKEN__` slots (JS chunks first, data payloads second). Ported in the
//!   private `html` module.
//! * `template.html` + `views/{ui_chrome,schema_view,story_view,inspector,memory_map,semantic_map}.js`
//!   — **vendored byte-for-byte** into `assets/`, not ported. They must be
//!   re-synced wholesale from upstream rather than hand-edited; see
//!   `assets/README.md` for the rule and the reason it exists.
//!
//! The rendered page has four tabs: **Graph** (canvas force/flow/story
//! renderer), **Schema** (ontology diagram + type/instance inspector),
//! **Memory** (deterministic column map of documents → chunks → entities →
//! summaries → context) and **Semantic**.
//!
//! # Deferred pieces
//!
//! * **Semantic tab** — `__SEMANTIC_POSITIONS__` and `__SEMANTIC_CLUSTERS__`
//!   are both the literal `null`, so the tab renders its friendly empty state.
//!   Rust's `cognee_vector::SearchResult` carries no `vector` field, so stored
//!   embeddings cannot be read back, and the workspace has no PCA/SVD or
//!   k-means. This is the same state Python reaches whenever its own
//!   best-effort `_semantic_payload` raises.
//! * **Session events** — `__SEARCH_EVENTS__` is the literal `[]`; there is no
//!   Rust equivalent of Python's `visualization/session_events.py`, so the
//!   Memory timeline rail and the Semantic recall overlay stay empty.
//! * **Subgraph bounding** — Python's `subgraph_data.py` (bounded per-dataset
//!   subgraph extraction) has no Rust counterpart; Rust always renders the
//!   full graph returned by `get_graph_data()`.
//!
//! # Quick start
//!
//! ```no_run
//! use cognee_graph::GraphDBTrait;
//! use cognee_visualization::visualize;
//! use std::path::Path;
//!
//! # async fn example(graph_db: &dyn GraphDBTrait) -> Result<(), Box<dyn std::error::Error>> {
//! // Write the visualization to a caller-specified file.
//! let _path = visualize(graph_db, Some(Path::new("/tmp/graph.html"))).await?;
//!
//! // Or write it to `~/graph_visualization.html` (matches Python behavior).
//! let _path = visualize(graph_db, None).await?;
//! # Ok(()) }
//! ```

mod colors;
mod error;
mod html;
mod paths;
pub mod preprocessor;

pub use error::VisualizationError;
pub use preprocessor::{ColorMaps, PreprocessedGraph, preprocess};

use std::path::{Path, PathBuf};

use cognee_graph::GraphDBTrait;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::info;

/// Generate an interactive HTML knowledge-graph visualization of the supplied
/// graph database.
///
/// * `graph_db` — graph database from which all nodes + edges are fetched via
///   `get_graph_data()`.
/// * `output_path` — optional destination path. When `None`, the file is
///   written to `~/graph_visualization.html` (or `%USERPROFILE%` on Windows),
///   matching Python's `visualize_graph()`.
///
/// Returns the absolute path the file was written to.
pub async fn visualize(
    graph_db: &dyn GraphDBTrait,
    output_path: Option<&Path>,
) -> Result<PathBuf, VisualizationError> {
    let html = render(graph_db).await?;

    let dest: PathBuf = match output_path {
        Some(p) => p.to_path_buf(),
        None => paths::default_output_path()?,
    };

    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).await?;
    }
    let mut file = fs::File::create(&dest).await?;
    file.write_all(html.as_bytes()).await?;
    file.flush().await?;

    info!(path = %dest.display(), "Graph visualization saved");
    Ok(dest)
}

/// Render the HTML visualization string for the supplied graph database,
/// without writing it anywhere.
///
/// Preprocesses the raw graph (Python `preprocess(graph_data,
/// schema_data=None)`) and then assembles the vendored HTML shell around it
/// (Python `cognee_network_visualization`). `schema_data` is `None` here — the
/// preprocessor derives the type-schema graph from the nodes and links it sees,
/// which is what feeds the Schema tab.
///
/// Useful when the caller wants to stream the HTML over HTTP, embed it into a
/// larger page, or post-process it before persisting.
pub async fn render(graph_db: &dyn GraphDBTrait) -> Result<String, VisualizationError> {
    let (nodes, edges) = graph_db.get_graph_data().await?;
    render_graph_data(nodes, edges)
}

/// [`render`] over graph data already in hand — for a caller with no engine
/// to read, e.g. an empty graph for a dataset whose per-dataset storage was
/// never provisioned (rendering must not provision it).
pub fn render_graph_data(
    nodes: Vec<cognee_graph::GraphNode>,
    edges: Vec<cognee_graph::EdgeData>,
) -> Result<String, VisualizationError> {
    let pre = preprocessor::preprocess(nodes, edges, None);
    html::build_html(&pre)
}

/// Render a combined HTML visualization aggregating multiple `(user_label, graph_db)`
/// pairs into one output.
///
/// Each pair's nodes are tagged with a `source_user` attribute carrying the
/// supplied human-readable label so the d3 template can color-code by user.
/// Mirrors Python's `aggregate_multi_user_graphs()` in
/// [`cognee/modules/visualization/cognee_network_visualization.py:184-227`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/visualization/cognee_network_visualization.py#L184-L227):
///
/// - Nodes are deduplicated by `str(node_id)` with **first-write-wins**
///   semantics so iteration order across the supplied pairs determines the
///   surviving node entry.
/// - Edges are deduplicated by the `(source, target, relation)` tuple.
/// - The `source_user` field is only populated when the inbound node does not
///   already carry one (Python's `if not node_info.get("source_user")`).
///
/// An empty input produces a valid-but-empty HTML document.
///
/// `pairs` is a slice of `(user_label, graph_db)` tuples; the label is taken
/// as an arbitrary `&str` to keep this crate decoupled from
/// `cognee_models::User`. Callers should resolve the underlying user record
/// and pass `user.email` (or the stringified id as a fallback) so the
/// `userColors` palette key matches Python.
pub async fn render_multi_user(
    pairs: &[(String, std::sync::Arc<dyn GraphDBTrait>)],
) -> Result<String, VisualizationError> {
    use std::borrow::Cow;
    use std::collections::{HashMap, HashSet};

    // First-write-wins by stringified node id (mirror Python L211).
    let mut all_nodes: HashMap<String, cognee_graph::GraphNode> = HashMap::new();
    let mut node_order: Vec<String> = Vec::new();

    // Edge dedupe by (source, target, relation) (mirror Python L221-225).
    let mut all_edges: Vec<cognee_graph::EdgeData> = Vec::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();

    for (user_label, gdb) in pairs {
        let (nodes, edges) = gdb.get_graph_data().await?;
        for (node_id, mut node_info) in nodes {
            let key = node_id.to_string();
            if all_nodes.contains_key(&key) {
                continue;
            }
            // Mirror Python's `if not node_info.get("source_user"): node_info["source_user"] = user_label`
            // — preserve any pre-existing `source_user` on the node so the
            // owning user's value (if already tagged) survives.
            //
            // `not x` is Python *truthiness*, so `""`, `0`, `false`, `[]` and
            // `{}` are all "unlabelled" and must be overwritten just like a
            // missing key. Getting this wrong left the node's `source_user`
            // falsy, kept it out of the `userColors` map and left it uncoloured
            // in the colour-by-user overlay.
            let needs_label = !node_info
                .get("source_user")
                .is_some_and(preprocessor::is_truthy);
            if needs_label {
                node_info.insert(
                    Cow::Borrowed("source_user"),
                    serde_json::Value::String(user_label.clone()),
                );
            }
            node_order.push(key.clone());
            all_nodes.insert(key, (node_id, node_info));
        }
        for edge in edges {
            let edge_key = (edge.0.to_string(), edge.1.to_string(), edge.2.clone());
            if seen_edges.insert(edge_key) {
                all_edges.push(edge);
            }
        }
    }

    let ordered_nodes: Vec<cognee_graph::GraphNode> = node_order
        .into_iter()
        .filter_map(|k| all_nodes.remove(&k))
        .collect();

    let pre = preprocessor::preprocess(ordered_nodes, all_edges, None);
    html::build_html(&pre)
}
