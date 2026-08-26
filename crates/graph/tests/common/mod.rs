#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Shared GraphDBTrait contract tests.
//!
//! Each function exercises one aspect of the [`GraphDBTrait`] and can be called
//! with *any* backend (Ladybug, PostgreSQL, Mock, …). Backend-specific
//! integration tests construct their adapter and call these helpers.

use cognee_graph::{EdgeData, GraphDBTrait, GraphDBTraitExt, NodeTruthState};
use serde::Serialize;
use serde_json::json;
use std::borrow::Cow;
use std::collections::HashMap;

/// Simple test node.
#[derive(Debug, Clone, Serialize)]
struct TestNode {
    id: String,
    name: String,
    #[serde(rename = "type")]
    node_type: String,
    value: i32,
}

impl TestNode {
    fn new(id: &str, name: &str, node_type: &str, value: i32) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            node_type: node_type.to_string(),
            value,
        }
    }
}

// -- initialisation ----------------------------------------------------------

pub async fn test_initialize_is_empty(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();
    db.initialize().await.unwrap();
    assert!(db.is_empty().await.unwrap());
}

// -- node CRUD ---------------------------------------------------------------

pub async fn test_add_and_get_node(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let node = TestNode::new("n1", "Alice", "Person", 42);
    db.add_node(&node).await.unwrap();

    let fetched = db.get_node("n1").await.unwrap().expect("node should exist");
    assert_eq!(fetched.get("id").unwrap().as_str().unwrap(), "n1");
    assert_eq!(fetched.get("name").unwrap().as_str().unwrap(), "Alice");
    assert_eq!(fetched.get("value").unwrap().as_i64().unwrap(), 42);
}

pub async fn test_add_nodes_batch(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let nodes: Vec<TestNode> = (0..10)
        .map(|i| TestNode::new(&format!("b{i}"), &format!("Node{i}"), "Item", i))
        .collect();
    let refs: Vec<&TestNode> = nodes.iter().collect();
    db.add_nodes(&refs).await.unwrap();

    for i in 0..10 {
        assert!(
            db.has_node(&format!("b{i}")).await.unwrap(),
            "node b{i} should exist"
        );
    }
}

pub async fn test_has_node(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let node = TestNode::new("exists", "E", "T", 1);
    db.add_node(&node).await.unwrap();

    assert!(db.has_node("exists").await.unwrap());
    assert!(!db.has_node("ghost").await.unwrap());
}

pub async fn test_get_nodes_batch(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let n1 = TestNode::new("m1", "A", "T", 1);
    let n2 = TestNode::new("m2", "B", "T", 2);
    let n3 = TestNode::new("m3", "C", "T", 3);
    db.add_nodes(&[&n1, &n2, &n3]).await.unwrap();

    let fetched = db.get_nodes(&["m1".into(), "m3".into()]).await.unwrap();
    assert_eq!(fetched.len(), 2);

    let ids: Vec<&str> = fetched
        .iter()
        .filter_map(|n| n.get("id")?.as_str())
        .collect();
    assert!(ids.contains(&"m1"));
    assert!(ids.contains(&"m3"));
}

pub async fn test_delete_node(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let node = TestNode::new("del1", "D", "T", 0);
    db.add_node(&node).await.unwrap();
    assert!(db.has_node("del1").await.unwrap());

    db.delete_node("del1").await.unwrap();
    assert!(!db.has_node("del1").await.unwrap());
}

pub async fn test_delete_nodes_batch(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let n1 = TestNode::new("d1", "A", "T", 0);
    let n2 = TestNode::new("d2", "B", "T", 0);
    let n3 = TestNode::new("d3", "C", "T", 0);
    db.add_nodes(&[&n1, &n2, &n3]).await.unwrap();

    db.delete_nodes(&["d1".into(), "d3".into()]).await.unwrap();
    assert!(!db.has_node("d1").await.unwrap());
    assert!(db.has_node("d2").await.unwrap());
    assert!(!db.has_node("d3").await.unwrap());
}

