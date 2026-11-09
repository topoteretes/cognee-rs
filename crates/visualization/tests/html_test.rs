#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Orchestrator assembly contract.
//!
//! Mirrors Python's
//! [`cognee/tests/unit/modules/visualization/test_orchestrator_assembly.py`](https://github.com/topoteretes/cognee/blob/main/cognee/tests/unit/modules/visualization/test_orchestrator_assembly.py).
//!
//! These tests do not assert renderer *behavior* — they pin the assembly
//! contract: the vendored HTML shell is read, every view's JS chunk is injected
//! into the right slot, every data token is substituted with valid JSON, and no
//! `__TOKEN__` placeholder leaks into the output.

use cognee_graph::GraphDBTrait;
use cognee_graph::MockGraphDB;
use cognee_visualization::render;

/// The minimal graph Python's assembly test uses: one `Entity`, one
/// `DocumentChunk`, one `contains` edge between them.
async fn minimal_graph() -> MockGraphDB {
    let db = MockGraphDB::new();
    db.add_node_raw(serde_json::json!({"id": "a", "type": "Entity", "name": "A"}))
        .await
        .expect("add node a");
    db.add_node_raw(serde_json::json!({"id": "b", "type": "DocumentChunk", "text": "hi"}))
        .await
        .expect("add node b");
    db.add_edge("b", "a", "contains", None)
        .await
        .expect("add edge b -> a");
    db
}

/// Hand-rolled equivalent of Python's `re.findall(r"__[A-Z][A-Z0-9_]*__", html)`.
///
/// The crate deliberately carries no `regex` dependency, so we scan by hand: at
/// every `__` followed by an ASCII uppercase letter, walk the maximal run of
/// `[A-Z0-9_]` and report a leak as soon as a `__` terminator is reached. That
/// picks the leftmost-shortest terminator where Python's greedy `*` picks the
/// rightmost one, so the reported span can be shorter — but the *presence* of a
/// leak is detected identically, which is all the `leaks == []` assertion needs.
fn find_leaked_tokens(html: &str) -> Vec<String> {
    let bytes = html.as_bytes();
    let mut leaks = Vec::new();
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        let starts_token =
            bytes[i] == b'_' && bytes[i + 1] == b'_' && bytes[i + 2].is_ascii_uppercase();
        if !starts_token {
            i += 1;
            continue;
        }
        // The star region begins right after the mandatory leading `[A-Z]`.
        let mut j = i + 3;
        let mut end = None;
        while j + 1 < bytes.len() {
            let c = bytes[j];
            if !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_') {
                break;
            }
            if c == b'_' && bytes[j + 1] == b'_' {
                end = Some(j + 2);
                break;
            }
            j += 1;
        }
        match end {
            Some(e) => {
                leaks.push(html[i..e].to_string());
                i = e;
            }
            None => i += 1,
        }
    }
    leaks
}

#[tokio::test]
async fn orchestrator_returns_full_html() {
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    assert!(html.starts_with("<!DOCTYPE html>"), "missing doctype");
    assert!(html.trim_end().ends_with("</html>"), "truncated document");
}

#[tokio::test]
async fn no_token_placeholders_leak() {
    // Any `__SOMETHING__` left in the output is an unfilled token slot and means
    // a view or data substitution was missed (Python
    // `test_no_token_placeholders_leak`).
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    let leaks = find_leaked_tokens(&html);
    assert!(leaks.is_empty(), "unfilled tokens: {leaks:?}");
}

#[tokio::test]
async fn no_token_placeholders_leak_on_empty_graph() {
    // The empty graph exercises every JSON fallback (`{}`, `[]`, `null`) at
    // once — the path most likely to leave a token behind.
    let db = MockGraphDB::new();
    let html = render(&db).await.expect("render empty graph");
    let leaks = find_leaked_tokens(&html);
    assert!(
        leaks.is_empty(),
        "unfilled tokens on empty graph: {leaks:?}"
    );
}

