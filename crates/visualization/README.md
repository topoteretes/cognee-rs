# Cognee Visualization

Interactive HTML knowledge-graph visualization for Cognee-Rust.

Reads all nodes and edges from any `GraphDBTrait` implementation, enriches them,
and renders a single HTML file. The frontend is **vendored verbatim** from the
Python `cognee` repo — this crate is the preprocessor plus the orchestrator that
stitches the vendored assets around the data.

Parity mapping:

| Rust | Python source |
| --- | --- |
| `src/preprocessor/` | `cognee/modules/visualization/preprocessor.py` |
| `src/html.rs` | `cognee/modules/visualization/cognee_network_visualization.py` |
| `src/colors.rs` | the `color_map` dict + `_generate_provenance_colors()` |
| `assets/template.html`, `assets/views/*.js` | vendored byte-for-byte, not ported |
| `assets/vendor/d3.v7.9.0.min.js` | d3 v7.9.0 release, inlined by the offline transform |

## Fully offline output

Every HTML this crate produces — `render`, `render_graph_data`,
`render_multi_user`, `visualize` — works with no network: no external script,
stylesheet, font, image or fetch. The vendored `template.html` loads d3 from
`https://d3js.org` and the Inter webfont from Google Fonts, exactly as Python
does, so `src/html.rs` applies an **offline transform** to the shell before any
token is substituted. It is the **single deliberate divergence** from upstream:

- the exact tag `<script src="https://d3js.org/d3.v7.min.js"></script>` is
  replaced by `<script>` + the bundled `assets/vendor/d3.v7.9.0.min.js` +
  `</script>` (~280 KB);
- the three exact Google Fonts `<link>` lines (two `preconnect`s and the
  `css2?family=Inter` stylesheet) are deleted. The template's CSS already falls
  back to a locally installed Inter (`src: local('Inter')`) and then
  `system-ui`, so no font is bundled.

Each pattern must occur **exactly once** in the template, or rendering fails
with `VisualizationError::OfflineTransform`. A resync that changes those tags
therefore breaks loudly instead of silently re-enabling the CDN. The vendored
files themselves stay byte-identical to upstream.

