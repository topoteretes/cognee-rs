# Cognee Visualization

Interactive HTML knowledge-graph visualization for Cognee-Rust.

This crate ports the Python `cognee_network_visualization` module. It reads all
nodes and edges from any `GraphDBTrait` implementation and renders them into a
single self-contained HTML file that uses d3.js v7 for force-directed layout and
Canvas rendering.

## Usage

```rust
use cognee_graph::GraphDBTrait;
use cognee_visualization::visualize;
use std::path::Path;

async fn example(graph_db: &dyn GraphDBTrait) -> Result<(), Box<dyn std::error::Error>> {
    // Write the visualization to a caller-specified file.
    let path = visualize(graph_db, Some(Path::new("/tmp/graph.html"))).await?;

    // Or write to ~/graph_visualization.html (matches Python behavior).
    let path = visualize(graph_db, None).await?;

    println!("wrote {}", path.display());
    Ok(())
}
```

## API

- `visualize(graph_db, output_path) -> PathBuf` — render and write the HTML file.
  When `output_path` is `None`, writes to `~/graph_visualization.html`
  (`%USERPROFILE%` on Windows). Returns the path written.
- `render(graph_db) -> String` — render the HTML string without writing it
  (useful for streaming over HTTP or embedding in a larger page).
- `render_multi_user(pairs) -> String` — aggregate multiple
  `(user_label, Arc<dyn GraphDBTrait>)` pairs into one HTML document. Nodes are
  deduplicated by stringified id (first-write-wins) and tagged with a
  `source_user` attribute so the d3 template can color-code by user. Mirrors
  Python's `aggregate_multi_user_graphs()`.

Errors are surfaced via `VisualizationError`.