#[tokio::test]
async fn all_view_modules_contribute() {
    // Spot-check a distinctive identifier from every vendored JS chunk. A bad or
    // partial asset resync fails here rather than shipping a broken page.
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");

    // ui_chrome.js: theme toggle
    assert!(html.contains("_isLightMode"), "ui_chrome.js missing");
    // schema_view.js: the D3 schema diagram renderer
    assert!(
        html.contains("_renderSchemaGraph"),
        "schema_view.js missing"
    );
    // schema_view.js: the "Tables" column for SQL/DLT tables (SCHEMA_TABLE_RANK).
    assert!(
        html.contains("'6': 'Tables'"),
        "schema_view.js is missing the Tables column label"
    );
    // story_view.js: canvas renderer, label budget, and the data globals the
    // Memory/Semantic tabs read back out of it.
    assert!(
        html.contains("computeRankedLayout"),
        "story_view.js layout missing"
    );
    assert!(
        html.contains("labelBudget"),
        "story_view.js label budget missing"
    );
    assert!(
        html.contains("window._vizNodeById"),
        "story_view.js node index global missing"
    );
    assert!(
        html.contains("window._vizLinks"),
        "story_view.js link global missing"
    );
    // memory_map.js: lazy-render entry point for the Memory tab
    assert!(html.contains("_renderMemoryView"), "memory_map.js missing");
    // semantic_map.js: lazy-render entry point for the Semantic tab
    assert!(
        html.contains("_renderSemanticView"),
        "semantic_map.js missing"
    );
    // inspector.js: the three globals it installs. We match the *definitions*,
    // which exist only in inspector.js — schema_view.js merely calls them.
    for global in [
        "window._showSchemaInspector = function",
        "window._hideSchemaInspector = function",
        "window._showSchemaInstanceInspector = function",
    ] {
        assert!(html.contains(global), "inspector.js missing `{global}`");
    }
}

#[tokio::test]
async fn pipeline_layout_slot_is_filled_with_nothing() {
    // Upstream `layouts/pipeline_layout.emit_js()` returns "" — the slot must
    // still be consumed so nothing leaks, but no JS is contributed.
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    assert!(!html.contains("__PIPELINE_LAYOUT_JS__"));
}

#[tokio::test]
async fn data_tokens_resolve_to_json_literals() {
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    // The node payload carries the name we put in (serde emits compact JSON, so
    // there is no space after the colon — unlike Python's `json.dumps` default).
    assert!(
        html.contains("\"name\":\"A\""),
        "node `A` name not embedded"
    );
    // Color maps default to `{}` when no provenance is set; the JS variable is
    // still declared.
    assert!(html.contains("taskColors"), "taskColors global missing");
}

#[tokio::test]
async fn schema_data_is_null_when_omitted() {
    // `render()` passes no caller-supplied schema, so the orchestrator emits the
    // literal `null` — `schema_view.js` tests for it.
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    assert!(
        html.contains("const schemaData = null"),
        "expected `const schemaData = null`"
    );
}

#[tokio::test]
async fn memory_and_search_event_tokens_are_substituted() {
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    // `__MEMORY_DATA__` resolves to the memory-map JSON object…
    assert!(
        html.contains("const memoryMap = {"),
        "expected `const memoryMap = {{`"
    );
    // …and `__SEARCH_EVENTS__` falls back to an empty JSON array (deferred: no
    // session-event source in Rust yet).
    assert!(
        html.contains("const searchEvents = []"),
        "expected `const searchEvents = []`"
    );
}

#[tokio::test]
async fn semantic_tokens_are_null() {
    // Deferred payloads: both semantic tokens are the literal `null`, which is
    // the same state Python reaches when `_semantic_payload` raises.
    // `semantic_map.js` then renders its friendly empty state.
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    assert!(
        html.contains("window._semanticPositions = null;"),
        "expected the semantic layout chunk to pin `null` positions"
    );
    assert!(
        html.contains("const CLUSTERS = null"),
        "expected `const CLUSTERS = null`"
    );
}

#[tokio::test]
async fn memory_view_scaffolding_is_wired() {
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    // Containers live in the vendored template.html.
    for id in [
        "data-view=\"memory\"",
        "id=\"memory-view\"",
        "id=\"memory-svg\"",
        "id=\"memory-timeline\"",
        "id=\"memory-side-panel\"",
        "id=\"memory-zoom-fit\"",
        "id=\"memory-empty\"",
    ] {
        assert!(html.contains(id), "template.html missing `{id}`");
    }
    // memory_map.js: deterministic layout + overlay machinery.
    assert!(html.contains("computeLayout"));
    assert!(html.contains("mm-searching"));
}

#[tokio::test]
async fn preprocessor_enrichment_reaches_html() {
    // The JS-facing payload must carry the preprocessor-derived fields the
    // renderer reads (Python `test_preprocessor_enrichment_reaches_html`).
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render minimal graph");
    for field in [
        "\"stage\":",          // per-node pipeline stage
        "\"label_priority\":", // per-node label budget flag
        "\"visual_rank\":",    // topological_rank, or the stage-order fallback
        "\"edge_class\":",     // per-link structural/semantic classification
    ] {
        assert!(
            html.contains(field),
            "preprocessor field `{field}` missing from the embedded payload"
        );
    }
}

