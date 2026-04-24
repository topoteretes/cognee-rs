# Implementation Plan: `visualize()`

**Gap doc:** [../visualize.md](../visualize.md)  
**Python reference:** `cognee/api/v1/visualize/visualize.py` (+ `cognee_network_visualization.py`)  
**Rust entry point:** *(new)* `crates/visualization/` or module in `cognee-lib`

---

## 1. Goal & Scope

Port Python's `visualize_graph()` (aliased to `cognee.visualize` in V2) to Rust with near-exact visual parity.

### Final user-visible Rust API

Primary (top-level re-export from `cognee-lib`):

```rust
use std::path::{Path, PathBuf};

/// Generate an interactive HTML knowledge-graph visualization.
///
/// * `output_path` – optional destination. When `None`, writes to
///   `~/graph_visualization.html` (matching Python).
/// * `graph_db`   – any `Arc<dyn GraphDBTrait>` (caller supplies; matches the
///   pattern used by `memify()` and other cognee-lib free functions).
///
/// Returns the absolute path of the written file.
pub async fn visualize(
    graph_db: &dyn GraphDBTrait,
    output_path: Option<&Path>,
) -> Result<PathBuf, VisualizationError>;
```

Convenience wrapper that mirrors Python signature (`destination_file_path` only):

```rust
pub async fn visualize_with_default_components(
    output_path: Option<&Path>,
) -> Result<PathBuf, VisualizationError>;
```

This internally constructs a `ComponentManager` from environment / settings and pulls the graph DB, matching `cognee.visualize()` in Python.

### Default output location

Mirrors Python `cognee_network_visualization.py:101–102`:

```
~/graph_visualization.html
```

Implementation uses the `dirs` crate or `std::env::var("HOME")` fallback. On Windows: `%USERPROFILE%\graph_visualization.html`.

### Optional HTTP server helper

`start_visualization_server()` is **out of scope** for this task. See §6.

---

## 2. Design Overview

### 2.1 Where the code lives — new `crates/visualization/` crate (recommended)

Create a new workspace crate `crates/visualization/` (crate name `cognee-visualization`). Rationale:

1. **Mirrors Python layout.** Python places the core logic in `cognee/modules/visualization/` — a standalone module.
2. **Self-contained dependencies.** The HTML template asset (~50 KB) is embedded via `include_str!`; keeping that blob out of `cognee-lib` avoids bloat for users who don't need visualization.
3. **Consistent with existing pattern.** `crates/ontology`, `crates/delete`, `crates/session` are all standalone crates with a single domain responsibility.
4. **Feature-gate in `cognee-lib`.** Re-export via `pub mod visualization { pub use cognee_visualization::*; }` with a `visualization` default feature.

Directory layout:

```
crates/visualization/
├── Cargo.toml
├── assets/
│   └── graph_template.html        # verbatim port of _get_html_template()
├── src/
│   ├── lib.rs                     # public API: visualize(), VisualizationError
│   ├── error.rs                   # thiserror enum
│   ├── colors.rs                  # static color map + HSL provenance colors
│   ├── serialize.rs               # graph_data -> (nodes_list, links_list) JSON
│   ├── html.rs                    # template loading, placeholder substitution
│   └── paths.rs                   # home_dir resolution, default path
└── tests/
    ├── colors_test.rs
    ├── html_test.rs
    └── end_to_end_test.rs
```

### 2.2 HTML template shipping strategy

The Python `_get_html_template()` at `cognee_network_visualization.py:188–1869` is a ~1680-line embedded HTML string. In Rust:

1. **Extract verbatim** into `crates/visualization/assets/graph_template.html`. Preserve every byte including the seven `__NODES_DATA__`, `__LINKS_DATA__`, `__TASK_COLORS__`, `__PIPELINE_COLORS__`, `__NODESET_COLORS__`, `__USER_COLORS__`, `__SCHEMA_DATA__` placeholders.
2. **Embed at compile time** via:

   ```rust
   pub const HTML_TEMPLATE: &str = include_str!("../assets/graph_template.html");
   ```

3. **No build script.** `include_str!` is enough. A unit test validates that every placeholder token appears.

### 2.3 JSON embedding strategy

Python `_build_html` (lines 159–185):

```python
def _safe_json_embed(obj):
    return json.dumps(obj).replace("</", "<\\/")
```

Rust port (`src/html.rs`):

```rust
pub(crate) fn safe_json_embed<T: serde::Serialize>(value: &T) -> Result<String, VisualizationError> {
    let raw = serde_json::to_string(value)?;
    Ok(raw.replace("</", "<\\/"))
}
```

