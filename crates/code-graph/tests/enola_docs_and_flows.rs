//! Port of the database-free tests of Python's
//! `tests/unit/tasks/code_graph/test_enola_docs_and_flows.py`: symbol docs
//! and entry-point flows. The golden hashes are Python's own constants.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

mod common;

use std::collections::{BTreeMap, HashSet};

use cognee_code_graph::{
    CODE_INDEX_FIELDS, CodeNode, CodeNodeType, Fact, build_code_graph_edges,
    map_facts_to_data_points, parse_enola_snapshot,
};
use common::{by_type_and_name, entity, facts, id, name_triples, triples, write_snapshot};
use serde_json::{Value, json};

const REPO: &str = "shop";

fn id_of(character: char) -> String {
    character.to_string().repeat(32)
}

fn hashes(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, hash)| (name.to_string(), hash.to_string()))
        .collect()
}

fn fact_hashes(nodes: &[CodeNode]) -> BTreeMap<String, String> {
    common::fact_hashes(nodes).into_iter().collect()
}

fn triple_list(entries: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    entries
        .iter()
        .map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string()))
        .collect()
}

fn parse_and_map(dir: &std::path::Path) -> (Vec<Fact>, Vec<CodeNode>) {
    let (facts, _receipt) = parse_enola_snapshot(dir).unwrap();
    let nodes = map_facts_to_data_points(&facts, Some("/tmp/shop"), None, None);
    (facts, nodes)
}

fn fixture_flows() -> Vec<Value> {
    let text = std::fs::read_to_string(common::docs_and_flows_dir().join("flows.json")).unwrap();
    serde_json::from_str::<Value>(&text).unwrap()["flows"]
        .as_array()
        .unwrap()
        .clone()
}

fn flow_nodes(nodes: &[CodeNode]) -> BTreeMap<String, &cognee_code_graph::CodeGraphEntity> {
    nodes
        .iter()
        .filter_map(CodeNode::as_entity)
        .filter(|entity| entity.node_type() == Some(CodeNodeType::CodeFlow))
        .map(|entity| (entity.name.clone(), entity))
        .collect()
}

// --- a snapshot without docs or flows ingests exactly as before ---------------

#[test]
fn snapshot_without_docs_or_flows_ingests_exactly_as_before() {
    let (facts, nodes) = parse_and_map(&common::basic_dir());
    let (edges, skipped) = build_code_graph_edges(&facts, Some("/tmp/shop"));

    assert!(!facts.iter().any(|fact| fact["kind"] == "flow"));
    assert!(flow_nodes(&nodes).is_empty());
    assert!(
        nodes
            .iter()
            .filter_map(CodeNode::as_entity)
            .all(|entity| entity.doc.is_none())
    );
    assert_eq!(
        fact_hashes(&nodes),
        hashes(&[
            (
                "app",
                "sha256:f2dce3d555c747c7bd1ca9aa7a279021e83f3ce6465a3cc2a6f0845ced2d215d"
            ),
            (
                "utils",
                "sha256:7fa17c384a91bf8dc874247ba467779361276d4f189956235226cd91a38a6b3b"
            ),
            (
                "main",
                "sha256:036e48f152a374d3b8b2be320bea8bba89732d57a5ecbba9964776ad7c857973"
            ),
            (
                "helper",
                "sha256:8499b79c7abc5ad9e4c88f0da87fd967479c811ccde0d6509a67413e2da4b61d"
            ),
            (
                "Config",
                "sha256:3d2e11e16a295f85a000fe0bcb0d056071ab1d164bdaa823c00258cb7e2afe1c"
            ),
            (
                "Load",
                "sha256:4739c575ab3ef79c54a89807769b538b6194235f3b742478b2cbcd8503beba49"
            ),
            (
                "Storer",
                "sha256:74a92ddf832ae406b9c52306c4e139dfa0cafa45aeea5ef4271eeb588ec715d3"
            ),
            (
                "PostgresStore",
                "sha256:138c42e769d5b9b71c44263abf01c9faad71e4ca1c18c735162c8c87e6c123c3"
            ),
            (
                "MAX_RETRIES",
                "sha256:4cc1c614b4b5a684c92210950c7a8d1a07094571f136525b781c8315969acc95"
            ),
            (
                "log_level",
                "sha256:aa3b2d677f70a60dd6676f79bd55687ef608304e6f6eccbb9aa669c7ca87d7c8"
            ),
            (
                "Color",
                "sha256:87ce0bcdeb70cc65a280e54f116f6297e4b2484385d236d02cf6f2c53be4ee0b"
            ),
            (
                "OrderID",
                "sha256:c9957e8e964b4397486a566235c4a06aa8f4c4fdc39ee0ef56e9531c1d6f4394"
            ),
            (
                "GET /orders",
                "sha256:b7458067a4881f61bc89a84b6c87ef20c201ef1a5aebfc5ccd75f3644e9b3805"
            ),
            (
                "orders_table",
                "sha256:46e6ea1b702dc013d9e29bbf8b5118480c9018770129615487e40b74cd7fcd1e"
            ),
            (
                "github.com/lib/pq",
                "sha256:b150c4f0473778b380a078ad9047fbe7a49500b79d63ad8235dd3d4a948ed173"
            ),
            (
                "shop-api",
                "sha256:c2508b513891d886dd921be89c2dd0b946549ef7835010a8095e8ac6bac66d66"
            ),
        ])
    );
    assert_eq!(
        name_triples(&edges, &nodes),
        triple_list(&[
            ("Config", "has_method", "Load"),
            ("GET /orders", "handled_by", "helper"),
            ("PostgresStore", "implements", "Storer"),
            ("app", "declares", "main"),
            ("app", "imports", "utils"),
            ("main", "calls", "helper"),
            ("main", "instantiates", "Config"),
            ("shop-api", "depends_on", "github.com/lib/pq"),
            ("shop-api", "injects", "PostgresStore"),
        ])
    );
    assert_eq!(skipped, 2);
}

