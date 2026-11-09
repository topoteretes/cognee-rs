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

    /// The offline transform (inline d3, drop the webfont links) did not find
    /// one of its exact patterns exactly once in the vendored `template.html`.
    ///
    /// This means an upstream resync changed the tags the transform rewrites;
    /// rendering fails loudly rather than silently shipping a page that
    /// fetches from a CDN. Update the patterns in `src/html.rs`.
    #[error(
        "offline transform: expected exactly one occurrence of {pattern:?} in the vendored \
         template.html, found {found}; the template was resynced with changed tags"
    )]
    OfflineTransform {
        /// The exact pattern that was looked for.
        pattern: &'static str,
        /// How many times it occurred.
        found: usize,
    },

    /// The bundled d3 contains `</script`, `<!--` or `<script`, which would
    /// terminate the inline `<script>` element early or change how the HTML
    /// tokenizer finds its end.
    #[error("bundled d3 contains `</script`, `<!--` or `<script` and cannot be inlined verbatim")]
    UnsafeInlineScript,

    /// The visualization bounds are out of range (Python raises `ValueError`;
    /// the HTTP layer maps this to a 400).
    #[error("invalid visualization bounds: {0}")]
    InvalidBounds(#[from] crate::subgraph::BoundsError),

    /// The [`SeedSearcher`](crate::subgraph::SeedSearcher) failed to turn the
    /// query into seed nodes.
    #[error("seed search failed: {0}")]
    SeedSearch(#[source] crate::subgraph::SeedSearchError),
}
