# API v2: `visualize()`

**Python source:** `cognee/api/v1/visualize/visualize.py` (67 lines)
**Rust status:** Not Started
**Implementation plan:** [impl/visualize-plan.md](impl/visualize-plan.md)

---

## 1. What it does

`visualize()` generates an **interactive HTML5 knowledge graph visualization** from the Cognee graph database. The function:

1. **Queries the graph DB** via `graph_engine.get_graph_data()` → returns `(nodes_data, edges_data)` tuples
2. **Processes node/edge metadata**: assigns colors by node type, extracts weights, handles provenance fields (`source_task`, `source_pipeline`, `source_node_set`, `source_user`)
3. **Generates deterministic color maps** for provenance filters via HSL hue rotation
4. **Embeds data as JSON** into an HTML template with escaping for `</` sequences
5. **Writes HTML file** to disk (default: `~/graph_visualization.html`, or custom path)
6. **Returns the HTML string** for optional further processing

**Key outputs:**
- **File**: HTML5 single-page app with embedded JSON data
- **Rendering library**: **d3.js v7** (force-directed graph layout)
- **Graph backend queried**: The underlying `GraphDBTrait` implementation (Ladybug in Rust, native graph DB in Python)
- **Browser**: Opens in desktop browser (no automatic launch, user must navigate manually)

**Interactive features** (from HTML template):
- Pan, zoom, drag-to-move nodes (canvas-based)
- Search box to highlight nodes by name
- Control bar: toggle filters, switch theme (dark/light), show/hide node labels
- Color-coding: node types (Entity, DocumentChunk, TextSummary, etc.), provenance sources (task, pipeline, user)
- Edge weight visualization (line thickness proportional to weight)
- Responsive layout (100vw/100vh canvas)

---

## 2. Related: `start_visualization_server()` and `cognee_network_visualization()`

Three distinct components, **not a V2 API function**:

### `cognee_network_visualization(graph_data, destination_file_path=None, schema_data=None)`
- **File**: `/tmp/cognee-python/cognee/modules/visualization/cognee_network_visualization.py`
- **What it does**: Core HTML generation function (lines 22–112)
  - Assembles node list with colors and metadata
  - Assembles edge list with weights and relationship types
  - Calls `_build_html()` (line 90) to inject JSON into template
  - Calls `_get_html_template()` (line 188) to retrieve embedded d3 + Canvas HTML
  - Writes file via `LocalFileStorage.store()` (line 108) if path provided, else to `~/graph_visualization.html`
  - Returns HTML string
- **Not a public V2 API export**; used internally by `visualize_graph()`

### `start_visualization_server(host='0.0.0.0', port=8001)`
- **File**: `/tmp/cognee-python/cognee/shared/utils.py`
- **What it does**: Spawns a **background HTTP server** in a daemon thread to serve files
  - Uses Python's `socketserver.TCPServer` + `SimpleHTTPRequestHandler`
  - Logs: `"Visualization server running at: http://{host}:{port}"`
  - Returns a `shutdown()` function to stop the server
- **Use case**: Serve HTML files over HTTP (e.g., for sandboxed environments or remote access)
- **V2 re-export**: `cognee.visualization_server(port=int)` → calls this with default host
- **Not automatically called by `visualize()`**; user must explicitly invoke if needed

### `visualize_graph()` (V2: `visualize`)
- **File**: `/tmp/cognee-python/cognee/api/v1/visualize/visualize.py` (lines 17–31)
- **Entry point**: `async def visualize_graph(destination_file_path: str = None) -> str`
- **Does**: Calls `get_graph_engine()` → `get_graph_data()` → `cognee_network_visualization()`
- **V2 re-export**: `cognee.visualize` (alias in `cognee/api/v1/__init__.py` line 6)
- **Returns**: HTML string

**Relationship summary:**
- **V2 API: `visualize()`** = `visualize_graph()` (thin wrapper over `cognee_network_visualization()`)
- **Helper: `cognee_network_visualization()`** = HTML template assembly + file I/O
- **Utility: `start_visualization_server()`** = Optional HTTP server (decoupled, user-invoked)

---