#[test]
fn docs_and_flows_leave_undocumented_facts_and_code_edges_untouched() {
    let (facts, nodes) = parse_and_map(&common::docs_and_flows_dir());
    let (edges, skipped) = build_code_graph_edges(&facts, Some("/tmp/shop"));

    let undocumented: BTreeMap<String, String> = nodes
        .iter()
        .filter_map(CodeNode::as_entity)
        .filter(|entity| entity.node_type() != Some(CodeNodeType::CodeFlow) && entity.doc.is_none())
        .map(|entity| (entity.name.clone(), entity.fact_hash.clone().unwrap()))
        .collect();
    assert_eq!(
        undocumented,
        hashes(&[
            (
                ".",
                "sha256:4e7b17a7f021621785446ae4a972c0af46a864d550ff0c1792e22da07aa52f36"
            ),
            (
                "auth",
                "sha256:7d1c8a7a0a7c388a871b61493ee97d5c239ae4ff7c353cb0d73f85160dd33e6e"
            ),
            (
                "orders",
                "sha256:d7e44a4cf9fedad3b08a953d042b757aec71df4d08e34cd786c7c6d8270f4375"
            ),
            (
                "scripts",
                "sha256:a9e327a2ffafe5968664794819e30ff5e6bb6d5de2e715323f93b7ca2edff830"
            ),
            (
                "/checkout",
                "sha256:34c8f4b2f644ca197ba94ebea3edca13a8ddb575de344009b57a593be54dc8f1"
            ),
            (
                "app.serve",
                "sha256:55b611e717d9817215620517935c23f3034fb97400c6379122ef4b44711b927f"
            ),
            (
                "auth/tokens.decode",
                "sha256:59c983d4a68e064f2f6237b26b57c0dd0562f776558fb114c5c229b366a88ca3"
            ),
            (
                "orders/repository.OrderRepository.save",
                "sha256:a8f808fa46be3315fe9af60f43b9510f0fb3bfb7c4503c4d8767c47d6fc4cfde"
            ),
        ])
    );
    let code_edges: Vec<_> = edges
        .iter()
        .filter(|edge| !edge.relationship_name.starts_with("flow_"))
        .cloned()
        .collect();
    assert_eq!(
        name_triples(&code_edges, &nodes),
        triple_list(&[
            ("/checkout", "declares", "orders"),
            ("/checkout", "handled_by", "orders/checkout.place_order"),
            ("app.main", "calls", "app.serve"),
            ("app.main", "declares", "."),
            ("app.serve", "declares", "."),
            ("auth/tokens.decode", "declares", "auth"),
            ("auth/tokens.validate_token", "calls", "auth/tokens.decode"),
            ("auth/tokens.validate_token", "declares", "auth"),
            (
                "orders/checkout.place_order",
                "calls",
                "auth/tokens.validate_token"
            ),
            (
                "orders/checkout.place_order",
                "calls",
                "orders/repository.OrderRepository.save"
            ),
            ("orders/checkout.place_order", "declares", "orders"),
            ("orders/repository.OrderRepository", "declares", "orders"),
            (
                "orders/repository.OrderRepository",
                "has_method",
                "orders/repository.OrderRepository.save"
            ),
            (
                "orders/repository.OrderRepository.save",
                "declares",
                "orders"
            ),
            (
                "scripts/seed.main",
                "calls",
                "orders/repository.OrderRepository.save"
            ),
            ("scripts/seed.main", "declares", "scripts"),
        ])
    );
    // The call into a library (jwt.decode) is the only skip.
    assert_eq!(skipped, 1);
}

