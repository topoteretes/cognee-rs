//! Error types for the visualization crate.

use thiserror::Error;

/// Errors that can occur while generating an HTML graph visualization.
#[derive(Debug, Error)]
pub enum VisualizationError {
    /// Error returned by the underlying graph database when fetching data.
    #[error("Graph DB error: {0}")]
    GraphDb(#[from] cognee_graph::GraphDBError),

    /// JSON serialization failed while embedding graph data into the template.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Filesystem IO error while writing the HTML file.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Neither `dirs::home_dir()` nor the `HOME` / `USERPROFILE` environment
    /// variables yielded a valid path, so the default output location cannot
    /// be resolved.
    #[error("Could not determine home directory for default output path")]
    NoHomeDir,
}
