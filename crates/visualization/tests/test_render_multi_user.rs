#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for `render_multi_user` — the multi-user aggregation entry point used
//! by the visualize HTTP router's `POST /multi` endpoint.

use std::sync::Arc;

use cognee_graph::GraphDBTrait;
use cognee_graph::mock::MockGraphDB;

use cognee_visualization::render_multi_user;

/// Extract a top-level `var <name> = <json>;` payload from the rendered HTML.
///
/// The vendored `views/story_view.js` chunk declares the six graph payloads as
/// `var <name> = __TOKEN__;` at its top (`story_view.js:4-9`); the orchestrator
/// substitutes each token with JSON. `var nodes = ` / `var links = ` are unique
/// across the whole assembled document. We find the marker, then scan forward
/// counting `[`/`]` (or `{`/`}`) until balanced, mirroring the cross-SDK
/// harness's regex extractor without pulling in a regex dep.
fn extract_var(html: &str, name: &str) -> serde_json::Value {
    let marker = format!("var {name} = ");
    let start = html
        .find(&marker)
        .unwrap_or_else(|| panic!("marker {marker:?} not found in HTML"));
    let body = &html[start + marker.len()..];
    let bytes = body.as_bytes();
    let opener = bytes[0];
    let (open, close) = match opener {
        b'[' => (b'[', b']'),
        b'{' => (b'{', b'}'),
        other => panic!("unexpected JSON opener {:?} for var {name}", other as char),
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
    assert!(end > 0, "could not balance var {name} payload");
    let raw = &body[..end];
    // The script-escapes (`\u003c` & co.) are JSON escapes: parse verbatim.
    serde_json::from_str(raw).unwrap_or_else(|e| panic!("parse var {name} JSON: {e}: raw={raw:?}"))
}

async fn seed_three_nodes(db: &MockGraphDB, prefix: &str) {
    for i in 0..3 {
        db.add_node_raw(serde_json::json!({
            "id": format!("{prefix}-{i}"),
            "type": "Entity",
            "name": format!("{prefix}-{i}"),
        }))
        .await
        .expect("add node");
    }
}

#[tokio::test]
async fn aggregates_two_users_with_three_nodes_each() {
    let alice = MockGraphDB::new();
    seed_three_nodes(&alice, "alice").await;
    let bob = MockGraphDB::new();
    seed_three_nodes(&bob, "bob").await;

    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> = vec![
        ("alice@example.com".to_string(), Arc::new(alice)),
        ("bob@example.com".to_string(), Arc::new(bob)),
    ];

    let html = render_multi_user(&pairs).await.expect("render");

    // The vendored story view inlines node JSON via `__NODES_DATA__`. Three
    // nodes per user means each label appears at least three times.
    let alice_count = html.matches("alice@example.com").count();
    let bob_count = html.matches("bob@example.com").count();
    assert!(
        alice_count >= 3,
        "expected alice@example.com to appear at least 3 times, found {alice_count}"
    );
    assert!(
        bob_count >= 3,
        "expected bob@example.com to appear at least 3 times, found {bob_count}"
    );

    // Python emits `source_user` (not `user_id`) per node; assert the literal
    // attribute key is present in the embedded JSON.
    assert!(
        html.contains("\"source_user\""),
        "rendered HTML should carry the source_user attribute"
    );
}

#[tokio::test]
async fn empty_input_produces_valid_empty_html() {
    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> = vec![];
    let html = render_multi_user(&pairs).await.expect("render");
    // Valid HTML — at minimum the template wrapper survives.
    assert!(html.contains("<html") || html.contains("<!DOCTYPE"));
}

#[tokio::test]
async fn tags_each_node_with_owning_user_label() {
    let alice = MockGraphDB::new();
    alice
        .add_node_raw(serde_json::json!({"id": "n1", "type": "Entity"}))
        .await
        .expect("add");

    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> =
        vec![("alice@example.com".to_string(), Arc::new(alice))];

    let html = render_multi_user(&pairs).await.expect("render");
    // The source_user label should be the email for the single node.
    assert!(
        html.contains("alice@example.com"),
        "node missing source_user tag"
    );
}

#[tokio::test]
async fn dedupe_overlapping_nodes_first_write_wins() {
    // Two pairs share the node id "shared" by content; assert exactly one entry
    // survives, with `source_user` taken from the first pair (mirror Python
    // `cognee_network_visualization.py:211`).
    let alice = MockGraphDB::new();
    alice
        .add_node_raw(serde_json::json!({
            "id": "shared",
            "type": "Entity",
            "name": "shared-node",
        }))
        .await
        .expect("add alice shared");
    alice
        .add_node_raw(serde_json::json!({
            "id": "alice-only",
            "type": "Entity",
            "name": "alice-only",
        }))
        .await
        .expect("add alice-only");

    let bob = MockGraphDB::new();
    bob.add_node_raw(serde_json::json!({
        "id": "shared",
        "type": "Entity",
        "name": "shared-node",
    }))
    .await
    .expect("add bob shared");
    bob.add_node_raw(serde_json::json!({
        "id": "bob-only",
        "type": "Entity",
        "name": "bob-only",
    }))
    .await
    .expect("add bob-only");

    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> = vec![
        ("alice@example.com".to_string(), Arc::new(alice)),
        ("bob@example.com".to_string(), Arc::new(bob)),
    ];

    let html = render_multi_user(&pairs).await.expect("render");

    let arr_json = extract_var(&html, "nodes");
    let arr = arr_json.as_array().expect("nodes is an array");

    // Three unique ids across the two pairs: shared, alice-only, bob-only.
    assert_eq!(arr.len(), 3, "expected 3 deduplicated nodes, got {arr:#?}");

    let shared = arr
        .iter()
        .find(|n| n.get("id").and_then(|v| v.as_str()) == Some("shared"))
        .expect("shared node present");
    assert_eq!(
        shared.get("source_user").and_then(|v| v.as_str()),
        Some("alice@example.com"),
        "first-write-wins: shared node must carry alice's label, got {shared:?}"
    );
}

/// Python's guard is `if not node_info.get("source_user")`, i.e. *truthiness*:
/// every falsy value is "unlabelled" and gets overwritten with the owning user's
/// label. A present-but-falsy `source_user` that is left alone stays out of the
/// `userColors` map, so the node is never coloured in the colour-by-user overlay.
#[tokio::test]
async fn falsy_source_user_is_overwritten_with_the_owning_label() {
    let db = MockGraphDB::new();
    // Every falsy JSON shape Python's `not` accepts, plus the absent key.
    for (id, falsy) in [
        ("empty-string", serde_json::json!("")),
        ("zero", serde_json::json!(0)),
        ("zero-float", serde_json::json!(0.0)),
        ("false", serde_json::json!(false)),
        ("null", serde_json::Value::Null),
        ("empty-array", serde_json::json!([])),
        ("empty-object", serde_json::json!({})),
    ] {
        db.add_node_raw(serde_json::json!({
            "id": id,
            "type": "Entity",
            "name": id,
            "source_user": falsy,
        }))
        .await
        .expect("add falsy node");
    }
    db.add_node_raw(serde_json::json!({"id": "absent", "type": "Entity", "name": "absent"}))
        .await
        .expect("add node without source_user");
    // A truthy label must be preserved, not clobbered.
    db.add_node_raw(serde_json::json!({
        "id": "already-tagged",
        "type": "Entity",
        "name": "already-tagged",
        "source_user": "carol@example.com",
    }))
    .await
    .expect("add pre-tagged node");

    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> =
        vec![("alice@example.com".to_string(), Arc::new(db))];
    let html = render_multi_user(&pairs).await.expect("render");

    let nodes_json = extract_var(&html, "nodes");
    let nodes = nodes_json.as_array().expect("nodes is an array");
    let by_id = |id: &str| {
        nodes
            .iter()
            .find(|node| node.get("id").and_then(|v| v.as_str()) == Some(id))
            .unwrap_or_else(|| panic!("node {id} present"))
            .get("source_user")
            .cloned()
    };

    for id in [
        "empty-string",
        "zero",
        "zero-float",
        "false",
        "null",
        "empty-array",
        "empty-object",
        "absent",
    ] {
        assert_eq!(
            by_id(id),
            Some(serde_json::json!("alice@example.com")),
            "falsy/absent source_user on `{id}` must be replaced by the owning label"
        );
    }
    assert_eq!(
        by_id("already-tagged"),
        Some(serde_json::json!("carol@example.com")),
        "a truthy source_user must survive untouched"
    );

    // The colour map is keyed off the emitted `source_user`, so the owning label
    // must have earned a palette entry.
    assert!(
        html.contains("alice@example.com"),
        "owning label missing from the rendered payload"
    );
}

#[tokio::test]
async fn dedupe_edges_by_source_target_relation() {
    // Both pairs declare the same edge (a -[knows]-> b). Assert the rendered
    // output contains exactly one link entry (mirror Python `cognee_network_visualization.py:221-225`).
    let alice = MockGraphDB::new();
    alice
        .add_node_raw(serde_json::json!({"id": "a", "type": "Entity"}))
        .await
        .expect("add a alice");
    alice
        .add_node_raw(serde_json::json!({"id": "b", "type": "Entity"}))
        .await
        .expect("add b alice");
    alice
        .add_edge("a", "b", "knows", None)
        .await
        .expect("alice edge");

    let bob = MockGraphDB::new();
    bob.add_node_raw(serde_json::json!({"id": "a", "type": "Entity"}))
        .await
        .expect("add a bob");
    bob.add_node_raw(serde_json::json!({"id": "b", "type": "Entity"}))
        .await
        .expect("add b bob");
    bob.add_edge("a", "b", "knows", None)
        .await
        .expect("bob edge");

    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> = vec![
        ("alice@example.com".to_string(), Arc::new(alice)),
        ("bob@example.com".to_string(), Arc::new(bob)),
    ];

    let html = render_multi_user(&pairs).await.expect("render");

    let arr_json = extract_var(&html, "links");
    let arr = arr_json.as_array().expect("links is an array");

    assert_eq!(arr.len(), 1, "expected 1 deduplicated edge, got {arr:#?}");
    let only = &arr[0];
    assert_eq!(only.get("source").and_then(|v| v.as_str()), Some("a"));
    assert_eq!(only.get("target").and_then(|v| v.as_str()), Some("b"));
    assert_eq!(only.get("relation").and_then(|v| v.as_str()), Some("knows"));
}