#[test]
fn empty_or_non_text_docs_and_docs_on_insights_change_nothing() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "app/db.connect", "file": "app/db.py", "line": 3, "repo": REPO,
         "props": {"doc": "", "symbol_kind": "function"}},
        {"kind": "symbol", "name": "app/db.ping", "file": "app/db.py", "line": 15, "repo": REPO,
         "props": {"doc": "   ", "symbol_kind": "function"}},
        {"kind": "symbol", "name": "app/db.close", "file": "app/db.py", "line": 9, "repo": REPO,
         "props": {"doc": ["not", "prose"], "symbol_kind": "function"}},
        {"kind": "insight", "name": "Call-graph hotspot: app/db.connect",
         "props": {"source": "hotspots", "description": "A pinch point.", "doc": "Not a doc comment."}},
        {"kind": "insight", "name": "Empty finding", "props": {"source": "domain", "description": ""}},
        {"kind": "insight", "name": "Bare finding", "props": {"source": "layers", "confidence": 0.5}},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/tmp/shop"), None, None);
    assert!(
        nodes
            .iter()
            .filter_map(CodeNode::as_entity)
            .all(|entity| entity.doc.is_none())
    );
    assert_eq!(
        entity(common::by_name(
            &nodes,
            "Call-graph hotspot: app/db.connect"
        ))
        .description
        .as_deref(),
        Some("A pinch point.")
    );
    assert_eq!(
        entity(common::by_name(&nodes, "Empty finding"))
            .description
            .as_deref(),
        Some("")
    );
    assert_eq!(
        fact_hashes(&nodes),
        hashes(&[
            (
                "app/db.connect",
                "sha256:b765e80dee56352e5bbbbe08d5f6f9ee4ddb8249ce09b5aeb38b9f67edc59a3a"
            ),
            (
                "app/db.ping",
                "sha256:3e778ddbe0405c83c53e176d64296b6c52955136cdfa236bf73743656a0d54ab"
            ),
            (
                "app/db.close",
                "sha256:54e056fb77d11a5fedca01854c9c32ae08978843b1ac9a58927ea6357c770675"
            ),
            (
                "Call-graph hotspot: app/db.connect",
                "sha256:3caef8cd94ea8ae8e81dd26d585805650a1e2b2466df55e801f92338da523f62"
            ),
            (
                "Empty finding",
                "sha256:ed33ed0a2e1fd0e57cab45b707d68ce4d5547f384c3daa9dc3aa654e750fe1c0"
            ),
            (
                "Bare finding",
                "sha256:008084dd5b32f3f5c3534a8d116829631d62be0718f1092deee083f0a4512525"
            ),
        ])
    );
}

// --- docs ---------------------------------------------------------------------

#[test]
fn documented_symbol_carries_its_doc_as_doc_and_description() {
    let (_facts, nodes) = parse_and_map(&common::docs_and_flows_dir());
    let validate = entity(by_type_and_name(
        &nodes,
        "CodeSymbol",
        "auth/tokens.validate_token",
    ));
    let doc = "Check a bearer token's signature and expiry, then return the user it was issued to.";
    assert_eq!(validate.doc.as_deref(), Some(doc));
    assert_eq!(validate.description, validate.doc);
    assert_eq!(validate.fact_properties["doc"], doc);
    let repository = entity(by_type_and_name(
        &nodes,
        "CodeSymbol",
        "orders/repository.OrderRepository",
    ));
    assert_eq!(
        repository.doc.as_deref(),
        Some("Persist orders in the orders table.")
    );
    assert_eq!(repository.description, repository.doc);
}

