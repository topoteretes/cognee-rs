#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Validate HTML placeholder substitution and content.

use cognee_graph::GraphDBTrait;
use cognee_graph::MockGraphDB;
use cognee_visualization::render;

const PLACEHOLDERS: [&str; 7] = [
    "__NODES_DATA__",
    "__LINKS_DATA__",
    "__TASK_COLORS__",
    "__PIPELINE_COLORS__",
    "__NODESET_COLORS__",
    "__USER_COLORS__",
    "__SCHEMA_DATA__",
];

#[tokio::test]
async fn render_leaves_no_placeholders_behind() {
    let db = MockGraphDB::new();
    let html = render(&db).await.expect("render empty graph");
    for p in PLACEHOLDERS {
        assert!(
            !html.contains(p),
            "placeholder {p} still present in rendered HTML"
        );
    }
}

#[tokio::test]
async fn render_empty_graph_embeds_empty_arrays() {
    let db = MockGraphDB::new();
    let html = render(&db).await.expect("render empty graph");
    // Literal JSON `[]` should be present where nodes/edges are embedded.
    assert!(html.contains("[]"), "empty array literal not found");
}

#[tokio::test]
async fn render_escapes_closing_script_in_data() {
    let db = MockGraphDB::new();
    // A name containing `</script>` should be escaped so the embedded
    // <script> block isn't prematurely terminated.
    db.add_node_raw(serde_json::json!({
        "id": "evil",
        "type": "Entity",
        "name": "hi</script>bye",
    }))
    .await
    .expect("MockGraphDB accepts valid node JSON");
    let html = render(&db).await.expect("render succeeds");
    assert!(html.contains("hi<\\/script>bye"));
}

#[tokio::test]
async fn render_contains_d3_script_tag() {
    let db = MockGraphDB::new();
    let html = render(&db).await.expect("render succeeds");
    // d3.js is loaded from CDN in the template.
    assert!(html.contains("d3.v7.min.js"));
    assert!(html.contains("<!DOCTYPE html>"));
}
