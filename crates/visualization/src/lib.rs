//! Interactive HTML knowledge-graph visualization for Cognee-Rust.
//!
//! This crate ports the Python `cognee_network_visualization` module to Rust.
//! It renders all nodes + edges of a `GraphDBTrait` into a single self-contained
//! HTML file that uses d3.js v7 for force-directed layout and Canvas rendering.
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
mod serialize;

pub use error::VisualizationError;

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
/// Useful when the caller wants to stream the HTML over HTTP, embed it into a
/// larger page, or post-process it before persisting.
pub async fn render(graph_db: &dyn GraphDBTrait) -> Result<String, VisualizationError> {
    let (nodes, edges) = graph_db.get_graph_data().await?;
    let serialized = serialize::serialize_graph(nodes, edges);
    html::build_html(&serialized, None)
}

/// Render a combined HTML visualization aggregating multiple `(user_id, graph_db)`
/// pairs into one output.
///
/// Each pair's nodes are tagged with a `user_id` attribute (UUID-stringified)
/// so the d3 template can color-code by user. Edges from all pairs are
/// concatenated unchanged.
///
/// Mirrors Python's `visualize_multi_user_graph()` in
/// [`cognee/api/v1/visualize/visualize.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/visualize/visualize.py).
/// An empty input produces a valid-but-empty HTML document.
///
/// `pairs` is a slice of `(user_id_string, graph_db)` tuples; the user-id is
/// taken as a `&str` to keep the visualization crate decoupled from the
/// `cognee_models::User` type. Callers stringify their UUID/ID before invoking.
pub async fn render_multi_user(
    pairs: &[(String, std::sync::Arc<dyn GraphDBTrait>)],
) -> Result<String, VisualizationError> {
    use std::borrow::Cow;

    let mut all_nodes = Vec::new();
    let mut all_edges = Vec::new();
    for (user_id, gdb) in pairs {
        let (nodes, edges) = gdb.get_graph_data().await?;
        for (node_id, mut node_info) in nodes {
            node_info.insert(
                Cow::Borrowed("user_id"),
                serde_json::Value::String(user_id.clone()),
            );
            // `source_user` participates in the existing color-by-user
            // template, so populate it as well to drive the d3 palette.
            node_info.insert(
                Cow::Borrowed("source_user"),
                serde_json::Value::String(user_id.clone()),
            );
            all_nodes.push((node_id, node_info));
        }
        all_edges.extend(edges);
    }
    let serialized = serialize::serialize_graph(all_nodes, all_edges);
    html::build_html(&serialized, None)
}
