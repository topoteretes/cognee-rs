//! Port of the database-free tests of Python's
//! `tests/unit/tasks/code_graph/test_extract_code_graph.py`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

mod common;

use std::collections::{BTreeSet, HashSet};

use cognee_code_graph::{
    CodeNodeType, build_code_graph_edges, map_facts_to_data_points, parse_enola_snapshot,
};
use common::{by_name, entity, facts, id, triples};
use serde_json::json;

fn fixture_facts() -> Vec<cognee_code_graph::Fact> {
    parse_enola_snapshot(common::basic_dir()).unwrap().0
}

#[test]
fn every_kind_maps_to_the_right_data_point_class() {
    let nodes = map_facts_to_data_points(&fixture_facts(), Some("/tmp/shop"), None, None);
    let node_type = |name: &str| by_name(&nodes, name).node_type().unwrap();
    assert_eq!(node_type("acme/shop"), CodeNodeType::CodeRepository);
    assert_eq!(node_type("app"), CodeNodeType::CodeModule);
    assert_eq!(node_type("main"), CodeNodeType::CodeSymbol);
    assert_eq!(node_type("GET /orders"), CodeNodeType::ApiEndpoint);
    assert_eq!(node_type("orders_table"), CodeNodeType::StorageResource);
    assert_eq!(
        node_type("github.com/lib/pq"),
        CodeNodeType::ExternalDependency
    );
    assert_eq!(node_type("shop-api"), CodeNodeType::CodeService);
    // 16 facts + the CodeRepository node.
    assert_eq!(nodes.len(), 17);
}