#[tokio::test]
async fn render_escapes_closing_script_in_data() {
    // A node name containing `</script>` must not be able to terminate the
    // embedded <script> block — `safe_json_embed` escapes `<` / `>` as
    // `\u003c` / `\u003e`.
    let db = MockGraphDB::new();
    db.add_node_raw(serde_json::json!({
        "id": "evil",
        "type": "Entity",
        "name": "hi</script>bye",
    }))
    .await
    .expect("MockGraphDB accepts valid node JSON");
    let html = render(&db).await.expect("render succeeds");
    assert!(
        html.contains("hi\\u003c/script\\u003ebye"),
        "`</script>` in node data was not escaped"
    );
    assert!(
        !html.contains("hi</script>bye"),
        "raw `</script>` leaked into the document"
    );
}

#[tokio::test]
async fn render_contains_d3_script_tag_and_all_four_tabs() {
    let db = minimal_graph().await;
    let html = render(&db).await.expect("render succeeds");
    // d3 v7.9.0 is inlined by the offline transform; nothing is fetched.
    assert!(html.contains("d3js.org v7.9.0"));
    assert!(!html.contains("<script src="));
    for view in ["graph", "schema", "memory", "semantic"] {
        assert!(
            html.contains(&format!("data-view=\"{view}\"")),
            "tab button for `{view}` missing"
        );
    }
}

/// Pull the compact JSON literal a vendored chunk assigns to `prefix`.
///
/// `safe_json_embed` emits single-line JSON, so the payload is the remainder of
/// the declaration line minus the trailing `;`.
fn embedded_json(html: &str, prefix: &str) -> serde_json::Value {
    let line = html
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no line starting with `{prefix}`"));
    let literal = line[prefix.len()..].trim().trim_end_matches(';');
    serde_json::from_str(literal)
        .unwrap_or_else(|e| panic!("`{prefix}` payload is not valid JSON: {e}\n{literal}"))
}

/// Regression: a payload that *contains* a token name must not be rewritten.
///
/// `no_token_placeholders_leak` cannot catch this — its fixtures carry no token
/// text, so nothing is there to be corrupted. Sequential `String::replace` over
/// the whole document rewrote the `__SCHEMA_GRAPH_DATA__` / `__MEMORY_DATA__`
/// literals *inside* the already-embedded node JSON, which broke the JSON (
/// `expected ',' or '}'`) and blanked every tab. This crate's own
/// `assets/README.md` quotes those token names, so cognifying this repository
/// was enough to trigger it.
#[tokio::test]
async fn payload_containing_token_names_survives_verbatim() {
    // Two tokens that are substituted *after* `__NODES_DATA__`, plus one from
    // the JS-chunk phase for good measure.
    const TEXT: &str = "the tokens __SCHEMA_GRAPH_DATA__, __MEMORY_DATA__ and \
                        __SEMANTIC_LAYOUT_JS__ are documented in assets/README.md";

    let db = MockGraphDB::new();
    db.add_node_raw(serde_json::json!({
        "id": "readme",
        "type": "DocumentChunk",
        "name": "assets/README.md",
        "text": TEXT,
        "chunk_index": 0,
    }))
    .await
    .expect("add readme chunk");
    db.add_node_raw(serde_json::json!({"id": "e", "type": "Entity", "name": "Token"}))
        .await
        .expect("add entity");
    db.add_edge("readme", "e", "contains", None)
        .await
        .expect("add edge");

    let html = render(&db).await.expect("render token-quoting graph");

    // 1) The node payload must still be parseable JSON…
    let nodes = embedded_json(&html, "var nodes = ");
    // …and the text must have survived byte-for-byte.
    let readme = nodes
        .as_array()
        .expect("nodes payload is an array")
        .iter()
        .find(|node| node["id"] == "readme")
        .expect("readme node in payload");
    assert_eq!(
        readme["text"].as_str(),
        Some(TEXT),
        "node text was rewritten by a later token substitution"
    );

    // 2) The tokens that follow `__NODES_DATA__` must still have been filled in
    //    their own slots — the fix must not stop substituting, only stop
    //    re-scanning.
    assert!(
        embedded_json(&html, "const memoryMap = ").is_object(),
        "`__MEMORY_DATA__` slot did not resolve to a JSON object"
    );
    assert!(
        embedded_json(&html, "const schemaGraphData = ").is_object(),
        "`__SCHEMA_GRAPH_DATA__` slot did not resolve to a JSON object"
    );
    assert!(
        html.contains("window._semanticPositions = null;"),
        "`__SEMANTIC_LAYOUT_JS__` slot did not resolve"
    );
}

/// Sanity check on the hand-rolled scanner itself: it must catch a real token and
/// must not fire on lowercase `__dunder__` text or single-underscore names.
#[test]
fn token_scanner_detects_leaks() {
    assert_eq!(
        find_leaked_tokens("prefix __NODES_DATA__ suffix"),
        vec!["__NODES_DATA__".to_string()]
    );
    assert!(find_leaked_tokens("__lowercase__ and _SINGLE_ and plain").is_empty());
}
