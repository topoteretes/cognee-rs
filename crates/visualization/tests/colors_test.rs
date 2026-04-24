//! Integration-level assertions for the public behavior of the colors
//! sub-module. Because the color helpers are module-private, these tests
//! exercise them indirectly by asserting the shape of the final rendered
//! HTML output.

use cognee_graph::GraphDBTrait;
use cognee_graph::MockGraphDB;
use cognee_visualization::render;

#[tokio::test]
async fn rendered_html_uses_entity_color_for_entity_nodes() {
    let db = MockGraphDB::new();
    db.add_node_raw(serde_json::json!({
        "id": "n1",
        "type": "Entity",
        "name": "Alice",
    }))
    .await
    .expect("MockGraphDB accepts valid node JSON");

    let html = render(&db).await.expect("render succeeds");
    // Escaped inline JSON inside the embedded <script> block.
    assert!(html.contains("\"color\":\"#6510F4\""));
}

#[tokio::test]
async fn rendered_html_uses_ontology_override() {
    let db = MockGraphDB::new();
    db.add_node_raw(serde_json::json!({
        "id": "n2",
        "type": "Entity",
        "ontology_valid": true,
    }))
    .await
    .expect("MockGraphDB accepts valid node JSON");

    let html = render(&db).await.expect("render succeeds");
    assert!(html.contains("\"color\":\"#D8D8D8\""));
    assert!(!html.contains("\"color\":\"#6510F4\""));
}

#[tokio::test]
async fn rendered_html_contains_provenance_color_maps_block() {
    // With no provenance data, all four color maps serialize to {}.
    let db = MockGraphDB::new();
    let html = render(&db).await.expect("render succeeds");

    // None of the placeholders should remain, and the empty-color-map
    // JSON object must appear (at least once per provenance dimension).
    for placeholder in [
        "__TASK_COLORS__",
        "__PIPELINE_COLORS__",
        "__NODESET_COLORS__",
        "__USER_COLORS__",
    ] {
        assert!(
            !html.contains(placeholder),
            "placeholder {placeholder} still present"
        );
    }
    assert!(html.contains("{}"));
}
