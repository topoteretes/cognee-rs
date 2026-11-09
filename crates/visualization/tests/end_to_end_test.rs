#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! End-to-end: drive a `MockGraphDB` through `render()` / `visualize()` and
//! assert the *preprocessor-enriched* payload actually reaches the HTML.
//!
//! `tests/html_test.rs` pins the assembly contract (no token leaks, every chunk
//! injected). This file pins the data path: the enrichment the Rust preprocessor
//! computes must survive JSON embedding and land in the JS globals the vendored
//! `views/*.js` read.

use cognee_graph::GraphDBTrait;
use cognee_graph::MockGraphDB;
use cognee_visualization::{render, visualize};

/// Extract a `<decl> <name> = <json>;` payload out of the rendered HTML.
///
/// The vendored chunks inline every payload as a JS variable assignment
/// (`var nodes = […];`, `const schemaGraphData = {…};`). We locate the marker
/// then scan forward counting brackets until balanced — string-aware, so
/// brackets inside JSON strings do not confuse the depth counter. This mirrors
/// the cross-SDK harness's regex extractor without pulling in a `regex` dep.
fn extract_js_value(html: &str, marker: &str) -> serde_json::Value {
    let start = html
        .find(marker)
        .unwrap_or_else(|| panic!("marker {marker:?} not found in HTML"));
    let body = &html[start + marker.len()..];
    let bytes = body.as_bytes();
    let (open, close) = match bytes[0] {
        b'[' => (b'[', b']'),
        b'{' => (b'{', b'}'),
        other => panic!("unexpected JSON opener {:?} for {marker:?}", other as char),
    };
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if *b == b'\\' {
                esc = true;
            } else if *b == b'"' {
                in_str = false;
            }
            continue;
        }
        match *b {
            b'"' => in_str = true,
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(end > 0, "could not balance the payload for {marker:?}");
    let raw = &body[..end];
    // Undo the `</` → `<\/` script-escape before parsing.
    let unescaped = raw.replace("<\\/", "</");
    serde_json::from_str(&unescaped)
        .unwrap_or_else(|e| panic!("parse {marker:?} JSON: {e}: raw={raw:?}"))
}

fn node_by_id<'a>(nodes: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    nodes
        .as_array()
        .expect("nodes payload is an array")
        .iter()
        .find(|n| n.get("id").and_then(|v| v.as_str()) == Some(id))
        .unwrap_or_else(|| panic!("node {id:?} missing from the embedded payload"))
}

/// A small but structurally complete graph: a document, a chunk, an entity, an
/// entity type, and one structural + one semantic edge. One node carries an
/// explicit `topological_rank`, one deliberately does not.
async fn enriched_graph() -> MockGraphDB {
    let db = MockGraphDB::new();
    db.add_node_raw(serde_json::json!({
        "id": "doc1",
        "type": "TextDocument",
        "name": "Report.txt",
        "source_pipeline": "cognify_pipeline",
    }))
    .await
    .expect("add doc1");
    db.add_node_raw(serde_json::json!({
        "id": "chunk1",
        "type": "DocumentChunk",
        "text": "Alice works on graphs.",
        "source_task": "extract_chunks",
    }))
    .await
    .expect("add chunk1");
    // `topological_rank: 3` must win over the stage-order fallback, which for
    // stage `type` (STAGE_ORDER index 3) would otherwise be 4.
    db.add_node_raw(serde_json::json!({
        "id": "ranked",
        "type": "EntityType",
        "name": "Person",
        "topological_rank": 3,
    }))
    .await
    .expect("add ranked");
    db.add_node_raw(serde_json::json!({
        "id": "alice",
        "type": "Entity",
        "name": "Alice",
        "source_task": "extract_graph",
    }))
    .await
    .expect("add alice");

    // `is_a` is in the structural relation set → edge_class "structural".
    db.add_edge("alice", "ranked", "is_a", None)
        .await
        .expect("add is_a edge");
    // `mentions` is not → edge_class "semantic".
    db.add_edge(
        "chunk1",
        "alice",
        "mentions",
        Some(
            [(
                std::borrow::Cow::from("weight"),
                serde_json::Value::from(0.75),
            )]
            .into_iter()
            .collect(),
        ),
    )
    .await
    .expect("add mentions edge");
    db
}

#[tokio::test]
async fn stamped_topological_rank_becomes_visual_rank() {
    let db = enriched_graph().await;
    let html = render(&db).await.expect("render enriched graph");
    let nodes = extract_js_value(&html, "var nodes = ");

    assert_eq!(
        node_by_id(&nodes, "ranked").get("visual_rank"),
        Some(&serde_json::Value::from(3)),
        "a node carrying topological_rank: 3 must render visual_rank 3"
    );
    // Belt and braces: the literal must be in the HTML text too.
    assert!(
        html.contains("\"visual_rank\":3"),
        "expected the literal `\"visual_rank\":3` in the embedded payload"
    );
}