Substitution uses successive `str::replace` calls (7 placeholders) — cheap relative to template size.

### 2.4 Color mapping logic

**A) Static node-type → color map.** `src/colors.rs`:

```rust
pub(crate) fn type_color(node_type: Option<&str>, ontology_valid: bool) -> &'static str {
    if ontology_valid { return "#D8D8D8"; }
    match node_type.unwrap_or("default") {
        "Entity"            => "#6510F4",
        "EntityType"        => "#A550FF",
        "DocumentChunk"     => "#0DFF00",
        "TextSummary"       => "#6510F4",
        "TableRow"          => "#A550FF",
        "TableType"         => "#6510F4",
        "ColumnValue"       => "#747470",
        "SchemaTable"       => "#A550FF",
        "DatabaseSchema"    => "#6510F4",
        "SchemaRelationship"=> "#323332",
        "default"           => "#7c3aed",
        _                   => "#DBD8D8",
    }
}
```

Exactly mirrors Python `cognee_network_visualization.py:27–47`.

**B) Deterministic HSL-based provenance color generator.** Python:

```python
def _generate_provenance_colors(values):
    color_map = {}
    unique = sorted(set(v for v in values if v))
    for i, name in enumerate(unique):
        hue = (i * 137.5) % 360
        r, g, b = colorsys.hls_to_rgb(hue / 360, 0.6, 0.65)
        color_map[name] = "#{:02x}{:02x}{:02x}".format(int(r*255), int(g*255), int(b*255))
    return color_map
```

Rust port:

```rust
pub(crate) fn provenance_colors<I>(values: I) -> BTreeMap<String, String>
where I: IntoIterator<Item = Option<String>>
{
    let mut unique: Vec<String> = values.into_iter().flatten().collect();
    unique.sort();
    unique.dedup();
    unique.into_iter().enumerate().map(|(i, name)| {
        let hue = (i as f64 * 137.5) % 360.0;
        let (r, g, b) = hls_to_rgb(hue / 360.0, 0.6, 0.65);
        let hex = format!(
            "#{:02x}{:02x}{:02x}",
            (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8,
        );
        (name, hex)
    }).collect()
}
```

`hls_to_rgb` is a short function (~15 lines) implementing Python `colorsys.hls_to_rgb` exactly — must use HLS (not HSL) so math matches Python bit-for-bit. Deterministic: same input → same output, enabling exact HTML output assertions.

**Note:** return type is `BTreeMap` (not `HashMap`) so JSON serialization is ordered → tests can assert exact HTML bytes.

### 2.5 `start_visualization_server()` — deferred

Out of scope (see §6). The Python helper spawns a TCP server in a daemon thread. Rust would need `hyper`/`axum`, lifecycle handling, and a shutdown handle.

---

## 3. Step-by-Step Implementation

### Step 1 — Scaffold the `cognee-visualization` crate

**Files to create:**
- `crates/visualization/Cargo.toml`
- `crates/visualization/src/lib.rs` (skeleton)
- `crates/visualization/src/error.rs`

**Cargo.toml:**

```toml
[package]
name = "cognee-visualization"
version.workspace = true
edition.workspace = true

[dependencies]
async-trait = { workspace = true }
cognee-graph = { path = "../graph" }
dirs = "5"
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["fs", "io-util"] }
tracing = { workspace = true }

[dev-dependencies]
cognee-test-utils = { path = "../test-utils" }
tempfile = { workspace = true }
tokio = { workspace = true, features = ["macros", "rt"] }
```