#[test]
fn undocumented_symbol_has_no_doc_and_keeps_the_generic_description() {
    let (_facts, nodes) = parse_and_map(&common::docs_and_flows_dir());
    let serve = entity(by_type_and_name(&nodes, "CodeSymbol", "app.serve"));
    assert_eq!(serve.doc, None);
    assert_eq!(
        serve.description.as_deref(),
        Some("symbol: language=python, symbol_kind=function")
    );
}

#[test]
fn a_doc_on_any_kind_but_insight_is_read() {
    let facts = facts(
        json!([{"kind": "route", "name": "/health", "repo": REPO, "props": {"doc": "Liveness."}}]),
    );
    let nodes = map_facts_to_data_points(&facts, None, None, None);
    let route = entity(&nodes[1]);
    assert_eq!(route.doc.as_deref(), Some("Liveness."));
    assert_eq!(route.description.as_deref(), Some("Liveness."));
}

#[test]
fn docs_and_flows_add_no_embedded_field() {
    assert_eq!(CODE_INDEX_FIELDS, &["name"]);
    let (_facts, nodes) = parse_and_map(&common::docs_and_flows_dir());
    for node in &nodes {
        assert_eq!(
            node.graph_properties()["metadata"],
            json!({"index_fields": ["name"]})
        );
    }
}

// --- flows --------------------------------------------------------------------

