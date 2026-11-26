//! The CODE operations against what Python cognee's own retriever returned.
//!
//! `fixtures/code_search/graph.json` is the raw graph an adapter returns
//! (Python's navigation-test shop repository plus the adapter quirks: JSON
//! text properties, a Ladybug-style nested `properties` column, a null and a
//! blank doc, duplicate edges, non-code nodes) and `goldens.json` what
//! Python's `CodeRetriever` at the parity pin returned for each case —
//! result and context text, or error message, code and param. Both are
//! written by `scripts/generate_code_search_goldens.py`.
//!
//! The graph goes through a mock adapter and the real snapshot loader, so
//! the type filter and the normalization are under test too.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::borrow::Cow;
use std::collections::HashMap;

use cognee_code_graph::query::{
    CodeGraphSnapshot, CodeOperation, CodeRequest, json_key, load_code_graph_snapshot,
    run_code_operation,
};
use cognee_graph::{EdgeData, GraphDBTrait, MockGraphDB};
use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/code_search");

fn read_json(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{FIXTURES}/{name}"))
        .unwrap_or_else(|error| panic!("read {name}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("parse {name}: {error}"))
}

async fn fixture_snapshot() -> CodeGraphSnapshot {
    let graph = read_json("graph.json");
    let mock = MockGraphDB::new();
    let mut nodes = Vec::new();
    for node in graph["nodes"].as_array().expect("nodes") {
        let id = node[0].as_str().expect("node id");
        let mut properties = node[1].as_object().expect("node properties").clone();
        properties.insert("id".into(), json!(id));
        nodes.push(Value::Object(properties));
    }
    mock.add_nodes_raw(nodes).await.expect("add nodes");
    let edges: Vec<EdgeData> = graph["edges"]
        .as_array()
        .expect("edges")
        .iter()
        .map(|edge| {
            let properties: HashMap<Cow<'static, str>, Value> = edge[3]
                .as_object()
                .expect("edge properties")
                .iter()
                .map(|(key, value)| (Cow::Owned(key.clone()), value.clone()))
                .collect();
            (
                edge[0].as_str().expect("source").to_string(),
                edge[1].as_str().expect("target").to_string(),
                edge[2].as_str().expect("relation").to_string(),
                properties,
            )
        })
        .collect();
    mock.add_edges(&edges).await.expect("add edges");
    load_code_graph_snapshot(&mock).await.expect("load")
}

/// Python's `capabilities` lists all fifteen operations; this build lists
/// the ones it runs (see `ops::capabilities`).
fn expected_result(name: &str, mut result: Value) -> Value {
    if result["operation"] == "capabilities" {
        let implemented: Vec<&str> = CodeOperation::implemented()
            .iter()
            .map(|operation| operation.as_str())
            .collect();
        assert_eq!(
            result["operations"].as_array().map(Vec::len),
            Some(15),
            "{name}"
        );
        result["operations"] = json!(implemented);
    }
    result
}

#[tokio::test]
async fn every_operation_matches_python() {
    let snapshot = fixture_snapshot().await;
    let empty = CodeGraphSnapshot::empty();
    let goldens = read_json("goldens.json");
    let goldens = goldens.as_array().expect("a list of cases");
    assert!(
        goldens.len() >= 80,
        "the goldens hold {} cases",
        goldens.len()
    );

    let mut failures = Vec::new();
    for case in goldens {
        let name = case["name"].as_str().expect("case name");
        let config = case["config"].as_object().expect("config").clone();
        let query = case["query"].as_str().expect("query");
        let graph = if name.ends_with("_empty_graph") {
            &empty
        } else {
            &snapshot
        };
        let outcome = CodeRequest::parse(config)
            .and_then(|request| run_code_operation(graph, &request, query));
        match (outcome, case.get("error")) {
            (Ok(result), None) => {
                let expected = expected_result(name, case["result"].clone());
                // `Value` equality ignores object key order; the serialized
                // forms keep it (preserve_order), so Python's dict insertion
                // order — what an unsorted JSON response shows — is checked.
                if result != expected
                    || serde_json::to_string(&result).unwrap()
                        != serde_json::to_string(&expected).unwrap()
                {
                    failures.push(format!(
                        "{name}: result differs\n  rust:   {}\n  python: {}",
                        json_key(&result),
                        json_key(&expected)
                    ));
                    continue;
                }
                let context = if result["operation"] == "capabilities" {
                    json_key(&expected)
                } else {
                    case["context"].as_str().expect("context").to_string()
                };
                if json_key(&result) != context {
                    failures.push(format!(
                        "{name}: context differs\n  rust:   {}\n  python: {context}",
                        json_key(&result)
                    ));
                }
            }
            (Err(error), Some(expected)) => {
                let actual = json!({
                    "type": error.class.python_name(),
                    "message": error.message,
                    "code": error.code.as_str(),
                    "param": error.param,
                });
                if &actual != expected {
                    failures.push(format!(
                        "{name}: error differs\n  rust:   {actual}\n  python: {expected}"
                    ));
                }
            }
            (Ok(result), Some(expected)) => failures.push(format!(
                "{name}: Python raised {expected}, Rust returned {}",
                json_key(&result)
            )),
            (Err(error), None) => failures.push(format!(
                "{name}: Rust raised {error:?}, Python returned {}",
                case["context"]
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} cases differ from Python:\n{}",
        failures.len(),
        goldens.len(),
        failures.join("\n")
    );
}

#[tokio::test]
async fn the_loader_drops_what_is_not_code() {
    let snapshot = fixture_snapshot().await;
    // The DocumentChunk and the NodeSet are not code; the two repositories
    // and the three flows are held aside.
    assert_eq!(snapshot.repositories.len(), 2);
    assert_eq!(snapshot.flows.len(), 3);
    assert!(!snapshot.nodes.contains_key("noise"));
    assert!(!snapshot.nodes.contains_key("nodeset"));
    // The Ladybug-style node is flattened and its null doc dropped.
    let list = &snapshot.nodes["list"];
    assert_eq!(list["file_path"], json!("orders/api.py"));
    assert!(!list.contains_key("doc"));
    assert!(!list.contains_key("properties"));
    // A flow stage resolved through its edge, not its (collapsed) enola id.
    let members = &snapshot.flow_members["flow-orders"];
    assert_eq!(members.entry.as_deref(), Some("create"));
    assert_eq!(
        members.stages,
        vec![
            Some("place".to_string()),
            Some("charge".to_string()),
            Some("validate".to_string()),
            None
        ]
    );
    assert_eq!(members.routes, vec![Some("route".to_string())]);
    assert_eq!(
        snapshot
            .declaration_of
            .get("order-storage")
            .map(String::as_str),
        Some("order-model")
    );
}

#[tokio::test]
async fn unported_operations_and_diagrams_are_feature_unavailable() {
    let snapshot = fixture_snapshot().await;
    for operation in CodeOperation::ALL {
        let config = json!({"operation": operation.as_str()})
            .as_object()
            .expect("object")
            .clone();
        let request = CodeRequest::parse(config).expect("a known operation");
        let outcome = run_code_operation(&snapshot, &request, "create_order");
        if operation.is_implemented() {
            assert!(outcome.is_ok(), "{}: {outcome:?}", operation.as_str());
            continue;
        }
        let error = outcome.expect_err("not implemented");
        assert_eq!(error.code.as_str(), "feature_unavailable");
        assert_eq!(error.param.as_deref(), Some("operation"));
        // A validation error: every dataset would fail it the same way.
        assert!(!error.is_seed_miss());
    }

    let config = json!({"operation": "neighbors", "diagram": "dot"})
        .as_object()
        .expect("object")
        .clone();
    let request = CodeRequest::parse(config).expect("neighbors can be drawn");
    let error = run_code_operation(&snapshot, &request, "create_order").expect_err("no diagrams");
    assert_eq!(error.code.as_str(), "feature_unavailable");
    assert_eq!(error.param.as_deref(), Some("diagram"));

    // No operation means explore, which is not ported yet.
    let request = CodeRequest::parse(serde_json::Map::new()).expect("explore");
    assert_eq!(request.operation, CodeOperation::Explore);
}