pub async fn test_node_upsert_same_id(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let original = TestNode::new("u1", "Original", "Person", 10);
    let replacement = TestNode::new("u1", "Updated", "Person", 20);

    db.add_node(&original).await.unwrap();
    db.add_node(&replacement).await.unwrap();

    let fetched = db.get_node("u1").await.unwrap().expect("node should exist");
    assert_eq!(fetched.get("name").unwrap().as_str().unwrap(), "Updated");
    assert_eq!(fetched.get("value").unwrap().as_i64().unwrap(), 20);

    let metrics = db.get_graph_metrics(false).await.unwrap();
    assert_eq!(metrics.get("node_count").unwrap().as_i64().unwrap(), 1);
}

// -- edge CRUD ---------------------------------------------------------------

pub async fn test_add_and_has_edge(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ea", "A", "T", 0);
    let b = TestNode::new("eb", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();

    db.add_edge("ea", "eb", "knows", None).await.unwrap();
    assert!(db.has_edge("ea", "eb", "knows").await.unwrap());
    assert!(!db.has_edge("eb", "ea", "knows").await.unwrap());
    assert!(!db.has_edge("ea", "eb", "other").await.unwrap());
}

pub async fn test_add_edges_batch(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ba", "A", "T", 0);
    let b = TestNode::new("bb", "B", "T", 0);
    let c = TestNode::new("bc", "C", "T", 0);
    db.add_nodes(&[&a, &b, &c]).await.unwrap();

    let edges: Vec<EdgeData> = vec![
        ("ba".into(), "bb".into(), "r1".into(), HashMap::new()),
        ("bb".into(), "bc".into(), "r2".into(), HashMap::new()),
    ];
    db.add_edges(&edges).await.unwrap();

    assert!(db.has_edge("ba", "bb", "r1").await.unwrap());
    assert!(db.has_edge("bb", "bc", "r2").await.unwrap());
}

pub async fn test_has_edges(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ha", "A", "T", 0);
    let b = TestNode::new("hb", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();
    db.add_edge("ha", "hb", "rel", None).await.unwrap();

    let candidates: Vec<EdgeData> = vec![
        ("ha".into(), "hb".into(), "rel".into(), HashMap::new()),
        ("ha".into(), "hb".into(), "nope".into(), HashMap::new()),
    ];
    let found = db.has_edges(&candidates).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].2, "rel");
}