#[test]
fn each_flow_becomes_a_fact_linked_to_its_entry_stages_and_routes() {
    let (facts, _receipt) = parse_enola_snapshot(common::docs_and_flows_dir()).unwrap();
    let flow = &fixture_flows()[0];
    assert_eq!(
        facts.iter().filter(|fact| fact["kind"] == "flow").count(),
        3
    );
    let last_names: Vec<&Value> = facts[facts.len() - 3..]
        .iter()
        .map(|fact| &fact["name"])
        .collect();
    assert_eq!(
        last_names,
        vec!["POST /checkout", "app.main", "scripts/seed.main"]
    );

    let relation = |kind: &str, reference: &Value| json!({"kind": kind, "target": reference["name"], "target_id": reference["id"]});
    let mut relations = vec![relation("flow_entry", &flow["entry"])];
    relations.extend(
        flow["stages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|stage| relation("flow_stage", stage)),
    );
    relations.extend(
        flow["routes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|route| relation("flow_route", route)),
    );
    let mut checkout = facts[facts.len() - 3].clone();
    checkout.remove("props");
    assert_eq!(
        Value::Object(checkout),
        json!({
            "kind": "flow",
            "name": "POST /checkout",
            "repo": REPO,
            "file": "orders/checkout.py",
            "line": 14,
            "id": flow["id"],
            "relations": relations,
        })
    );
}

#[test]
fn flows_json_becomes_one_code_flow_per_flow() {
    let (_facts, nodes) = parse_and_map(&common::docs_and_flows_dir());
    let flows: BTreeMap<String, Value> = fixture_flows()
        .into_iter()
        .map(|flow| (flow["name"].as_str().unwrap().to_string(), flow))
        .collect();
    let flow_points = flow_nodes(&nodes);
    assert_eq!(
        flow_points.keys().collect::<Vec<_>>(),
        flows.keys().collect::<Vec<_>>()
    );
    let checkout = flow_points["POST /checkout"];
    assert_eq!(checkout.base.id, id(REPO, "flow", "POST /checkout"));
    assert_eq!(checkout.kind, "flow");
    assert_eq!(checkout.repo.as_deref(), Some(REPO));
    assert_eq!(
        checkout.enola_id.as_deref(),
        flows["POST /checkout"]["id"].as_str()
    );
    assert_eq!(checkout.file_path.as_deref(), Some("orders/checkout.py"));
    assert_eq!(checkout.line, Some(14));
    assert_eq!(
        checkout.description.as_deref(),
        flows["POST /checkout"]["summary"].as_str()
    );
    assert_eq!(checkout.doc, None);
    let expected: serde_json::Map<String, Value> = flows["POST /checkout"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "id" | "name" | "repo"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    assert_eq!(checkout.fact_properties, expected);
    assert_eq!(checkout.part_of, Some(id(REPO, "repository", REPO)));
    assert_eq!(
        flow_points["scripts/seed.main"].fact_properties["side"],
        true
    );
    assert!(!flow_points["app.main"].fact_properties.contains_key("side"));
}

#[test]
fn flow_edges_join_each_flow_to_its_entry_stages_and_routes() {
    let (facts, _nodes) = parse_and_map(&common::docs_and_flows_dir());
    let (edges, _skipped) = build_code_graph_edges(&facts, Some("/tmp/shop"));
    let node = |kind: &str, name: &str| id(REPO, kind, name);
    let checkout = node("flow", "POST /checkout");
    let main = node("flow", "app.main");
    let seed = node("flow", "scripts/seed.main");
    let save = node("symbol", "orders/repository.OrderRepository.save");
    let flow_edges: HashSet<_> = triples(&edges)
        .into_iter()
        .filter(|(_, relation, _)| relation.starts_with("flow_"))
        .collect();
    let expected: HashSet<_> = [
        (
            checkout,
            "flow_entry",
            node("symbol", "orders/checkout.place_order"),
        ),
        (
            checkout,
            "flow_stage",
            node("symbol", "auth/tokens.validate_token"),
        ),
        (checkout, "flow_stage", node("symbol", "auth/tokens.decode")),
        (checkout, "flow_stage", save),
        (checkout, "flow_route", node("route", "/checkout")),
        (main, "flow_entry", node("symbol", "app.main")),
        (main, "flow_stage", node("symbol", "app.serve")),
        (seed, "flow_entry", node("symbol", "scripts/seed.main")),
        (seed, "flow_stage", save),
    ]
    .into_iter()
    .map(|(source, relation, target)| (source, relation.to_string(), target))
    .collect();
    assert_eq!(flow_edges, expected);
}

#[test]
fn flow_refs_resolve_by_fact_id_before_name() {
    let dir = tempfile::tempdir().unwrap();
    let raw = json!([
        {"kind": "symbol", "name": "handlers.checkout", "file": "a.py", "repo": REPO, "id": id_of('a')},
        {"kind": "symbol", "name": "handlers.checkout", "file": "b.py", "repo": "web", "id": id_of('b')},
        {"kind": "symbol", "name": "orders.save", "file": "c.py", "repo": REPO},
    ]);
    let flows = json!({"flows": [{
        "name": "handlers.checkout",
        "repo": REPO,
        "entry": {"name": "handlers.checkout", "id": id_of('b')},
        "stages": [{"order": 1, "depth": 1, "name": "orders.save", "reach": 1}],
    }]});
    write_snapshot(dir.path(), &raw, None, None, Some(&flows));
    let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let (edges, skipped) = build_code_graph_edges(&parsed, None);
    let flow = id(REPO, "flow", "handlers.checkout");
    assert_eq!(skipped, 0);
    assert_eq!(
        triples(&edges),
        HashSet::from([
            (
                flow,
                "flow_entry".to_string(),
                id("web", "symbol", "handlers.checkout")
            ),
            (
                flow,
                "flow_stage".to_string(),
                id(REPO, "symbol", "orders.save")
            ),
        ])
    );
}

#[test]
fn flows_sharing_a_route_name_stay_separate_nodes_with_their_own_edges() {
    let dir = tempfile::tempdir().unwrap();
    let raw = json!([
        {"kind": "symbol", "name": "svc_a/app.health", "file": "svc_a/app.py", "repo": REPO},
        {"kind": "symbol", "name": "svc_b/app.health", "file": "svc_b/app.py", "repo": REPO},
        {"kind": "symbol", "name": "svc_a/app.work", "file": "svc_a/app.py", "repo": REPO},
        {"kind": "symbol", "name": "svc_b/app.work", "file": "svc_b/app.py", "repo": REPO},
        {"kind": "symbol", "name": "orders.save", "file": "orders.py", "repo": REPO},
    ]);
    let flows = json!({"flows": [
        {"name": "GET /health", "repo": REPO, "id": id_of('a'), "summary": "svc_a health",
         "entry": {"name": "svc_a/app.health"},
         "stages": [{"order": 1, "depth": 1, "name": "svc_a/app.work"}]},
        {"name": "GET /health", "repo": REPO, "id": id_of('b'), "summary": "svc_b health",
         "entry": {"name": "svc_b/app.health"},
         "stages": [{"order": 1, "depth": 1, "name": "svc_b/app.work"}]},
        {"name": "POST /orders", "repo": REPO, "entry": {"name": "orders.save"}},
    ]});
    write_snapshot(dir.path(), &raw, None, None, Some(&flows));
    let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let nodes = map_facts_to_data_points(&parsed, Some("/tmp/shop"), None, None);
    let (edges, skipped) = build_code_graph_edges(&parsed, Some("/tmp/shop"));

    let flow_points = flow_nodes(&nodes);
    assert_eq!(
        flow_points.keys().cloned().collect::<HashSet<_>>(),
        HashSet::from([
            "GET /health (svc_a/app.health)".to_string(),
            "GET /health (svc_b/app.health)".to_string(),
            "POST /orders".to_string(),
        ])
    );
    assert_eq!(
        flow_points["POST /orders"].base.id,
        id(REPO, "flow", "POST /orders")
    );
    assert_eq!(skipped, 0);
    let flow_edges: HashSet<_> = name_triples(&edges, &nodes)
        .into_iter()
        .filter(|(source, _, _)| flow_points.contains_key(source))
        .collect();
    assert_eq!(
        flow_edges,
        triple_list(&[
            (
                "GET /health (svc_a/app.health)",
                "flow_entry",
                "svc_a/app.health"
            ),
            (
                "GET /health (svc_a/app.health)",
                "flow_stage",
                "svc_a/app.work"
            ),
            (
                "GET /health (svc_b/app.health)",
                "flow_entry",
                "svc_b/app.health"
            ),
            (
                "GET /health (svc_b/app.health)",
                "flow_stage",
                "svc_b/app.work"
            ),
            ("POST /orders", "flow_entry", "orders.save"),
        ])
        .into_iter()
        .collect()
    );
}

#[test]
fn flows_sharing_a_name_and_an_entry_fall_back_to_their_ids() {
    let dir = tempfile::tempdir().unwrap();
    let raw = json!([{"kind": "symbol", "name": "app.health", "file": "app.py", "repo": REPO}]);
    let flows = json!({"flows": [
        {"name": "GET /health", "repo": REPO, "id": id_of('a'), "entry": {"name": "app.health"}},
        {"name": "GET /health", "repo": REPO, "id": id_of('b'), "entry": {"name": "app.health"}},
        {"name": "GET /ping", "repo": REPO, "entry": {"name": "app.health"}},
        {"name": "GET /ping", "repo": REPO, "entry": {"name": "app.health"}},
    ]});
    write_snapshot(dir.path(), &raw, None, None, Some(&flows));
    let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let mut names: Vec<String> = parsed
        .iter()
        .filter(|fact| fact["kind"] == "flow")
        .map(|fact| fact["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            format!("GET /health (app.health) ({})", id_of('a')),
            format!("GET /health (app.health) ({})", id_of('b')),
            "GET /ping (app.health) #1".to_string(),
            "GET /ping (app.health) #2".to_string(),
        ]
    );
}

#[test]
fn a_flow_named_like_its_entry_leaves_name_resolution_unambiguous() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "jobs.send_receipt", "repo": REPO},
        {"kind": "symbol", "name": "orders.complete", "repo": REPO, "relations": [
            {"kind": "calls", "target": "jobs.send_receipt"},
            {"kind": "calls", "target": "send_receipt"},
        ]},
        {"kind": "flow", "name": "jobs.send_receipt", "repo": REPO,
         "relations": [{"kind": "flow_entry", "target": "jobs.send_receipt"}]},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    let entry = id(REPO, "symbol", "jobs.send_receipt");
    assert_eq!(skipped, 0);
    assert_eq!(
        triples(&edges),
        HashSet::from([
            (
                id(REPO, "symbol", "orders.complete"),
                "calls".to_string(),
                entry
            ),
            (
                id(REPO, "flow", "jobs.send_receipt"),
                "flow_entry".to_string(),
                entry
            ),
        ])
    );
}

#[test]
fn flow_props_keep_the_documented_fields_that_are_set() {
    let dir = tempfile::tempdir().unwrap();
    let flow = json!({
        "id": id_of('c'),
        "name": "POST /orders",
        "rank": 2,
        "repo": REPO,
        "mechanism": "route",
        "side": false,
        "routes": [{"name": "/orders", "id": id_of('a'), "method": "POST"}],
        "entry": {"name": "orders.create", "id": id_of('b'), "file": "orders.py", "line": 4},
        "summary": "POST /orders is served by orders.create.",
        "metrics": {"reach": 1, "modules": 1, "max_depth": 0, "unresolved_calls": 0},
        "modules": [{"name": "orders", "symbols": 1}],
        "same_reach_as": ["PUT /orders"],
        "nested_in": null,
        "renderer_hint": "not part of the contract",
    });
    let raw = json!([{"kind": "symbol", "name": "orders.create", "repo": REPO, "id": id_of('b')}]);
    write_snapshot(
        dir.path(),
        &raw,
        None,
        None,
        Some(&json!({"flows": [flow.clone()]})),
    );
    let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let expected: serde_json::Map<String, Value> = flow
        .as_object()
        .unwrap()
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "id" | "name" | "repo" | "side" | "nested_in" | "renderer_hint"
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    assert_eq!(parsed.last().unwrap()["props"], Value::Object(expected));
}

#[test]
fn flows_of_other_repositories_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let raw = json!([{"kind": "symbol", "name": "app.main", "file": "shop/app.py", "repo": REPO}]);
    let flows = json!({"flows": [
        {"name": "app.main", "repo": REPO, "entry": {"name": "app.main"}},
        {"name": "invoices.main", "repo": "billing", "entry": {"name": "invoices.main"}},
        {"name": "unlabelled.main", "entry": {"name": "unlabelled.main"}},
    ]});
    write_snapshot(dir.path(), &raw, None, None, Some(&flows));
    let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let flows: Vec<(&Value, &Value)> = parsed
        .iter()
        .filter(|fact| fact["kind"] == "flow")
        .map(|fact| (&fact["name"], &fact["repo"]))
        .collect();
    assert_eq!(flows, vec![(&json!("app.main"), &json!(REPO))]);
}