## 3. Building blocks (Python)

| Component | File | Purpose |
|---|---|---|
| **Graph reader** | `cognee.infrastructure.databases.graph.get_graph_engine()` | Returns graph DB interface; `.get_graph_data()` yields `(nodes_data, edges_data)` |
| **Color generation** | `cognee_network_visualization.py:_generate_provenance_colors()` | HSL hue-based deterministic color map from provenance values |
| **Node processing** | `cognee_network_visualization.py:cognee_network_visualization()` lines 41–51 | Assign IDs, colors, names; strip timestamps |
| **Edge processing** | `cognee_network_visualization.py` lines 53–83 | Extract source/target/relation/weights; flatten multi-weights dict |
| **HTML template assembly** | `cognee_network_visualization.py:_build_html()` lines 159–185 | String substitution: replace `__NODES_DATA__`, `__LINKS_DATA__`, color maps, schema data |
| **HTML template** | `cognee_network_visualization.py:_get_html_template()` lines 188–*end* | ~500-line embedded HTML5 template with d3.js CDN link, Canvas rendering, inline CSS/JS |
| **File storage** | `cognee.infrastructure.files.storage.LocalFileStorage` | `.store(filename, content, overwrite=True)` writes to disk |
| **Multi-user aggregation** | `cognee_network_visualization.py:aggregate_multi_user_graphs()` lines 115–156 | Merge graphs from multiple (user, dataset) pairs, tag nodes with `source_user` |
| **HTTP server** | `cognee.shared.utils.start_visualization_server()` | Python `socketserver.TCPServer`, daemon thread, returns shutdown function |

**Graph DB interface** (trait-like in Python):
- `await graph_engine.get_graph_data()` → `tuple[list[tuple[node_id, node_info]], list[tuple[source, target, relation, edge_info]]]`
- `node_info`: dict with `id` (overwritten), `type`, `name`, `ontology_valid`, `source_task`, `source_pipeline`, `source_node_set`, `source_user`, `created_at`, `updated_at`
- `edge_info`: dict with `weight`, `weights` (nested dict), `weight_*` (number fields), `relationship_type`

---

## 4. Rust status per building block

| Building Block | Status | Notes | Rust File(s) |
|---|---|---|---|
| **Graph reader** | ✓ Implemented | `GraphDBTrait::get_all_nodes()` + `get_all_edges()` exist; no `get_graph_data()` method | `crates/graph/src/traits.rs` |
| **Node/edge processing** | ✗ Not started | Would need to implement node type → color mapping, weight extraction, provenance field parsing | — |
| **Color generation** | ✗ Not started | HSL hue rotation logic (lines 16–19 Python) not ported | — |
| **HTML template assembly** | ✗ Not started | String interpolation + JSON embedding; requires d3.js HTML template | — |
| **File I/O** | ✓ Available | `LocalStorage::store()` exists, returns `StorageResult` | `crates/storage/src/local.rs` |
| **HTTP server** | ✗ Not started | No HTTP server in Rust codebase; would need tokio-based implementation | — |
| **Multi-user aggregation** | ✗ Not started | No equivalent; cognee-rust is single-tenant by design (no user/dataset aggregation) | — |

**Gap in Rust graph interface:**
- Python's `get_graph_data()` returns a unified tuple; Rust's `GraphDBTrait` has `.get_all_nodes()` and `.get_all_edges()` as separate methods
- No metadata on nodes/edges in Rust types; Python embeds rich dicts with provenance, weights, validation flags

---

## 5. Gaps — what Rust needs

### A. Core visualization infrastructure
1. **`get_graph_data()` method on `GraphDBTrait`** or a new function `serialize_graph_for_visualization()`
   - Return format: `(Vec<(node_id, node_metadata)>, Vec<(source_id, target_id, relation, edge_metadata)>)`
   - Node metadata: `type`, `name`, `ontology_valid` (bool), optional provenance fields
   - Edge metadata: `weight`, optional `weights` dict, `relationship_type`

2. **Node type → color mapper** (deterministic lookup)
   - Hard-code map: `Entity` → `#6510F4`, `DocumentChunk` → `#0DFF00`, etc.
   - Fallback: `"default"` → `#7c3aed`, `ontology_valid=true` → `#D8D8D8`