#[test]
fn symbol_kind_and_common_fields_are_mapped() {
    let nodes = map_facts_to_data_points(&fixture_facts(), Some("/tmp/shop"), None, None);
    let main = entity(by_name(&nodes, "main"));
    assert_eq!(main.symbol_kind.as_deref(), Some("function"));
    assert_eq!(main.file_path.as_deref(), Some("app/main.go"));
    assert_eq!(main.line, Some(12));
    assert_eq!(main.repo.as_deref(), Some("acme/shop"));
    assert_eq!(main.kind, "symbol");
    assert_eq!(
        json!(main.fact_properties),
        json!({"symbol_kind": "function"})
    );
    assert_eq!(main.part_of, Some(by_name(&nodes, "acme/shop").id()));

    let route = entity(by_name(&nodes, "GET /orders"));
    assert_eq!(route.kind, "route");
    assert_eq!(
        json!(route.fact_properties),
        json!({"method": "GET", "path": "/orders"})
    );

    let symbol_kinds: BTreeSet<&str> = nodes
        .iter()
        .filter_map(|node| node.as_entity())
        .filter(|entity| entity.node_type() == Some(CodeNodeType::CodeSymbol))
        .filter_map(|entity| entity.symbol_kind.as_deref())
        .collect();
    assert_eq!(
        symbol_kinds,
        [
            "function",
            "method",
            "struct",
            "interface",
            "class",
            "constant",
            "variable",
            "enum",
            "type"
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn ids_are_deterministic_across_runs() {
    let first = map_facts_to_data_points(&fixture_facts(), Some("/tmp/shop"), None, None);
    let second = map_facts_to_data_points(&fixture_facts(), Some("/tmp/shop"), None, None);
    let ids = |nodes: &[cognee_code_graph::CodeNode]| {
        let mut ids: Vec<String> = nodes.iter().map(|node| node.id().to_string()).collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(&first), ids(&second));
    assert_eq!(
        ids(&first).into_iter().collect::<HashSet<_>>().len(),
        first.len()
    );
}

#[test]
fn build_code_graph_edges_resolves_and_skips() {
    let (edges, skipped) = build_code_graph_edges(&fixture_facts(), None);
    // One relation targets "ghost_function", one has un-normalisable keys.
    assert_eq!(skipped, 2);
    let names: BTreeSet<&str> = edges
        .iter()
        .map(|edge| edge.relationship_name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "declares",
            "imports",
            "calls",
            "instantiates",
            "has_method",
            "implements",
            "handled_by",
            "depends_on",
            "injects"
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(edges.len(), 9);
}

#[test]
fn edge_tuples_have_correct_source_relationship_target() {
    let (edges, _skipped) = build_code_graph_edges(&fixture_facts(), None);
    let source = id("acme/shop", "symbol", "main");
    let target = id("acme/shop", "symbol", "helper");
    let calls: Vec<_> = edges
        .iter()
        .filter(|edge| edge.relationship_name == "calls")
        .collect();
    assert_eq!(calls.len(), 1);
    let edge = calls[0];
    assert_eq!((edge.source_id, edge.target_id), (source, target));
    assert_eq!(edge.properties["source_node_id"], source.to_string());
    assert_eq!(edge.properties["target_node_id"], target.to_string());
    assert_eq!(edge.properties["relationship_name"], "calls");
    assert_eq!(edge.properties["edge_text"], "calls");
    assert!(
        edge.properties["updated_at"]
            .as_str()
            .unwrap()
            .ends_with("+00:00")
    );
}

#[test]
fn unknown_kind_and_missing_name_facts_are_skipped() {
    let facts = facts(json!([
        {"kind": "unknown_kind", "name": "x"},
        {"kind": "symbol"},
        {"kind": "symbol", "name": ""},
        {"kind": "module", "name": "app", "repo": "acme/shop"},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/tmp/shop"), None, None);
    let names: HashSet<&str> = nodes.iter().map(|node| node.name()).collect();
    assert_eq!(names, HashSet::from(["acme/shop", "app"]));
}

#[test]
fn test_and_file_reference_facts_are_preserved_for_impact_analysis() {
    let facts = facts(json!([
        {"kind": "test_ref", "name": "TestCheckout", "file": "checkout_test.go", "repo": "r",
         "relations": [{"type": "calls", "target": "Checkout"}]},
        {"kind": "file_ref", "name": "cmd/main.go", "file": "cmd/main.go", "repo": "r",
         "relations": [{"type": "calls", "target": "Checkout"}]},
        {"kind": "symbol", "name": "Checkout", "repo": "r", "props": {"symbol_kind": "function"}},
    ]));
    let nodes = map_facts_to_data_points(&facts, None, None, None);
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert_eq!(
        by_name(&nodes, "TestCheckout").node_type(),
        Some(CodeNodeType::CodeTestReference)
    );
    assert_eq!(
        by_name(&nodes, "cmd/main.go").node_type(),
        Some(CodeNodeType::CodeFileReference)
    );
    assert_eq!(skipped, 0);
    assert_eq!(
        edges
            .iter()
            .filter(|edge| edge.relationship_name == "calls")
            .count(),
        2
    );
}

#[test]
fn malformed_field_types_are_skipped_not_fatal() {
    let facts = facts(json!([
        {"kind": ["module"], "name": "x"},
        {"kind": "module", "name": 42},
        {"kind": "module", "name": "m", "line": "abc", "repo": "acme/shop"},
        {"kind": "module", "name": "app", "file": 7, "props": ["not", "a", "dict"]},
        {"kind": "module", "name": "ok", "line": 3, "repo": "acme/shop"},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/tmp/shop"), None, None);
    assert_eq!(entity(by_name(&nodes, "ok")).line, Some(3));
    assert_eq!(entity(by_name(&nodes, "m")).line, None);
    assert_eq!(entity(by_name(&nodes, "app")).file_path, None);
    assert!(nodes.iter().all(|node| node.name() != "x"));
    let (edges, _skipped) = build_code_graph_edges(&facts, Some("/tmp/shop"));
    assert!(edges.is_empty());
}

#[test]
fn a_symbol_with_a_non_text_symbol_kind_is_skipped_like_a_model_validation_error() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "good", "repo": "r", "props": {"symbol_kind": null}},
        {"kind": "symbol", "name": "bad", "repo": "elsewhere", "props": {"symbol_kind": 3}},
    ]));
    let nodes = map_facts_to_data_points(&facts, None, None, None);
    let names: Vec<&str> = nodes.iter().map(|node| node.name()).collect();
    // The rejected fact still created its repository: Python prints
    // ['r', 'elsewhere', 'good'] for these facts.
    assert_eq!(names, vec!["r", "elsewhere", "good"]);
    assert_eq!(entity(by_name(&nodes, "good")).symbol_kind, None);
}

#[test]
fn node_and_edge_ids_agree_when_facts_have_no_repo_field() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "main", "relations": [{"type": "calls", "target": "helper"}]},
        {"kind": "symbol", "name": "helper"},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/path/to/myrepo"), None, None);
    let (edges, skipped) = build_code_graph_edges(&facts, Some("/path/to/myrepo"));
    let node_ids: HashSet<_> = nodes.iter().map(|node| node.id()).collect();
    assert_eq!(skipped, 0);
    assert_eq!(edges.len(), 1);
    assert!(node_ids.contains(&edges[0].source_id));
    assert!(node_ids.contains(&edges[0].target_id));
    assert_eq!(edges[0].source_id, id("myrepo", "symbol", "main"));
}

#[test]
fn duplicate_names_in_same_repo_resolve_to_the_single_shared_node() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "init", "file": "a.go", "repo": "r"},
        {"kind": "symbol", "name": "init", "file": "b.go", "repo": "r"},
        {"kind": "symbol", "name": "main", "repo": "r", "relations": [{"type": "calls", "target": "init"}]},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert_eq!(skipped, 0);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].target_id, id("r", "symbol", "init"));
}