`tests/offline_test.rs` pins this on an empty graph, a non-trivial graph, a
table dataset and `render_multi_user`: it locates the inline d3 by its exact
bundled content and checks its SHA-256, scans the rest of the page for any
resource-loading construct (`<script src`, `<link href`, non-`data:` `<img>`,
`<iframe>`/`<object>`/`<embed>`, CSS `url(http`/`url(//`/`@import`, `fetch(`,
`XMLHttpRequest`, `WebSocket`, `EventSource`, `sendBeacon`, `importScripts`),
and checks that every `http(s)://` substring in the whole document is one of a
fixed allowlist (d3's banner comment and the five w3.org XML namespace URIs).

## The four tabs

| Tab | Rendered by | State in Rust |
| --- | --- | --- |
| **Graph** | `views/story_view.js` | Full. Canvas renderer with Story / Flow / Force layouts, quadtree hit-testing, minimap, label budget, density layers, search and hover. The Flow layout keys off the preprocessor's `visual_rank` (the runtime-stamped `topological_rank` when present, otherwise a stage-order fallback). |
| **Schema** | `views/schema_view.js` + `views/inspector.js` | Full. Ontology diagram of node types in canonical pipeline order, labelled relationship curves, edge-type cards, and a side-panel inspector with type → instance drill-down. Fed by the preprocessor's derived type-schema graph, so it works without any caller-supplied schema. A DLT/SQL dataset (`SchemaTable` / `TableType` nodes) is drawn as one card per table in a "Tables" column, with its foreign keys as relationship edges. The **Transformations** overlay is fed by the operations catalog: hand-curated rows plus one generated row per improve stage (`crates/lib/tests/visualization_improve_stages.rs` keeps those in step with the improve registry). |
| **Memory** | `views/memory_map.js` | Renders. Deterministic column map: documents (with ordered chunk cells) → entity-type groups → summaries → global context, plus the run timeline the preprocessor gap-clusters from each node's `t_created`. Only the *search-retrieval* half of the timeline rail (and the recall overlay) is inert, because `__SEARCH_EVENTS__` is empty (see Deferred). |
| **Semantic** | `views/semantic_map.js` | Empty state only. See Deferred. |

## The 20 tokens

The vendored shell and JS chunks declare 20 `__TOKEN__` slots.
`src/html.rs::build_html` substitutes all of them, and **the order is
load-bearing**: the eight JS chunks go first, because ten of the twelve data
tokens live *inside* those chunks and would otherwise never be reached.

**1. JS chunks** (in this order):

| Token | Filled with |
| --- | --- |
| `__UI_CHROME_JS__` | `assets/views/ui_chrome.js` |
| `__SCHEMA_VIEW_JS__` | `assets/views/schema_view.js` |
| `__STORY_VIEW_JS__` | `assets/views/story_view.js` |
| `__PIPELINE_LAYOUT_JS__` | the **empty string** — upstream `layouts/pipeline_layout.py:16-17` is a deliberate stub whose `emit_js()` returns `""`. The slot is kept, not filled with invented content. |
| `__INSPECTOR_JS__` | `assets/views/inspector.js` |
| `__MEMORY_VIEW_JS__` | `assets/views/memory_map.js` |
| `__SEMANTIC_LAYOUT_JS__` | the one line `window._semanticPositions = __SEMANTIC_POSITIONS__;` (upstream `layouts/semantic_layout.py:200-206`) — which is why this chunk must land before the data tokens |
| `__SEMANTIC_VIEW_JS__` | `assets/views/semantic_map.js` |

**2. Data payloads** (after the chunks):

| Token | Filled with |
| --- | --- |
| `__NODES_DATA__` | `pre.nodes` |
| `__LINKS_DATA__` | `pre.links` |
| `__TASK_COLORS__` | `pre.color_maps.task` |
| `__PIPELINE_COLORS__` | `pre.color_maps.pipeline` |
| `__NODESET_COLORS__` | `pre.color_maps.node_set` |
| `__USER_COLORS__` | `pre.color_maps.user` |
| `__SCHEMA_DATA__` | the caller-supplied opaque passthrough, or the literal `null` |
| `__SCHEMA_GRAPH_DATA__` | `pre.schema_graph`, falling back to `{"nodes":[],"links":[]}` |
| `__MEMORY_DATA__` | `pre.memory_map`, falling back to `{}` |
| `__SEARCH_EVENTS__` | the literal `[]` (deferred) |
| `__SEMANTIC_POSITIONS__` | the literal `null` (deferred) |
| `__SEMANTIC_CLUSTERS__` | the literal `null` (deferred) |

Every data token is substituted **unconditionally, with a JSON fallback**. A
leaked `__TOKEN__` is a hard failure — it lands as literal garbage inside a
`<script>` block and breaks the page — so `tests/html_test.rs` scans the whole
output for any surviving `__[A-Z][A-Z0-9_]*__`.

All embedded JSON goes through `safe_json_embed`: `serde_json::to_string`, then
`<` / `>` / `&` / U+2028 / U+2029 become the JSON escapes `\u003c` / `\u003e` /
`\u0026` / `\u2028` / `\u2029` (serde emits these characters only inside string
literals, where the escape decodes to the same value). That is the only XSS
guard on the payload. Upstream's `_safe_json_embed` only rewrites `</` → `<\/`,
which stops a `</script>` in a node `name` from closing the block but not a
`<!--` followed by `<script` (e.g. a chunk of an ingested HTML file): that pair
puts the HTML tokenizer into the script-data-double-escaped state, the
template's own `</script>` no longer closes the block, and the page renders
blank. The inlined d3 is likewise refused if it ever contains `</script`,
`<!--` or `<script`. `tests/offline_test.rs` renders hostile data and asserts
it stays inert.

## Vendored assets: re-sync, never hand-edit

`assets/template.html` and `assets/views/*.js` are byte-for-byte copies of the
upstream Python files. **Do not hand-edit them.** Change the frontend upstream,
then re-copy the whole set and bump the recorded commit in
[`assets/README.md`](assets/README.md), which holds the source paths and the
resync script.

This rule exists because it was broken once already: Rust used to carry
`assets/graph_template.html`, a 1680-line hand-fork of Python's pre-refactor
monolith. Edited in place, it drifted silently and fell several features behind
(two tabs instead of four, no Memory or Semantic view, no layout switch, no
inspector, and a Schema tab that could never render). `tests/html_test.rs`
asserts each chunk's distinctive identifiers survived the copy, so a bad or
partial resync fails the test suite instead of shipping quietly.

The offline transform (see [Fully offline output](#fully-offline-output))
rewrites the d3 and webfont tags at render time; it never edits the vendored
files.

## Deferred

- **Semantic tab** — `__SEMANTIC_POSITIONS__` and `__SEMANTIC_CLUSTERS__` are
  both the literal `null`, so `semantic_map.js` shows its friendly empty state.
  Filling them needs each node's stored embedding vector, but Rust's
  `cognee_vector::SearchResult` has no `vector` field so stored vectors cannot be
  read back at all; a projection (PCA/SVD) and k-means seeded to match NumPy's
  PCG64 would also be required. This is the same state Python reaches whenever
  its own best-effort `_semantic_payload` raises, so it is a legal, upstream
  test-passing configuration.
- **Session events** — `__SEARCH_EVENTS__` is the literal `[]`. There is no Rust
  counterpart to Python's `visualization/session_events.py`, so the Semantic
  recall overlay has nothing to display and the Memory timeline rail shows only
  its run events (`memory_map.js:464-471` concatenates the two sources). The run
  events themselves are *not* deferred: they are gap-clustered from each node's
  `t_created`, which the graph adapters surface as the `DataPoint`'s epoch-ms
  `created_at` (`crates/graph/tests/common/mod.rs::
  test_get_graph_data_surfaces_created_at` pins that contract).
- **Subgraph bounding** — Python's `subgraph_data.py` (bounded per-dataset
  subgraph extraction) is not ported; Rust always renders the whole graph
  returned by `get_graph_data()`.

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
  `source_user` attribute so the renderer can color-code by user; edges are
  deduplicated by `(source, target, relation)`. Mirrors Python's
  `aggregate_multi_user_graphs()`.
- `preprocess(nodes, edges, schema_data) -> PreprocessedGraph` — the renderer
  snapshot on its own, re-exported from the public `preprocessor` module (Python
  `preprocessor.preprocess`). Exposed so callers can inspect or post-process the
  enriched nodes/links/color-maps without going through the HTML shell; the
  parity tests drive it directly.

Errors are surfaced via `VisualizationError`.