3. **Provenance color generator** (HSL hue rotation)
   - Input: list of provenance values
   - Output: `HashMap<String, String>` (value → hex color)
   - Algorithm: unique values → sorted → assign hue at 137.5° intervals

### B. HTML templating
4. **Embed d3.js v7 template** with Canvas rendering
   - ~500 lines of HTML/CSS/JavaScript
   - d3 force layout: `.forceSimulation()`, `.forceManyBody()`, `.forceLink()`, `.forceCollide()`
   - Canvas rendering: node circles, labels, edge lines
   - Controls: search box, theme toggle (dark/light), filter buttons
   - JSON placeholders: `__NODES_DATA__`, `__LINKS_DATA__`, `__TASK_COLORS__`, `__PIPELINE_COLORS__`, `__NODESET_COLORS__`, `__USER_COLORS__`, `__SCHEMA_DATA__`

5. **JSON serialization + escaping**
   - Use `serde_json::to_string()` + replace `"</"` → `"<\\/"`
   - Inject into template via string replacement

6. **Default file path logic**
   - If no path provided: write to `{HOME}/graph_visualization.html`
   - Use `dirs::home_dir()` or `std::env::var("HOME")`

### C. API entry point
7. **Add `visualize()` function to `cognee-lib` public API**
   ```rust
   pub async fn visualize(
       destination_file_path: Option<String>,
   ) -> Result<String, VisualizationError>
   ```
   - Async (requires `tokio` in calling context)
   - Returns HTML string
   - Writes file via `StorageTrait` if path provided

8. **CLI command** (optional but recommended)
   - `cognee-cli visualize [--output FILE]`
   - Reuses the function above

### D. Additional (future/optional)
9. **HTTP server for visualization** — separate from core function
   - Only if V2 API includes `start_visualization_server()` export
   - Low priority; can be CLI-only initially

10. **Multi-user aggregation** — document as out-of-scope
    - cognee-rust is single-tenant; multi-user graph merging is not a core feature
    - Can be deferred or implemented via scripting

---

## 6. Effort estimate

**T-shirt: S (small) to M (medium) — 2–4 days for a junior engineer, 1–2 for an experienced contributor**

### Rationale
- **Straightforward**: No complex algorithms; mostly HTML/JSON generation + file I/O
- **Leverages existing code**: `LocalStorage`, `GraphDBTrait`, `serde_json` already available
- **No external dependencies needed**: d3.js is CDN-hosted; no new crates required
- **Main effort**:
  - (1 day) Extract and adapt d3 HTML template from Python source; test Canvas rendering in browser
  - (0.5 days) Implement color mapping + provenance color generation
  - (0.5 days) Implement `get_graph_data()` or serialization function
  - (0.5 days) Wire up `visualize()` function and file I/O
  - (0.5 days) CLI integration + testing

### Risk factors (low)
- ✓ No external APIs (self-contained HTML/JS)
- ✓ No database schema changes
- ✓ Can be tested locally with any graph DB backend
- ✗ Browser rendering is not unit-testable; need manual QA with sample graphs

### Recommended approach
1. **Port HTML template** first (copy from Python, adapt JSON placeholder names if needed)
2. **Implement color mappers** (small, deterministic functions)
3. **Implement `get_graph_data()` serializer** (glue layer between `GraphDBTrait` and visualization)
4. **Integrate with `visualize()` function** (thin wrapper, similar to `cognify()`)
5. **Add CLI command** for convenience
6. **Manual test** in browser with real graph data

---

## References

- **Python source**: `/tmp/cognee-python/cognee/api/v1/visualize/`
- **Rust graph trait**: `/home/dmytro/dev/cognee/cognee-rust/crates/graph/src/traits.rs`
- **Rust storage trait**: `/home/dmytro/dev/cognee/cognee-rust/crates/storage/src/lib.rs`
- **Rust lib.rs**: `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/lib.rs` (API facade)
- **Rust CLI commands**: `/home/dmytro/dev/cognee/cognee-rust/crates/cli/src/commands/`