fn unlabelled_facts() -> Value {
    json!([
        {"kind": "symbol", "name": "main", "file": "main.go", "line": 3,
         "props": {"symbol_kind": "function"}, "relations": [{"kind": "calls", "target": "helper"}]},
        {"kind": "symbol", "name": "helper", "file": "main.go", "line": 9,
         "props": {"symbol_kind": "function"}},
    ])
}

#[test]
fn without_repo_labels_every_flow_is_kept_in_the_facts_repository() {
    let dir = tempfile::tempdir().unwrap();
    let flows = json!({"flows": [{
        "name": "main", "repo": "elsewhere", "mechanism": "main",
        "entry": {"name": "main", "file": "main.go", "line": 3},
    }]});
    write_snapshot(dir.path(), &unlabelled_facts(), None, None, Some(&flows));
    let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let nodes = map_facts_to_data_points(&parsed, Some("/repos/myrepo"), None, None);

    assert!(!parsed.last().unwrap().contains_key("repo"));
    let hash = |name: &str| {
        entity(by_type_and_name(&nodes, "CodeSymbol", name))
            .fact_hash
            .clone()
            .unwrap()
    };
    assert_eq!(
        hash("main"),
        "sha256:9d055a17d6673f97970de0fa698c6ca8bbb6e836b4759bddb2327a1274e5b74a"
    );
    assert_eq!(
        hash("helper"),
        "sha256:220cac306e84fbfcae9d39dbecf68fc1e9562d402203769c191d2fce1a824e24"
    );
    assert_eq!(
        by_type_and_name(&nodes, "CodeSymbol", "main").id(),
        id("myrepo", "symbol", "main")
    );
    assert_eq!(
        entity(by_type_and_name(&nodes, "CodeFlow", "main"))
            .repo
            .as_deref(),
        Some("myrepo")
    );
    assert_eq!(
        nodes
            .iter()
            .filter(|node| node.as_repository().is_some())
            .count(),
        1
    );
}