/// Equivalence + property-preservation check for the batched `has_edges`.
///
/// Asserts the set-based `has_edges` returns exactly the same subset as calling
/// `has_edge` per candidate (the old per-edge behaviour) on a mixed present/absent
/// batch, and that each returned edge keeps the *input* candidate's properties
/// (not the differing properties stored in the DB) — proving the result is mapped
/// back onto the input rather than reconstructed from the query.
pub async fn test_has_edges_batch_equivalence(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ea", "A", "T", 0);
    let b = TestNode::new("eb", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();

    // Store edges with specific properties in the DB.
    let mut stored_rel = HashMap::new();
    stored_rel.insert(Cow::Borrowed("weight"), json!(99));
    db.add_edge("ea", "eb", "rel", Some(stored_rel))
        .await
        .unwrap();

    let mut stored_rel2 = HashMap::new();
    stored_rel2.insert(Cow::Borrowed("since"), json!(1999));
    db.add_edge("eb", "ea", "rel2", Some(stored_rel2))
        .await
        .unwrap();

    // Candidates: two present (with *different* props than what's stored) and one absent.
    let mut cand_a_props = HashMap::new();
    cand_a_props.insert(Cow::Borrowed("weight"), json!(5));
    let mut cand_c_props = HashMap::new();
    cand_c_props.insert(Cow::Borrowed("since"), json!(2020));

    let candidates: Vec<EdgeData> = vec![
        ("ea".into(), "eb".into(), "rel".into(), cand_a_props.clone()), // present
        ("ea".into(), "eb".into(), "nope".into(), HashMap::new()),      // absent
        (
            "eb".into(),
            "ea".into(),
            "rel2".into(),
            cand_c_props.clone(),
        ), // present
    ];

    let found = db.has_edges(&candidates).await.unwrap();

    // Reference: the old per-edge behaviour.
    let mut expected = Vec::new();
    for c in &candidates {
        if db.has_edge(&c.0, &c.1, &c.2).await.unwrap() {
            expected.push((c.0.clone(), c.1.clone(), c.2.clone()));
        }
    }
    let found_keys: Vec<(String, String, String)> = found
        .iter()
        .map(|e| (e.0.clone(), e.1.clone(), e.2.clone()))
        .collect();
    assert_eq!(
        found_keys, expected,
        "batched has_edges must match per-edge has_edge results"
    );
    assert_eq!(found.len(), 2);

    // Properties must be the *input* candidate's, not the DB-stored ones.
    let a_found = found
        .iter()
        .find(|e| e.2 == "rel")
        .expect("present edge 'rel' should be returned");
    assert_eq!(a_found.3.get("weight").unwrap().as_i64().unwrap(), 5);

    let c_found = found
        .iter()
        .find(|e| e.2 == "rel2")
        .expect("present edge 'rel2' should be returned");
    assert_eq!(c_found.3.get("since").unwrap().as_i64().unwrap(), 2020);
}

pub async fn test_get_edges(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ge_a", "A", "T", 0);
    let b = TestNode::new("ge_b", "B", "T", 0);
    let c = TestNode::new("ge_c", "C", "T", 0);
    db.add_nodes(&[&a, &b, &c]).await.unwrap();

    db.add_edge("ge_a", "ge_b", "out", None).await.unwrap();
    db.add_edge("ge_c", "ge_a", "in", None).await.unwrap();

    let edges = db.get_edges("ge_a").await.unwrap();
    // Should see both the outgoing and incoming edge.
    assert_eq!(edges.len(), 2);
}

pub async fn test_edge_upsert_same_key(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("uea", "A", "T", 0);
    let b = TestNode::new("ueb", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();

    let mut original_props = HashMap::new();
    original_props.insert(Cow::Borrowed("since"), json!(2020));

    let mut replacement_props = HashMap::new();
    replacement_props.insert(Cow::Borrowed("since"), json!(2024));
    replacement_props.insert(Cow::Borrowed("strength"), json!("high"));

    db.add_edge("uea", "ueb", "knows", Some(original_props))
        .await
        .unwrap();
    db.add_edge("uea", "ueb", "knows", Some(replacement_props))
        .await
        .unwrap();

    let edges = db.get_edges("uea").await.unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].2, "knows");
    assert_eq!(edges[0].3.get("since").unwrap().as_i64().unwrap(), 2024);
    assert_eq!(
        edges[0].3.get("strength").unwrap().as_str().unwrap(),
        "high"
    );
}

// -- graph queries -----------------------------------------------------------

pub async fn test_get_neighbors(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("na", "A", "T", 0);
    let b = TestNode::new("nb", "B", "T", 0);
    let c = TestNode::new("nc", "C", "T", 0);
    db.add_nodes(&[&a, &b, &c]).await.unwrap();

    db.add_edge("na", "nb", "r", None).await.unwrap();
    db.add_edge("nc", "na", "r", None).await.unwrap();

    let neighbors = db.get_neighbors("na").await.unwrap();
    let ids: Vec<&str> = neighbors
        .iter()
        .filter_map(|n| n.get("id")?.as_str())
        .collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"nb"));
    assert!(ids.contains(&"nc"));
}

