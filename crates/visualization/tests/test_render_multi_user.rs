//! Tests for `render_multi_user` — the multi-user aggregation entry point used
//! by the visualize HTTP router's `POST /multi` endpoint.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use cognee_graph::GraphDBTrait;
use cognee_graph::mock::MockGraphDB;

use cognee_visualization::render_multi_user;

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
        ("user-alice".to_string(), Arc::new(alice)),
        ("user-bob".to_string(), Arc::new(bob)),
    ];

    let html = render_multi_user(&pairs).await.expect("render");

    // The d3 template inlines node JSON via `__NODES_DATA__`. Six nodes
    // means each user_id appears at least three times.
    let alice_count = html.matches("user-alice").count();
    let bob_count = html.matches("user-bob").count();
    assert!(
        alice_count >= 3,
        "expected user-alice to appear at least 3 times, found {alice_count}"
    );
    assert!(
        bob_count >= 3,
        "expected user-bob to appear at least 3 times, found {bob_count}"
    );

    // The user_id attribute is added to every node — assert the literal
    // attribute key is present in the embedded JSON.
    assert!(
        html.contains("\"user_id\""),
        "rendered HTML should carry the user_id attribute"
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
async fn tags_each_node_with_owning_user_id() {
    let alice = MockGraphDB::new();
    let mut node_data: HashMap<Cow<'static, str>, serde_json::Value> = HashMap::new();
    node_data.insert("id".into(), serde_json::Value::String("n1".into()));
    node_data.insert("type".into(), serde_json::Value::String("Entity".into()));
    alice
        .add_node_raw(serde_json::json!({"id": "n1", "type": "Entity"}))
        .await
        .expect("add");

    let pairs: Vec<(String, Arc<dyn GraphDBTrait>)> =
        vec![("alice-id".to_string(), Arc::new(alice))];

    let html = render_multi_user(&pairs).await.expect("render");
    // The user_id attribute should be alice-id for the single node.
    assert!(html.contains("alice-id"), "node missing user_id tag");
}