#[test]
fn unreadable_or_odd_flows_json_adds_no_flows() {
    let contents: [&[u8]; 6] = [
        b"{ not json",
        b"\xff\xfe not utf-8",
        b"[]",
        b"\"flows\"",
        b"{\"flows\": {\"name\": \"app.main\"}}",
        b"{}",
    ];
    for content in contents {
        let dir = tempfile::tempdir().unwrap();
        write_snapshot(
            dir.path(),
            &json!([{"kind": "symbol", "name": "app.main", "repo": REPO}]),
            None,
            None,
            None,
        );
        std::fs::write(dir.path().join("flows.json"), content).unwrap();
        let (facts, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
        let kinds: Vec<&Value> = facts.iter().map(|fact| &fact["kind"]).collect();
        assert_eq!(kinds, vec![&json!("symbol")]);
    }
}

#[test]
fn flows_without_a_usable_name_are_skipped_and_bad_refs_link_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let flows = json!({"flows": [
        "not an object",
        {"rank": 1},
        {"name": ""},
        {"name": 7},
        {"name": "worker.run", "repo": REPO, "id": "not-an-enola-id", "entry": "not an object",
         "stages": "not a list", "routes": [1, {"id": id_of('a')}, {"name": ""}]},
    ]});
    write_snapshot(
        dir.path(),
        &json!([{"kind": "symbol", "name": "worker.run", "repo": REPO}]),
        None,
        None,
        Some(&flows),
    );
    let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
    let flow_facts: Vec<_> = parsed
        .iter()
        .filter(|fact| fact["kind"] == "flow")
        .collect();
    assert_eq!(flow_facts.len(), 1);
    assert_eq!(flow_facts[0]["name"], "worker.run");
    assert_eq!(flow_facts[0]["relations"], json!([]));
    assert!(!flow_facts[0].contains_key("id"));
    assert!(!flow_facts[0].contains_key("file"));
}