#[test]
fn ambiguous_target_prefers_same_repo_else_skips() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "helper", "repo": "a"},
        {"kind": "symbol", "name": "helper", "repo": "b"},
        {"kind": "symbol", "name": "main", "repo": "a", "relations": [{"type": "calls", "target": "helper"}]},
        {"kind": "symbol", "name": "other", "repo": "c", "relations": [{"type": "calls", "target": "helper"}]},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert_eq!(skipped, 1);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].source_id, id("a", "symbol", "main"));
    assert_eq!(edges[0].target_id, id("a", "symbol", "helper"));
}

#[test]
fn duplicated_relation_emits_a_single_edge() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "main", "repo": "r", "relations": [
            {"type": "calls", "target": "helper"},
            {"type": "calls", "target": "helper"},
        ]},
        {"kind": "symbol", "name": "helper", "repo": "r"},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert_eq!(skipped, 0);
    assert_eq!(edges.len(), 1);
}

#[test]
fn relations_that_are_not_a_list_are_iterated_as_python_does() {
    // Python iterates `fact.get("relations") or []`: a string yields its
    // characters and a dict its keys, none of which normalise (Python counts
    // 3 skips for "xy" and {"k": 1}). An int is not iterable: Python raises
    // TypeError there, Rust reads it as no relations.
    let facts = facts(json!([
        {"kind": "symbol", "name": "a", "repo": "r", "relations": "xy"},
        {"kind": "symbol", "name": "b", "repo": "r", "relations": {"k": 1}},
        {"kind": "symbol", "name": "c", "repo": "r", "relations": 7},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert!(edges.is_empty());
    assert_eq!(skipped, 3);
}

#[test]
fn build_code_graph_edges_synthesizes_type_membership_without_duplicates() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "auth.AuthHandler", "repo": "r", "props": {"symbol_kind": "struct"},
         "relations": [{"type": "has_method", "target": "auth.AuthHandler.Login"}]},
        {"kind": "symbol", "name": "auth.AuthHandler.Login", "repo": "r", "props": {"symbol_kind": "method"}},
        {"kind": "symbol", "name": "auth.AuthHandler.helper", "repo": "r", "props": {"symbol_kind": "function"}},
        {"kind": "symbol", "name": "auth.Sign", "repo": "r", "props": {"symbol_kind": "function"}},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    let owner = id("r", "symbol", "auth.AuthHandler");
    let membership: HashSet<_> = edges
        .iter()
        .filter(|edge| edge.relationship_name == "has_method")
        .map(|edge| (edge.source_id, edge.target_id))
        .collect();
    assert_eq!(skipped, 0);
    assert_eq!(
        membership,
        HashSet::from([
            (owner, id("r", "symbol", "auth.AuthHandler.Login")),
            (owner, id("r", "symbol", "auth.AuthHandler.helper")),
        ])
    );
}

#[test]
fn build_code_graph_edges_synthesizes_module_import_bridge() {
    let facts = facts(json!([
        {"kind": "module", "name": "internal/server", "repo": "r"},
        {"kind": "module", "name": "internal/config", "repo": "r"},
        {"kind": "dependency", "name": "config-import", "file": "r/internal/server/server.go", "repo": "r",
         "relations": [{"type": "imports", "target": "internal/config/value"}]},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    let imports: HashSet<_> = edges
        .iter()
        .filter(|edge| edge.relationship_name == "imports")
        .map(|edge| (edge.source_id, edge.target_id))
        .collect();
    assert_eq!(skipped, 1);
    assert_eq!(
        imports,
        HashSet::from([(
            id("r", "module", "internal/server"),
            id("r", "module", "internal/config")
        )])
    );
}

#[test]
fn build_code_graph_edges_normalizes_cross_repo_go_call_targets() {
    let facts = facts(json!([
        {"kind": "module", "name": ".", "repo": "go-auth", "props": {"modulePath": "github.com/dejo1307/go-auth"}},
        {"kind": "symbol", "name": "adapters.AuthHandler.Login", "repo": "go-auth", "props": {"symbol_kind": "method"}},
        {"kind": "symbol", "name": "handlers.Login", "repo": "golf", "props": {"symbol_kind": "function"},
         "relations": [{"type": "calls", "target": "github.com/dejo1307/go-auth/adapters.AuthHandler.Login"}]},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    let calls: Vec<_> = edges
        .iter()
        .filter(|edge| edge.relationship_name == "calls")
        .collect();
    assert_eq!(skipped, 0);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].source_id, id("golf", "symbol", "handlers.Login"));
    assert_eq!(
        calls[0].target_id,
        id("go-auth", "symbol", "adapters.AuthHandler.Login")
    );
}

fn client_route(repo: &str, name: &str, props: serde_json::Value) -> serde_json::Value {
    let mut all = json!({"role": "client"});
    all.as_object_mut()
        .unwrap()
        .extend(props.as_object().unwrap().clone());
    json!({"kind": "route", "name": name, "repo": repo, "props": all})
}

#[test]
fn build_code_graph_edges_links_a_call_to_its_caller_and_the_route_it_reaches() {
    let caller_id = "a".repeat(32);
    let server_id = "b".repeat(32);
    let facts = facts(json!([
        {"kind": "symbol", "name": "api.fetchRounds", "repo": "ui", "id": caller_id},
        {"kind": "route", "name": "/api/rounds", "repo": "golf", "id": server_id},
        client_route("ui", "/api/rounds", json!({
            "caller": "api.fetchRounds",
            "caller_id": caller_id,
            "matched_routes": [{
                "repo": "golf", "name": "/api/rounds", "file": "golf/handlers/rounds.go",
                "method": "GET", "confidence": "verified", "id": server_id,
            }],
        })),
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    let client = id("ui", "route", "/api/rounds");
    assert_eq!(skipped, 0);
    let makes = edges
        .iter()
        .find(|edge| edge.relationship_name == "makes_request")
        .unwrap();
    assert_eq!(
        (makes.source_id, makes.target_id),
        (id("ui", "symbol", "api.fetchRounds"), client)
    );
    let reaches = edges
        .iter()
        .find(|edge| edge.relationship_name == "reaches_route")
        .unwrap();
    assert_eq!(
        (reaches.source_id, reaches.target_id),
        (client, id("golf", "route", "/api/rounds"))
    );
    assert_eq!(reaches.properties["confidence"], "verified");
}

#[test]
fn build_code_graph_edges_resolves_caller_and_match_by_name_without_ids() {
    let facts = facts(json!([
        {"kind": "symbol", "name": "fetchRounds", "repo": "ui"},
        {"kind": "symbol", "name": "fetchRounds", "repo": "other"},
        {"kind": "route", "name": "/api/rounds/:id", "repo": "golf"},
        client_route("ui", "/api/rounds/7", json!({
            "caller": "fetchRounds",
            "matched_routes": [{"repo": "golf", "name": "/api/rounds/:id", "confidence": "probable"}],
        })),
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    let client = id("ui", "route", "/api/rounds/7");
    let found = triples(&edges);
    assert_eq!(skipped, 0);
    assert!(found.contains(&(
        id("ui", "symbol", "fetchRounds"),
        "makes_request".into(),
        client
    )));
    assert!(found.contains(&(
        client,
        "reaches_route".into(),
        id("golf", "route", "/api/rounds/:id")
    )));
}

#[test]
fn build_code_graph_edges_skips_self_served_match_and_counts_unresolved_refs() {
    let facts = facts(json!([
        {"kind": "route", "name": "/health", "repo": "golf"},
        client_route("golf", "/health", json!({
            "matched_routes": [{"repo": "golf", "name": "/health", "confidence": "verified"}],
        })),
        client_route("ui", "/api/gone", json!({
            "caller": "missing",
            "matched_routes": [{"repo": "golf", "name": "/api/gone"}, "not-a-match"],
        })),
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert!(edges.is_empty());
    assert_eq!(skipped, 3);
}

#[test]
fn build_code_graph_edges_keeps_handled_by_from_route_to_handler() {
    let handler_id = "c".repeat(32);
    let facts = facts(json!([
        {"kind": "symbol", "name": "handlers.ListRounds", "repo": "golf", "id": handler_id},
        {"kind": "route", "name": "/api/rounds", "repo": "golf", "relations": [
            {"kind": "handled_by", "target": "handlers.ListRounds", "target_id": handler_id},
        ]},
    ]));
    let (edges, skipped) = build_code_graph_edges(&facts, None);
    assert_eq!(skipped, 0);
    let found: Vec<_> = edges
        .iter()
        .map(|edge| {
            (
                edge.source_id,
                edge.target_id,
                edge.relationship_name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![(
            id("golf", "route", "/api/rounds"),
            id("golf", "symbol", "handlers.ListRounds"),
            "handled_by"
        )]
    );
}

#[test]
fn multi_repo_snapshot_creates_one_repository_per_repo() {
    let facts = facts(json!([
        {"kind": "module", "name": "checkout", "repo": "acme/shop"},
        {"kind": "module", "name": "invoices", "repo": "acme/billing"},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/tmp/shop"), None, None);
    let shop = by_name(&nodes, "acme/shop");
    let billing = by_name(&nodes, "acme/billing");
    assert_eq!(shop.node_type(), Some(CodeNodeType::CodeRepository));
    assert_eq!(billing.node_type(), Some(CodeNodeType::CodeRepository));
    assert_eq!(entity(by_name(&nodes, "checkout")).part_of, Some(shop.id()));
    assert_eq!(
        entity(by_name(&nodes, "invoices")).part_of,
        Some(billing.id())
    );
    assert_eq!(
        billing.id(),
        id("acme/billing", "repository", "acme/billing")
    );
    // Only the primary repository gets the directory as its path.
    assert_eq!(shop.as_repository().unwrap().path, "/tmp/shop");
    assert_eq!(billing.as_repository().unwrap().path, "acme/billing");
}

#[test]
fn repo_scope_maps_one_member_of_a_multi_repo_snapshot() {
    let facts = facts(json!([
        {"kind": "module", "name": "checkout", "repo": "acme/shop"},
        {"kind": "module", "name": "invoices", "repo": "acme/billing"},
    ]));
    let nodes =
        map_facts_to_data_points(&facts, Some("/repos/billing"), Some("acme/billing"), None);
    let names: Vec<&str> = nodes.iter().map(|node| node.name()).collect();
    assert_eq!(names, vec!["acme/billing", "invoices"]);
    assert_eq!(nodes[0].as_repository().unwrap().path, "/repos/billing");
}

#[test]
fn empty_repo_scope_falls_back_to_the_snapshot_repository() {
    // Python: `primary_repo = repo_scope or fallback_repo`, while the fact
    // filter still compares against the empty scope.
    let facts = facts(json!([
        {"kind": "module", "name": "checkout", "repo": "acme/shop"},
    ]));
    let nodes = map_facts_to_data_points(&facts, Some("/repos/shop"), Some(""), None);
    let names: Vec<&str> = nodes.iter().map(|node| node.name()).collect();
    assert_eq!(names, vec!["acme/shop"]);
    assert_eq!(nodes[0].as_repository().unwrap().path, "/repos/shop");
}

#[test]
fn a_doc_of_python_whitespace_only_is_no_doc() {
    // Python's str.strip() also strips the ASCII separators \x1c..\x1f,
    // which Rust's char::is_whitespace does not.
    let facts = facts(json!([
        {"kind": "symbol", "name": "f", "repo": "r", "props": {"doc": " \u{1c}\u{1f} "}},
    ]));
    let nodes = map_facts_to_data_points(&facts, None, None, None);
    let symbol = entity(by_name(&nodes, "f"));
    assert_eq!(symbol.doc, None);
    assert_eq!(
        symbol.description.as_deref(),
        Some("symbol: doc= \u{1c}\u{1f} ")
    );
}