pub async fn test_get_connections(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ca", "A", "T", 0);
    let b = TestNode::new("cb", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();
    db.add_edge("ca", "cb", "linked", None).await.unwrap();

    let conns = db.get_connections("ca").await.unwrap();
    assert_eq!(conns.len(), 1);

    let (src, edge, tgt) = &conns[0];
    assert!(edge.get("relationship_name").is_some());

    // The queried node should appear in the connection.
    let src_id = src.get("id").unwrap().as_str().unwrap();
    let tgt_id = tgt.get("id").unwrap().as_str().unwrap();
    assert!(src_id == "ca" || tgt_id == "ca");
}

pub async fn test_get_graph_data(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ga", "A", "T", 0);
    let b = TestNode::new("gb", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();
    db.add_edge("ga", "gb", "r", None).await.unwrap();

    let (nodes, edges) = db.get_graph_data().await.unwrap();
    assert_eq!(nodes.len(), 2);
    assert_eq!(edges.len(), 1);
}

/// `get_graph_data()` must hand back the `DataPoint`'s own epoch-ms
/// `created_at` / `updated_at`.
///
/// Python keeps both in the serialized `properties` payload (its `core_keys` /
/// popped-key set is only `{id, name, type}`) and merges that payload back on
/// read, which is what makes `preprocessor.py`'s `t_created` — and with it the
/// Memory tab's run timeline and its summary/chunk ordering — work on real data.
/// Rust used to strip them at the write boundary, so `t_created` could only ever
/// appear in hand-written fixtures and every timeline was synthetic.
///
/// This runs against a real backend on purpose: `MockGraphDB` echoes whatever
/// JSON it was handed, so it cannot observe the core-field/properties split that
/// dropped the value.
pub async fn test_get_graph_data_surfaces_created_at(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    // `cognee_models::DataPoint` serializes both fields as epoch milliseconds.
    const CREATED_AT: i64 = 1_768_164_683_000;
    const UPDATED_AT: i64 = 1_768_164_684_500;

    db.add_node_raw(json!({
        "id": "ts1",
        "name": "Timestamped",
        "type": "DocumentChunk",
        "created_at": CREATED_AT,
        "updated_at": UPDATED_AT,
        "chunk_index": 0,
    }))
    .await
    .unwrap();

    let (nodes, _edges) = db.get_graph_data().await.unwrap();
    let (_, props) = nodes
        .iter()
        .find(|(id, _)| id == "ts1")
        .expect("ts1 returned by get_graph_data");

    assert_eq!(
        props.get("created_at").and_then(serde_json::Value::as_i64),
        Some(CREATED_AT),
        "get_graph_data must surface the epoch-ms created_at; got {:?}",
        props.get("created_at")
    );
    assert_eq!(
        props.get("updated_at").and_then(serde_json::Value::as_i64),
        Some(UPDATED_AT),
        "get_graph_data must surface the epoch-ms updated_at; got {:?}",
        props.get("updated_at")
    );
    // The ordinary property must be unaffected.
    assert_eq!(
        props.get("chunk_index").and_then(serde_json::Value::as_i64),
        Some(0)
    );

    // A node written without timestamps must not have any fabricated: the
    // preprocessor keys "does this node have a creation time?" off their absence.
    db.add_node_raw(json!({"id": "ts2", "name": "Legacy", "type": "DocumentChunk"}))
        .await
        .unwrap();
    let (nodes, _edges) = db.get_graph_data().await.unwrap();
    let (_, legacy) = nodes
        .iter()
        .find(|(id, _)| id == "ts2")
        .expect("ts2 returned by get_graph_data");
    assert!(
        legacy.get("created_at").is_none(),
        "created_at must not be invented for a node that carried none: {:?}",
        legacy.get("created_at")
    );
}

pub async fn test_get_graph_metrics(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ma", "A", "T", 0);
    let b = TestNode::new("mb", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();
    db.add_edge("ma", "mb", "r", None).await.unwrap();

    let metrics = db.get_graph_metrics(false).await.unwrap();
    assert_eq!(metrics.get("node_count").unwrap().as_i64().unwrap(), 2);
    assert_eq!(metrics.get("edge_count").unwrap().as_i64().unwrap(), 1);
}

pub async fn test_get_filtered_graph_data(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("fa", "A", "Person", 0);
    let b = TestNode::new("fb", "B", "Person", 0);
    let c = TestNode::new("fc", "C", "Animal", 0);
    db.add_nodes(&[&a, &b, &c]).await.unwrap();
    db.add_edge("fa", "fb", "r", None).await.unwrap();
    db.add_edge("fa", "fc", "r", None).await.unwrap();

    let mut filters = HashMap::new();
    filters.insert(Cow::Borrowed("type"), vec![json!("Person")]);

    let (nodes, edges) = db.get_filtered_graph_data(&filters).await.unwrap();
    assert_eq!(nodes.len(), 2, "only Person nodes should be returned");
    // Only edges between filtered nodes (fa->fb) should appear.
    assert_eq!(edges.len(), 1);
}

pub async fn test_get_nodeset_subgraph_or(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("sa", "Alice", "Person", 0);
    let b = TestNode::new("sb", "Bob", "Person", 0);
    let c = TestNode::new("sc", "Carol", "Person", 0);
    let d = TestNode::new("sd", "Dave", "Org", 0);
    db.add_nodes(&[&a, &b, &c, &d]).await.unwrap();
    db.add_edge("sa", "sc", "knows", None).await.unwrap();
    db.add_edge("sb", "sd", "works_at", None).await.unwrap();

    let (nodes, _edges) = db
        .get_nodeset_subgraph("Person", &["Alice".into(), "Bob".into()], "OR")
        .await
        .unwrap();

    let ids: Vec<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();
    // Primary (sa, sb) + OR-neighbors (sc, sd).
    assert!(ids.contains(&"sa"));
    assert!(ids.contains(&"sb"));
    assert!(ids.contains(&"sc"));
    assert!(ids.contains(&"sd"));
}

pub async fn test_get_nodeset_subgraph_and(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("aa", "Alice", "Person", 0);
    let b = TestNode::new("ab", "Bob", "Person", 0);
    let shared = TestNode::new("as", "Shared", "Org", 0);
    let only_a = TestNode::new("ao", "OnlyA", "Org", 0);
    db.add_nodes(&[&a, &b, &shared, &only_a]).await.unwrap();
    db.add_edge("aa", "as", "member", None).await.unwrap();
    db.add_edge("ab", "as", "member", None).await.unwrap();
    db.add_edge("aa", "ao", "member", None).await.unwrap();

    let (nodes, _edges) = db
        .get_nodeset_subgraph("Person", &["Alice".into(), "Bob".into()], "AND")
        .await
        .unwrap();

    let ids: Vec<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();
    // Primary (aa, ab) + AND-neighbor (as only, since ao is connected only to aa).
    assert!(ids.contains(&"aa"));
    assert!(ids.contains(&"ab"));
    assert!(ids.contains(&"as"));
    assert!(
        !ids.contains(&"ao"),
        "OnlyA should be excluded — not connected to all primary nodes"
    );
}

pub async fn test_get_id_filtered_graph_data(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ia", "A", "T", 0);
    let b = TestNode::new("ib", "B", "T", 0);
    let c = TestNode::new("ic", "C", "T", 0);
    db.add_nodes(&[&a, &b, &c]).await.unwrap();
    db.add_edge("ia", "ib", "r", None).await.unwrap();
    db.add_edge("ia", "ic", "r", None).await.unwrap();

    let (nodes, edges) = db
        .get_id_filtered_graph_data(&["ia".into(), "ib".into()])
        .await
        .unwrap();
    assert_eq!(nodes.len(), 2);
    // Only edges between the selected set (ia->ib).
    assert_eq!(edges.len(), 1);
}

pub async fn test_delete_graph(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("dg1", "A", "T", 0);
    db.add_node(&a).await.unwrap();
    assert!(!db.is_empty().await.unwrap());

    db.delete_graph().await.unwrap();
    assert!(db.is_empty().await.unwrap());
}

pub async fn test_node_delete_cascades_edges(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("cas_a", "A", "T", 0);
    let b = TestNode::new("cas_b", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();
    db.add_edge("cas_a", "cas_b", "r", None).await.unwrap();

    db.delete_node("cas_a").await.unwrap();
    assert!(!db.has_edge("cas_a", "cas_b", "r").await.unwrap());
}

pub async fn test_properties_json_round_trip(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let node = json!({
        "id": "jp1",
        "name": "JsonNode",
        "type": "Test",
        "score": 1.234,
        "tags": ["a", "b"],
        "nested": { "x": 1 }
    });
    db.add_node_raw(node).await.unwrap();

    let fetched = db.get_node("jp1").await.unwrap().unwrap();
    assert_eq!(fetched.get("score").unwrap().as_f64().unwrap(), 1.234);
    assert_eq!(fetched.get("tags").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(
        fetched
            .get("nested")
            .unwrap()
            .get("x")
            .unwrap()
            .as_i64()
            .unwrap(),
        1
    );
}

// -- node truth state --------------------------------------------------------

pub async fn test_node_truth_state_round_trip(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let a = TestNode::new("ts_a", "A", "T", 0);
    let b = TestNode::new("ts_b", "B", "T", 0);
    db.add_nodes(&[&a, &b]).await.unwrap();

    let mut updates = HashMap::new();
    updates.insert(
        "ts_a".to_string(),
        NodeTruthState {
            truth_alignment: vec![0.1, 0.2, 0.3],
            truth_epoch: 5,
        },
    );
    // Epoch 0 is a legitimate real epoch and must survive as 0, not -1.
    updates.insert(
        "ts_b".to_string(),
        NodeTruthState {
            truth_alignment: vec![],
            truth_epoch: 0,
        },
    );

    let set_result = db.set_node_truth_state(&updates).await.unwrap();
    assert_eq!(set_result.get("ts_a"), Some(&true));
    assert_eq!(set_result.get("ts_b"), Some(&true));

    let got = db
        .get_node_truth_state(&["ts_a".to_string(), "ts_b".to_string()])
        .await
        .unwrap();

    let a_state = got.get("ts_a").expect("ts_a present");
    assert_eq!(a_state.truth_alignment, vec![0.1, 0.2, 0.3]);
    assert_eq!(a_state.truth_epoch, 5);

    let b_state = got.get("ts_b").expect("ts_b present");
    assert_eq!(b_state.truth_alignment, Vec::<f64>::new());
    assert_eq!(b_state.truth_epoch, 0);
}

pub async fn test_node_truth_state_missing_and_invalid(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    // (a) never-added id -> absent from the result map.
    let ghost = db
        .get_node_truth_state(&["ghost".to_string()])
        .await
        .unwrap();
    assert!(!ghost.contains_key("ghost"));

    // (b) plain node with no truth props -> present with defaults ([], -1).
    let plain = TestNode::new("ts_plain", "P", "T", 0);
    db.add_node(&plain).await.unwrap();

    // (c) numeric-string epoch "3" parses to 3; non-array alignment -> [].
    db.add_node_raw(json!({
        "id": "ts_str",
        "name": "Str",
        "type": "T",
        "truth_epoch": "3",
        "truth_alignment": "not-an-array"
    }))
    .await
    .unwrap();

    // non-numeric epoch "bad" -> -1 sentinel.
    db.add_node_raw(json!({
        "id": "ts_bad",
        "name": "Bad",
        "type": "T",
        "truth_epoch": "bad"
    }))
    .await
    .unwrap();

    let got = db
        .get_node_truth_state(&[
            "ts_plain".to_string(),
            "ts_str".to_string(),
            "ts_bad".to_string(),
        ])
        .await
        .unwrap();

    let plain_state = got
        .get("ts_plain")
        .expect("ts_plain present with defaults, not omitted");
    assert_eq!(plain_state.truth_alignment, Vec::<f64>::new());
    assert_eq!(plain_state.truth_epoch, -1);

    let str_state = got.get("ts_str").expect("ts_str present");
    assert_eq!(str_state.truth_epoch, 3);
    assert_eq!(str_state.truth_alignment, Vec::<f64>::new());

    let bad_state = got.get("ts_bad").expect("ts_bad present");
    assert_eq!(bad_state.truth_epoch, -1);
}

/// `set_node_truth_state` must read-merge-write: writing `truth_alignment` /
/// `truth_epoch` onto a node must NOT clobber the node's other, pre-existing
/// properties. Seeds a node carrying a *custom* sibling property
/// (`feedback_weight`) that the fixed-schema `TestNode` doesn't have, so a
/// clobbering (delete-then-re-add-only-truth-fields) impl would be detected.
pub async fn test_node_truth_state_preserves_other_properties(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    // Seed via add_node_raw so we can attach an arbitrary sibling property that
    // is not part of the truth-state payload.
    db.add_node_raw(json!({
        "id": "ts_pres",
        "name": "Preserved",
        "type": "Person",
        "value": 77,
        "feedback_weight": 0.42
    }))
    .await
    .unwrap();

    let mut updates = HashMap::new();
    updates.insert(
        "ts_pres".to_string(),
        NodeTruthState {
            truth_alignment: vec![0.7, 0.8],
            truth_epoch: 9,
        },
    );
    let set_result = db.set_node_truth_state(&updates).await.unwrap();
    assert_eq!(set_result.get("ts_pres"), Some(&true));

    // Fetch the *whole* node and assert the sibling props AND the freshly
    // written truth-state fields all coexist.
    let node = db
        .get_node("ts_pres")
        .await
        .unwrap()
        .expect("node should still exist");

    // Pre-existing siblings survived the truth-state write.
    assert_eq!(
        node.get("name").unwrap().as_str().unwrap(),
        "Preserved",
        "name must be preserved after set_node_truth_state"
    );
    assert_eq!(
        node.get("type").unwrap().as_str().unwrap(),
        "Person",
        "type must be preserved after set_node_truth_state"
    );
    assert_eq!(
        node.get("value").unwrap().as_i64().unwrap(),
        77,
        "value must be preserved after set_node_truth_state"
    );
    assert_eq!(
        node.get("feedback_weight").unwrap().as_f64().unwrap(),
        0.42,
        "custom sibling property feedback_weight must not be clobbered"
    );

    // The truth-state fields were actually written.
    let alignment: Vec<f64> = node
        .get("truth_alignment")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(alignment, vec![0.7, 0.8]);
    assert_eq!(node.get("truth_epoch").unwrap().as_i64().unwrap(), 9);

    // Cross-check the canonical reader agrees.
    let got = db
        .get_node_truth_state(&["ts_pres".to_string()])
        .await
        .unwrap();
    let state = got.get("ts_pres").expect("ts_pres present");
    assert_eq!(state.truth_alignment, vec![0.7, 0.8]);
    assert_eq!(state.truth_epoch, 9);
}

// -- get_neighborhood --------------------------------------------------------

pub async fn test_get_neighborhood_depth1(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    // Fixture: `seed` is the stored TARGET of `source -> seed` (rel "in") and
    // the stored SOURCE of `seed -> target` (rel "out"). `far` is two hops away
    // via `target -> far` and must NOT appear at depth 1.
    let seed = TestNode::new("seed", "Seed", "T", 0);
    let target = TestNode::new("target", "Target", "T", 0);
    let source = TestNode::new("source", "Source", "T", 0);
    let far = TestNode::new("far", "Far", "T", 0);
    db.add_nodes(&[&seed, &target, &source, &far])
        .await
        .unwrap();

    db.add_edge("seed", "target", "out", None).await.unwrap();
    db.add_edge("source", "seed", "in", None).await.unwrap();
    db.add_edge("target", "far", "out", None).await.unwrap();

    let (nodes, edges) = db.get_neighborhood(&["seed".to_string()], 1).await.unwrap();

    // Node set is exactly {seed, target, source} — not `far`.
    let node_ids: std::collections::HashSet<&str> =
        nodes.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(node_ids.len(), 3, "expected exactly seed, target, source");
    assert!(node_ids.contains("seed"));
    assert!(node_ids.contains("target"));
    assert!(node_ids.contains("source"));
    assert!(!node_ids.contains("far"), "far is two hops away");

    // Exact-direction (anti-flip) assertions — the regression guard for
    // locked decision 6.
    let edge_set: std::collections::HashSet<(&str, &str, &str)> = edges
        .iter()
        .map(|(s, t, r, _)| (s.as_str(), t.as_str(), r.as_str()))
        .collect();
    assert!(
        edge_set.contains(&("source", "seed", "in")),
        "stored direction source->seed must be preserved, got {edge_set:?}"
    );
    assert!(
        edge_set.contains(&("seed", "target", "out")),
        "stored direction seed->target must be preserved, got {edge_set:?}"
    );
    // The flipped forms must be ABSENT.
    assert!(
        !edge_set.contains(&("seed", "source", "in")),
        "flipped edge seed->source must NOT appear"
    );
    assert!(
        !edge_set.contains(&("target", "seed", "out")),
        "flipped edge target->seed must NOT appear"
    );
    // The two-hop edge must not appear (far not in the resolved set).
    assert!(!edge_set.contains(&("target", "far", "out")));
    assert_eq!(edges.len(), 2, "exactly the two seed-incident edges");
}

pub async fn test_get_neighborhood_multiple_seeds(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    // Two seeds (s1, s2) share one common neighbor (`shared`) and have one edge
    // directly between them.
    let s1 = TestNode::new("s1", "S1", "T", 0);
    let s2 = TestNode::new("s2", "S2", "T", 0);
    let shared = TestNode::new("shared", "Shared", "T", 0);
    db.add_nodes(&[&s1, &s2, &shared]).await.unwrap();

    db.add_edge("s1", "shared", "r", None).await.unwrap();
    db.add_edge("s2", "shared", "r", None).await.unwrap();
    db.add_edge("s1", "s2", "between", None).await.unwrap();

    let (nodes, edges) = db
        .get_neighborhood(&["s1".to_string(), "s2".to_string()], 1)
        .await
        .unwrap();

    // `shared` must appear exactly once (dedup across the two seeds).
    let shared_count = nodes.iter().filter(|(id, _)| id == "shared").count();
    assert_eq!(shared_count, 1, "shared neighbor must be deduped");
    assert_eq!(nodes.len(), 3, "expected exactly s1, s2, shared");

    // The seed-seed edge must appear exactly once.
    let between_count = edges
        .iter()
        .filter(|(s, t, r, _)| s == "s1" && t == "s2" && r == "between")
        .count();
    assert_eq!(between_count, 1, "seed-seed edge must appear exactly once");
    assert_eq!(edges.len(), 3, "all three edges, no duplicates");
}

pub async fn test_get_neighborhood_empty_seeds(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    // Empty seeds must short-circuit to an empty result without issuing a query
    // that would error on an empty `IN (...)` / `unnest`.
    let (nodes, edges) = db.get_neighborhood(&[], 1).await.unwrap();
    assert!(nodes.is_empty());
    assert!(edges.is_empty());
}

// -- NUL bytes ---------------------------------------------------------------

/// A NUL byte in node or edge text must never break persistence.
///
/// PDF-extracted chunk text routinely contains a literal `0x00`. Postgres
/// cannot store it in `varchar` and its `jsonb` parser rejects the escape
/// `serde_json` emits for it, so before `cognee_utils::sanitize` the very first
/// `add_nodes` of a cognify run died — after both LLM stages had been paid for.
///
/// Backends differ in what they *store*: the Postgres adapters strip the NUL,
/// Ladybug keeps it. Both are acceptable, so this asserts the contract they do
/// share — the write succeeds, the row is retrievable, and every non-NUL
/// character survives in order.
pub async fn test_nul_bytes_in_text_are_persistable(db: &dyn GraphDBTrait) {
    db.delete_graph().await.unwrap();

    let dirty_name = "Nikola\0 Tesla";
    let dirty_body = "page 1\0page 2";
    let node = json!({
        "id": "nul1",
        "name": dirty_name,
        "type": "Person",
        "text": dirty_body,
        "nested": {"inner": ["a\0b"]},
    });
    db.add_node_raw(node).await.unwrap();

    let other = json!({"id": "nul2", "name": "Clean", "type": "Person"});
    db.add_node_raw(other).await.unwrap();

    let edge: EdgeData = (
        "nul1".to_string(),
        "nul2".to_string(),
        "knows".to_string(),
        HashMap::from([(Cow::Borrowed("edge_text"), json!("described\0relationship"))]),
    );
    db.add_edges(&[edge]).await.unwrap();

    let fetched = db
        .get_node("nul1")
        .await
        .unwrap()
        .expect("the node must be retrievable after a write carrying NUL bytes");

    let strip = |s: &str| s.replace('\0', "");
    assert_eq!(
        strip(fetched.get("name").unwrap().as_str().unwrap()),
        strip(dirty_name),
        "every non-NUL character of `name` must survive"
    );
    assert_eq!(
        strip(fetched.get("text").unwrap().as_str().unwrap()),
        strip(dirty_body),
        "every non-NUL character of a JSON property must survive"
    );

    assert!(
        db.has_edge("nul1", "nul2", "knows").await.unwrap(),
        "an edge whose properties carry NUL bytes must still be written"
    );

    // Two edges whose relationship names differ *only* by an embedded NUL
    // collapse to one row once sanitized. They must be deduplicated before the
    // insert is built: on Postgres both would otherwise reach a single
    // `INSERT ... ON CONFLICT DO UPDATE`, which fails with `21000: ON CONFLICT
    // DO UPDATE command cannot affect row a second time` and aborts the batch.
    let colliding: Vec<EdgeData> = vec![
        (
            "nul1".to_string(),
            "nul2".to_string(),
            "cite\0s".to_string(),
            HashMap::from([(Cow::Borrowed("edge_text"), json!("first"))]),
        ),
        (
            "nul1".to_string(),
            "nul2".to_string(),
            "cites".to_string(),
            HashMap::from([(Cow::Borrowed("edge_text"), json!("second"))]),
        ),
    ];
    db.add_edges(&colliding)
        .await
        .expect("edges colliding only after NUL removal must not abort the batch");
}