#[test]
fn receipt_counts_facts_jsonl_alone() {
    let (facts, receipt) = parse_enola_snapshot(common::docs_and_flows_dir()).unwrap();
    assert_eq!(receipt.unwrap()["fact_count"], 13);
    assert_eq!(facts.len(), 16);
}

#[test]
fn flows_json_is_read_only_when_the_receipt_vouches_for_it() {
    // (receipt fields, whether flows.json is read); "THESE" stands for the
    // hash of the flows.json written.
    let cases = [
        (
            json!({"renderers": ["llm_context"], "output_hashes": {"facts.jsonl": "sha256:0", "insights.json": "sha256:1"}}),
            false,
        ),
        (
            json!({"renderers": ["llm_context", "flows"], "output_hashes": {"flows.json": "THESE"}}),
            true,
        ),
        (
            json!({"renderers": ["flows"], "output_hashes": {"flows.json": format!("sha256:{}", "0".repeat(64))}}),
            false,
        ),
        (json!({"renderers": ["flows"]}), true),
        (json!({"renderers": ["llm_context"]}), false),
        (json!({}), false),
    ];
    for (fields, read) in cases {
        let dir = tempfile::tempdir().unwrap();
        let raw = json!([{"kind": "symbol", "name": "app.main", "file": "app.py", "line": 1, "repo": REPO}]);
        let flows =
            json!({"flows": [{"name": "app.main", "repo": REPO, "entry": {"name": "app.main"}}]});
        write_snapshot(dir.path(), &raw, None, None, Some(&flows));
        let flows_hash = {
            use sha2::Digest;
            format!(
                "sha256:{}",
                hex::encode(sha2::Sha256::digest(
                    std::fs::read(dir.path().join("flows.json")).unwrap()
                ))
            )
        };
        let mut receipt =
            json!({"snapshot_id": "sha256:5e1f", "format_version": 1, "fact_count": 1});
        for (key, value) in fields.as_object().unwrap() {
            let mut value = value.clone();
            if key == "output_hashes" {
                for digest in value.as_object_mut().unwrap().values_mut() {
                    if digest == "THESE" {
                        *digest = json!(flows_hash);
                    }
                }
            }
            receipt[key] = value;
        }
        std::fs::write(dir.path().join("receipt.json"), receipt.to_string()).unwrap();
        let (parsed, _receipt) = parse_enola_snapshot(dir.path()).unwrap();
        let flow_names: Vec<&Value> = parsed
            .iter()
            .filter(|fact| fact["kind"] == "flow")
            .map(|fact| &fact["name"])
            .collect();
        let expected: Vec<&Value> = if read { vec![&raw[0]["name"]] } else { vec![] };
        assert_eq!(flow_names, expected, "{fields}");
    }
}