#[tokio::test]
async fn nodes_without_topological_rank_fall_back_to_stage_order() {
    let db = enriched_graph().await;
    let html = render(&db).await.expect("render enriched graph");
    let nodes = extract_js_value(&html, "var nodes = ");

    // STAGE_ORDER = (document, chunk, entity, type, summary, context, schema,
    // other); the fallback is `index + 1`.
    for (id, expected_stage, expected_rank) in [
        ("doc1", "document", 1),
        ("chunk1", "chunk", 2),
        ("alice", "entity", 3),
    ] {
        let node = node_by_id(&nodes, id);
        assert_eq!(
            node.get("stage").and_then(|v| v.as_str()),
            Some(expected_stage),
            "node {id} stage"
        );
        assert_eq!(
            node.get("visual_rank"),
            Some(&serde_json::Value::from(expected_rank)),
            "node {id} should fall back to the stage-order rank"
        );
    }
}

#[tokio::test]
async fn edges_are_classified_structural_or_semantic() {
    let db = enriched_graph().await;
    let html = render(&db).await.expect("render enriched graph");
    let links = extract_js_value(&html, "var links = ");
    let arr = links.as_array().expect("links payload is an array");

    let is_a = arr
        .iter()
        .find(|l| l.get("relation").and_then(|v| v.as_str()) == Some("is_a"))
        .expect("is_a link present");
    assert_eq!(
        is_a.get("edge_class").and_then(|v| v.as_str()),
        Some("structural"),
        "`is_a` is a structural relation"
    );

    let mentions = arr
        .iter()
        .find(|l| l.get("relation").and_then(|v| v.as_str()) == Some("mentions"))
        .expect("mentions link present");
    assert_eq!(
        mentions.get("edge_class").and_then(|v| v.as_str()),
        Some("semantic"),
        "`mentions` is not in the structural relation set"
    );
    // Edge weights survive the round trip under `all_weights.default`.
    assert!(
        html.contains("\"all_weights\":{\"default\":0.75"),
        "edge weight missing from the embedded payload"
    );
}

#[tokio::test]
async fn schema_graph_carries_graph_node_type_cards() {
    let db = enriched_graph().await;
    let html = render(&db).await.expect("render enriched graph");
    // `__SCHEMA_GRAPH_DATA__` is substituted into schema_view.js.
    let schema_graph = extract_js_value(&html, "const schemaGraphData = ");
    let schema_nodes = schema_graph
        .get("nodes")
        .and_then(|v| v.as_array())
        .expect("schemaGraphData.nodes is an array");

    let type_cards: Vec<&serde_json::Value> = schema_nodes
        .iter()
        .filter(|n| n.get("type").and_then(|v| v.as_str()) == Some("GraphNodeType"))
        .collect();
    assert!(
        !type_cards.is_empty(),
        "expected at least one GraphNodeType card, got {schema_nodes:#?}"
    );
    // Every card names a node type and reports an instance count.
    for card in &type_cards {
        assert!(
            card.get("name").and_then(|v| v.as_str()).is_some(),
            "GraphNodeType card without a name: {card:#?}"
        );
        assert!(
            card.get("instance_count").is_some(),
            "GraphNodeType card without instance_count: {card:#?}"
        );
    }
}

#[tokio::test]
async fn all_four_tabs_are_present() {
    let db = enriched_graph().await;
    let html = render(&db).await.expect("render enriched graph");
    for view in ["graph", "schema", "memory", "semantic"] {
        assert!(
            html.contains(&format!("data-view=\"{view}\"")),
            "tab button for the `{view}` view is missing"
        );
    }
}

#[tokio::test]
async fn visualize_writes_html_file_to_provided_path() {
    let db = enriched_graph().await;
    let tmp = tempfile::tempdir().expect("create tempdir");
    let dest = tmp.path().join("graph.html");
    let written = visualize(&db, Some(&dest))
        .await
        .expect("visualize succeeds");
    assert_eq!(written, dest, "returned path matches input");

    let html = std::fs::read_to_string(&dest).expect("read generated HTML");
    assert!(html.starts_with("<!DOCTYPE html>"));
    // d3 is inlined by the offline transform, never fetched.
    assert!(html.contains("d3js.org v7.9.0"));
    assert!(!html.contains("d3.v7.min.js"));

    // Data round-trip: every node id reaches the embedded payload.
    for id in ["doc1", "chunk1", "ranked", "alice"] {
        assert!(
            html.contains(&format!("\"id\":\"{id}\"")),
            "node {id} missing from the written file"
        );
    }
}

#[tokio::test]
async fn visualize_creates_missing_parent_directories() {
    let db = MockGraphDB::new();
    let tmp = tempfile::tempdir().expect("create tempdir");
    let dest = tmp.path().join("nested").join("sub").join("graph.html");
    assert!(!dest.parent().expect("has parent").exists());

    let written = visualize(&db, Some(&dest))
        .await
        .expect("visualize succeeds");
    assert_eq!(written, dest);
    assert!(dest.exists());
}

#[tokio::test]
async fn visualize_empty_graph_produces_valid_html() {
    let db = MockGraphDB::new();
    let tmp = tempfile::tempdir().expect("create tempdir");
    let dest = tmp.path().join("empty.html");
    visualize(&db, Some(&dest))
        .await
        .expect("visualize succeeds");

    let html = std::fs::read_to_string(&dest).expect("read HTML");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.trim_end().ends_with("</html>"));
}
