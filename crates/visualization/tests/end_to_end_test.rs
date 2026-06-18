#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! End-to-end test: populate a `MockGraphDB`, call `visualize()`, and assert
//! that a valid HTML file is written to disk.

use cognee_graph::GraphDBTrait;
use cognee_graph::MockGraphDB;
use cognee_visualization::visualize;

#[tokio::test]
async fn visualize_writes_html_file_to_provided_path() {
    let db = MockGraphDB::new();
    db.add_node_raw(serde_json::json!({
        "id": "n1",
        "type": "Entity",
        "name": "Alice",
        "source_task": "task-a",
        "source_pipeline": "pipe-1",
    }))
    .await
    .expect("add node n1");
    db.add_node_raw(serde_json::json!({
        "id": "n2",
        "type": "DocumentChunk",
        "name": "Chunk 1",
        "source_task": "task-b",
    }))
    .await
    .expect("add node n2");
    db.add_node_raw(serde_json::json!({
        "id": "n3",
        "type": "EntityType",
        "name": "Person",
    }))
    .await
    .expect("add node n3");
    db.add_edge(
        "n1",
        "n3",
        "is_a",
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
    .expect("add edge n1 -> n3");

    let tmp = tempfile::tempdir().expect("create tempdir");
    let dest = tmp.path().join("graph.html");
    let written = visualize(&db, Some(&dest))
        .await
        .expect("visualize succeeds");
    assert_eq!(written, dest, "returned path matches input");

    let html = std::fs::read_to_string(&dest).expect("read generated HTML");

    // Sanity checks: content should mention d3 and contain no placeholders.
    assert!(html.contains("d3.v7.min.js"));
    assert!(!html.contains("__NODES_DATA__"));
    assert!(!html.contains("__LINKS_DATA__"));
    assert!(!html.contains("__TASK_COLORS__"));
    assert!(!html.contains("__PIPELINE_COLORS__"));
    assert!(!html.contains("__NODESET_COLORS__"));
    assert!(!html.contains("__USER_COLORS__"));
    assert!(!html.contains("__SCHEMA_DATA__"));

    // Data round-trip: node IDs and expected color must be in HTML.
    assert!(html.contains("\"id\":\"n1\""));
    assert!(html.contains("\"id\":\"n2\""));
    assert!(html.contains("\"id\":\"n3\""));
    assert!(html.contains("\"color\":\"#6510F4\"")); // Entity
    assert!(html.contains("\"color\":\"#0DFF00\"")); // DocumentChunk
    assert!(html.contains("\"color\":\"#D5C2FF\"")); // EntityType
    // Edge weight appears both as `weight` and under `all_weights.default`.
    assert!(html.contains("\"all_weights\":{\"default\":0.75"));
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