**error.rs:**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VisualizationError {
    #[error("Graph DB error: {0}")]
    GraphDb(#[from] cognee_graph::GraphDBError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Could not determine home directory")]
    NoHomeDir,
}
```

Register the crate in the workspace root `Cargo.toml` `[workspace] members` list.

**Depends on:** nothing. **Effort:** 1 h.

### Step 2 — Port the HTML template asset verbatim

**File to create:** `crates/visualization/assets/graph_template.html`

Copy the return value of `_get_html_template()` — every character from line 189 (`<!DOCTYPE html>`) through the closing `"""` at line ~1869 of `/tmp/cognee-python/cognee/modules/visualization/cognee_network_visualization.py`. Do **not** modify JS, CSS, or placeholders.

**Depends on:** Step 1.  
**Effort:** 1 h.

### Step 3 — Implement `colors.rs` (static + HSL)

**File to create:** `crates/visualization/src/colors.rs`

Contains:
- `fn type_color(node_type: Option<&str>, ontology_valid: bool) -> &'static str`
- `fn hls_to_rgb(h: f64, l: f64, s: f64) -> (f64, f64, f64)`
- `fn provenance_colors(values: impl IntoIterator<Item = Option<String>>) -> BTreeMap<String, String>`

**Test stubs** added inline:
- `type_color()` returns `#6510F4` for `"Entity"`, `#D8D8D8` when `ontology_valid=true`
- `hls_to_rgb(0.5, 0.6, 0.65)` matches Python output to 1e-9
- `provenance_colors(["task-a", "task-b", None, "task-a"])` produces 2 distinct deterministic hex colors, sorted

**Depends on:** Step 1. **Effort:** 3 h.

### Step 4 — Implement `serialize.rs` (graph data → JSON nodes/edges)

**File to create:** `crates/visualization/src/serialize.rs`

Python reference: `cognee_network_visualization.py:41–83`.

Public function:

```rust
pub(crate) struct Serialized {
    pub nodes: Vec<serde_json::Value>,
    pub links: Vec<serde_json::Value>,
    pub task_colors:     BTreeMap<String, String>,
    pub pipeline_colors: BTreeMap<String, String>,
    pub nodeset_colors:  BTreeMap<String, String>,
    pub user_colors:     BTreeMap<String, String>,
}

pub(crate) fn serialize_graph(
    nodes: Vec<GraphNode>,
    edges: Vec<EdgeData>,
) -> Serialized;
```

For **nodes**: clone NodeData, insert `id`, `color` (via `type_color`), `name` (or fallback), remove `created_at`/`updated_at`.

For **edges**: build `{source, target, relation, weight, all_weights, relationship_type, edge_info}` with weight flattening from `weight`, `weights` dict, and `weight_<key>` fields.

After processing nodes, derive the four provenance color maps:

```rust
let task_colors     = provenance_colors(nodes.iter().map(|n| extract_str(n, "source_task")));
let pipeline_colors = provenance_colors(nodes.iter().map(|n| extract_str(n, "source_pipeline")));
let nodeset_colors  = provenance_colors(nodes.iter().map(|n| extract_str(n, "source_node_set")));
let user_colors     = provenance_colors(nodes.iter().map(|n| extract_str(n, "source_user")));
```

**Depends on:** Step 3. **Effort:** 4 h.

### Step 5 — Implement `html.rs` (template substitution)

**File to create:** `crates/visualization/src/html.rs`

```rust
pub const HTML_TEMPLATE: &str = include_str!("../assets/graph_template.html");

pub(crate) fn build_html(s: &Serialized, schema_data: Option<&serde_json::Value>)
    -> Result<String, VisualizationError>
{
    let mut html = HTML_TEMPLATE.to_string();
    html = html.replace("__NODES_DATA__",     &safe_json_embed(&s.nodes)?);
    html = html.replace("__LINKS_DATA__",     &safe_json_embed(&s.links)?);
    html = html.replace("__TASK_COLORS__",    &safe_json_embed(&s.task_colors)?);
    html = html.replace("__PIPELINE_COLORS__",&safe_json_embed(&s.pipeline_colors)?);
    html = html.replace("__NODESET_COLORS__", &safe_json_embed(&s.nodeset_colors)?);
    html = html.replace("__USER_COLORS__",    &safe_json_embed(&s.user_colors)?);
    html = html.replace(
        "__SCHEMA_DATA__",
        &match schema_data {
            Some(v) => safe_json_embed(v)?,
            None    => "null".to_string(),
        },
    );
    Ok(html)
}

fn safe_json_embed<T: serde::Serialize>(v: &T) -> Result<String, VisualizationError> {
    Ok(serde_json::to_string(v)?.replace("</", "<\\/"))
}
```

**Depends on:** Steps 2, 4. **Effort:** 1 h.

### Step 6 — Implement `paths.rs` (default destination)

**File to create:** `crates/visualization/src/paths.rs`

```rust
pub(crate) fn default_output_path() -> Result<PathBuf, VisualizationError> {
    dirs::home_dir()
        .map(|h| h.join("graph_visualization.html"))
        .ok_or(VisualizationError::NoHomeDir)
}
```

Mirrors Python `cognee_network_visualization.py:101–102`.

**Depends on:** Step 1. **Effort:** 0.25 h.

### Step 7 — Implement public `visualize()` API

**File to create/extend:** `crates/visualization/src/lib.rs`

```rust
mod colors;
mod error;
mod html;
mod paths;
mod serialize;

pub use error::VisualizationError;

use cognee_graph::GraphDBTrait;
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::info;

pub async fn visualize(
    graph_db: &dyn GraphDBTrait,
    output_path: Option<&Path>,
) -> Result<PathBuf, VisualizationError> {
    let (nodes, edges) = graph_db.get_graph_data().await?;
    let serialized = serialize::serialize_graph(nodes, edges);
    let html = html::build_html(&serialized, None)?;

    let dest: PathBuf = match output_path {
        Some(p) => p.to_path_buf(),
        None    => paths::default_output_path()?,
    };

    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }
    let mut f = fs::File::create(&dest).await?;
    f.write_all(html.as_bytes()).await?;
    f.flush().await?;

    info!(path = %dest.display(), "Graph visualization saved");
    Ok(dest)
}

pub async fn render(
    graph_db: &dyn GraphDBTrait,
) -> Result<String, VisualizationError> {
    let (nodes, edges) = graph_db.get_graph_data().await?;
    let serialized = serialize::serialize_graph(nodes, edges);
    html::build_html(&serialized, None)
}
```

**Depends on:** Steps 3, 4, 5, 6. **Effort:** 2 h.

### Step 8 — Re-export from `cognee-lib`

**File to modify:** `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/lib.rs`

Add module re-export and top-level `visualize` alias:

```rust
pub mod visualization {
    pub use cognee_visualization::*;
}

pub use cognee_visualization::visualize;
```

Also add to `[dependencies]` of `crates/lib/Cargo.toml`:

```toml
cognee-visualization = { path = "../visualization" }
```

**Depends on:** Step 7. **Effort:** 0.5 h.

### Step 9 — CLI subcommand `cognee-cli visualize`

**Files to modify:**
- `/home/dmytro/dev/cognee/cognee-rust/crates/cli/src/cli.rs` — add `Visualize(VisualizeArgs)` variant to `enum Commands` and a new `VisualizeArgs` struct.
- `/home/dmytro/dev/cognee/cognee-rust/crates/cli/src/commands/mod.rs` — add `pub mod visualize;`
- `/home/dmytro/dev/cognee/cognee-rust/crates/cli/src/commands/visualize.rs` — **create**
- `/home/dmytro/dev/cognee/cognee-rust/crates/cli/src/main.rs` — dispatch to the new command

**Depends on:** Step 8. **Effort:** 1.5 h.

### Step 10 — Unit + integration tests

**Files to create:**
- `crates/visualization/src/colors.rs` — inline `#[cfg(test)]` tests
- `crates/visualization/tests/html_test.rs`
- `crates/visualization/tests/end_to_end_test.rs`

See §4 for test details.

**Depends on:** Steps 3, 5, 7. **Effort:** 4 h.

### Step 11 — Workspace wiring + docs

- Add `crates/visualization` to workspace `members` in root `Cargo.toml`.
- Add rustdoc examples to `visualize()` public function.
- Update `crates/visualization/README.md` with usage snippet and screenshot reference.
- Update `docs/api-v2/visualize.md` status line to `Implemented`.

**Depends on:** Steps 1–10. **Effort:** 1 h.

---

## 4. Test Plan

### 4.1 Unit tests — `crates/visualization/src/colors.rs`

1. **type_color static map** — each of 10 known types produces the expected hex string.
2. **type_color fallback** — `"UnknownType"` returns `"#DBD8D8"`, `None`/`"default"` returns `"#7c3aed"`.
3. **ontology_valid override** — when `ontology_valid=true`, color is always `"#D8D8D8"`.
4. **hls_to_rgb parity** — table of 5 Python-computed `(h,l,s) → (r,g,b)` triples; Rust output must match within `1e-9`.
5. **provenance_colors determinism** — same input sequence produces same `BTreeMap`; expected-output table assertion.
6. **provenance_colors ignores None and deduplicates** — `[Some("x"), None, Some("x"), Some("y")]` produces 2 entries, sorted by key.

### 4.2 Unit tests — `crates/visualization/tests/html_test.rs`

1. **Template contains all 7 placeholders** — `HTML_TEMPLATE.contains("__NODES_DATA__")` etc.
2. **safe_json_embed escapes `</`** — `safe_json_embed(&json!({"x": "</script>"}))` contains `"<\\/script>"`.
3. **build_html replaces all placeholders** — after substitution, asserted output contains neither `__NODES_DATA__` nor any other placeholder token.
4. **build_html with empty graph** — zero nodes, zero edges → all color maps serialize to `{}`, schema to `null`.

### 4.3 End-to-end test — `crates/visualization/tests/end_to_end_test.rs`

Uses `cognee-test-utils::MockGraphDB`.

```rust
#[tokio::test]
async fn visualize_writes_deterministic_html() {
    let db = MockGraphDB::new();
    db.add_node_raw(json!({
        "id": "n1", "type": "Entity", "name": "Alice",
        "source_task": "task-a", "source_pipeline": "pipe-1",
    })).await.unwrap();
    // ... (n2 DocumentChunk, n3 EntityType)

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out.html");
    let written = visualize(&db, Some(&dest)).await.unwrap();
    assert_eq!(written, dest);
    let html = std::fs::read_to_string(&dest).unwrap();

    assert!(html.contains("\"id\":\"n1\""));
    assert!(html.contains("\"color\":\"#6510F4\""));
    assert!(html.contains("\"all_weights\":{\"default\":"));
    assert!(!html.contains("__NODES_DATA__"));
    assert!(!html.contains("__LINKS_DATA__"));

    let hash = sha256_hex(&html);
    assert_eq!(hash, EXPECTED_SHA256);
}

#[tokio::test]
async fn visualize_default_path_uses_home_dir() {
    // Override HOME via env to a temp dir, call visualize(..., None),
    // assert the returned path equals <HOME>/graph_visualization.html
    // and that the file exists.
}

#[tokio::test]
async fn visualize_empty_graph() {
    let db = MockGraphDB::new();
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("empty.html");
    visualize(&db, Some(&dest)).await.unwrap();
    let html = std::fs::read_to_string(&dest).unwrap();
    assert!(html.contains("var nodes = [];"));
    assert!(html.contains("var links = [];"));
}
```

### 4.4 CLI smoke test — `crates/cli/tests/visualize_e2e.rs`

Invoke `cognee-cli visualize --output /tmp/x.html` via `assert_cmd`, assert exit 0, file exists, size > 50 KB (template size floor).

### 4.5 Manual QA (not automated)

- Open generated HTML in Firefox/Chrome. Verify: force layout animates, search highlights nodes, dark/light theme toggle, pan/zoom, filter buttons. No automated browser test in scope.

---

## 5. Effort Breakdown

| Step | Task | Hours |
|------|-------------------------------------------|-------|
| 1    | Scaffold crate                            | 1.0   |
| 2    | Extract HTML template asset               | 1.0   |
| 3    | `colors.rs` (static + HSL + tests)        | 3.0   |
| 4    | `serialize.rs`                            | 4.0   |
| 5    | `html.rs` substitution                    | 1.0   |
| 6    | `paths.rs` default home resolution        | 0.25  |
| 7    | Public `visualize()` + `render()`         | 2.0   |
| 8    | `cognee-lib` re-export                    | 0.5   |
| 9    | CLI `visualize` subcommand                | 1.5   |
| 10   | Tests (unit, integration, CLI e2e)        | 4.0   |
| 11   | Workspace wiring + docs                   | 1.0   |
| **Total**                                        | **19.25 h** (~2.5 days) |

Matches the gap doc's "2–4 days" estimate.

---

## 6. Out of Scope

The following are intentionally deferred:

1. **`start_visualization_server()`** — HTTP server to serve generated HTML files. Would require `hyper`/`axum`, lifecycle/shutdown handling. Tracked separately.
2. **`visualize_multi_user_graph()` / `aggregate_multi_user_graphs()`** — cognee-rust is single-tenant by design.
3. **Custom themes beyond the embedded dark/light toggle** — the template already includes both themes.
4. **Alternative renderers** (SVG-only, PNG export, React/Vue components).
5. **Schema-data visualization** (`__SCHEMA_DATA__` placeholder) — Rust port passes `None` for now; adding the schema-data path is <1 h follow-up when needed.
6. **Automatic browser launch** — Python doesn't do this; neither does Rust.
7. **Streaming / incremental render for very large graphs** — loads full graph into memory, matches Python behavior.

---

## Critical Files for Implementation

- /home/dmytro/dev/cognee/cognee-rust/crates/visualization/src/lib.rs *(new)*
- /home/dmytro/dev/cognee/cognee-rust/crates/visualization/src/colors.rs *(new)*
- /home/dmytro/dev/cognee/cognee-rust/crates/visualization/src/serialize.rs *(new)*
- /home/dmytro/dev/cognee/cognee-rust/crates/visualization/assets/graph_template.html *(new, verbatim port of `_get_html_template()`)*
- /home/dmytro/dev/cognee/cognee-rust/crates/lib/src/lib.rs *(modify — add `pub mod visualization` + prelude re-export)*
