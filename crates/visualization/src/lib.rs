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
