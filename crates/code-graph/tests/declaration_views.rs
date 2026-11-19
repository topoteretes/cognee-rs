//! Port of Python's `tests/unit/tasks/code_graph/test_declaration_views.py`:
//! relations into a name a framework extractor gave both a symbol and a view
//! of it (an ORM model's storage, a relation method's association).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

mod common;

use std::collections::HashSet;

use cognee_code_graph::{build_code_graph_edges, map_facts_to_data_points};
use common::{facts, id, triples};
use serde_json::{Value, json};
use uuid::Uuid;

const REPO: &str = "lv";
const MODEL: &str = "App\\Models\\User";
const MODEL_FILE: &str = "app/Models/User.php";
const RELATION: &str = "App\\Models\\User::posts";
const SHOW: &str = "App\\Http\\Controllers\\UserController::show";
const STORAGE_ENOLA_ID: &str = "4f6ccd8133ea81d75d0e70d041ae25e4";

fn model_facts(repo: &str, storage_file: Option<&str>, symbol_file: Option<&str>) -> Vec<Value> {
    vec![
        json!({"kind": "symbol", "name": MODEL, "file": symbol_file, "line": 8, "repo": repo,
               "props": {"symbol_kind": "class", "eloquent_table": "users"}}),
        json!({"kind": "storage", "name": MODEL, "file": storage_file, "line": 8, "repo": repo,
               "props": {"framework": "eloquent", "storage_kind": "model", "table": "users"},
               "id": STORAGE_ENOLA_ID}),
        json!({"kind": "symbol", "name": RELATION, "file": MODEL_FILE, "line": 14, "repo": repo,
               "props": {"symbol_kind": "method", "eloquent_relation": "hasMany"}}),
        json!({"kind": "association", "name": RELATION, "file": MODEL_FILE, "line": 14, "repo": repo,
               "props": {"framework": "eloquent", "model": MODEL, "target": "App\\Models\\Post"}}),
    ]
}

fn default_model_facts() -> Vec<Value> {
    model_facts(REPO, Some(MODEL_FILE), Some(MODEL_FILE))
}

fn caller(relations: &[(&str, &str)], name: &str, repo: &str) -> Value {
    let relations: Vec<Value> = relations
        .iter()
        .map(|(kind, target)| json!({"kind": kind, "target": target}))
        .collect();
    json!({"kind": "symbol", "name": name, "file": "app/Http/Controllers/UserController.php",
           "repo": repo, "props": {"symbol_kind": "method"}, "relations": relations})
}

fn all(parts: Vec<Value>) -> Vec<cognee_code_graph::Fact> {
    facts(Value::Array(parts))
}

fn set(entries: &[(Uuid, &str, Uuid)]) -> HashSet<(Uuid, String, Uuid)> {
    entries
        .iter()
        .map(|(source, relation, target)| (*source, relation.to_string(), *target))
        .collect()
}

#[test]
fn a_model_name_resolves_to_its_class_not_the_storage_view() {
    let mut show = caller(&[("instantiates", MODEL), ("names", MODEL)], SHOW, REPO);
    show["relations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"kind": "depends_on", "target": MODEL, "target_id": STORAGE_ENOLA_ID}));
    let mut parts = default_model_facts();
    parts.push(show);
    let facts = all(parts);

    let (edges, skipped) = build_code_graph_edges(&facts, None);
    let show = id(REPO, "symbol", SHOW);
    let model = id(REPO, "symbol", MODEL);
    let storage = id(REPO, "storage", MODEL);
    assert_eq!(skipped, 0);
    assert_eq!(
        triples(&edges),
        set(&[
            (show, "instantiates", model),
            (show, "names", model),
            (show, "depends_on", storage)
        ])
    );
    let node_ids: HashSet<Uuid> = map_facts_to_data_points(&facts, None, None, None)
        .iter()
        .map(|node| node.id())
        .collect();
    assert!(node_ids.is_superset(&HashSet::from([model, storage, show])));
}

#[test]
fn a_relation_method_name_resolves_to_the_method_not_the_association_view() {
    let mut parts = default_model_facts();
    parts.push(caller(&[("calls", RELATION)], SHOW, REPO));
    let (edges, skipped) = build_code_graph_edges(&all(parts), None);
    assert_eq!(skipped, 0);
    assert_eq!(
        triples(&edges),
        set(&[(
            id(REPO, "symbol", SHOW),
            "calls",
            id(REPO, "symbol", RELATION)
        )])
    );
}

#[test]
fn a_same_named_fact_from_another_file_stays_a_rival() {
    for parts in [
        model_facts(
            REPO,
            Some("database/migrations/2024_01_01_create_users_table.php"),
            Some(MODEL_FILE),
        ),
        // Without a file on the symbol nothing ties the two together either.
        model_facts(REPO, None, None),
        // Nor does a view that has no file of its own.
        model_facts(REPO, None, Some(MODEL_FILE)),
    ] {
        let mut parts = parts;
        parts.push(caller(&[("instantiates", MODEL)], SHOW, REPO));
        let (edges, skipped) = build_code_graph_edges(&all(parts), None);
        assert!(edges.is_empty());
        assert_eq!(skipped, 1);
    }
}

#[test]
fn two_same_named_symbols_stay_ambiguous_with_a_view_beside_them() {
    let mut parts = model_facts("a", Some(MODEL_FILE), Some(MODEL_FILE));
    parts.push(json!({"kind": "symbol", "name": MODEL, "file": MODEL_FILE, "repo": "b"}));
    parts.push(caller(&[("instantiates", MODEL)], SHOW, "c"));
    parts.push(caller(
        &[("instantiates", MODEL)],
        "App\\Jobs\\Sync::handle",
        "a",
    ));
    let (edges, skipped) = build_code_graph_edges(&all(parts), None);
    assert_eq!(skipped, 1);
    assert_eq!(
        triples(&edges),
        set(&[(
            id("a", "symbol", "App\\Jobs\\Sync::handle"),
            "instantiates",
            id("a", "symbol", MODEL)
        )])
    );
}

#[test]
fn a_view_in_another_repo_is_not_merged_into_the_symbol() {
    let model = model_facts("a", Some(MODEL_FILE), Some(MODEL_FILE));
    let symbol = model[0].clone();
    let mut storage = model[1].clone();
    storage["repo"] = json!("b");
    let parts = vec![
        symbol,
        storage,
        caller(&[("instantiates", MODEL)], SHOW, "c"),
        caller(&[("instantiates", MODEL)], "App\\Jobs\\Sync::handle", "b"),
    ];
    let (edges, skipped) = build_code_graph_edges(&all(parts), None);
    assert_eq!(skipped, 1);
    assert_eq!(
        triples(&edges),
        set(&[(
            id("b", "symbol", "App\\Jobs\\Sync::handle"),
            "instantiates",
            id("b", "storage", MODEL)
        )])
    );
}

#[test]
fn a_jpa_entity_resolves_by_qualified_and_bare_name() {
    let entity = "src/main/java/org/acme/owner.Owner";
    let entity_file = "src/main/java/org/acme/owner/Owner.java";
    let finder_name = "src/main/java/org/acme/owner.OwnerController.findOwner";
    let parts = vec![
        json!({"kind": "symbol", "name": entity, "file": entity_file, "repo": "petclinic",
               "props": {"symbol_kind": "class"}}),
        json!({"kind": "storage", "name": entity, "file": entity_file, "repo": "petclinic",
               "props": {"framework": "jpa", "storage_kind": "entity", "table": "owners"}}),
        caller(
            &[("instantiates", entity), ("calls", "Owner")],
            finder_name,
            "petclinic",
        ),
    ];
    let (edges, skipped) = build_code_graph_edges(&all(parts), None);
    let finder = id("petclinic", "symbol", finder_name);
    let owner = id("petclinic", "symbol", entity);
    assert_eq!(skipped, 0);
    assert_eq!(
        triples(&edges),
        set(&[(finder, "instantiates", owner), (finder, "calls", owner)])
    );
}

#[test]
fn a_bare_name_with_a_rival_symbol_elsewhere_stays_skipped() {
    let entity = "src/main/java/org/acme/owner.Owner";
    let entity_file = "src/main/java/org/acme/owner/Owner.java";
    let finder_name = "src/main/java/org/acme/owner.OwnerController.findOwner";
    let parts = vec![
        json!({"kind": "symbol", "name": entity, "file": entity_file, "repo": "petclinic"}),
        json!({"kind": "storage", "name": entity, "file": entity_file, "repo": "petclinic"}),
        json!({"kind": "symbol", "name": "src/main/java/org/acme/legacy.Owner",
               "file": "src/main/java/org/acme/legacy/Owner.java", "repo": "petclinic"}),
        caller(
            &[("instantiates", entity), ("calls", "Owner")],
            finder_name,
            "petclinic",
        ),
    ];
    let (edges, skipped) = build_code_graph_edges(&all(parts), None);
    assert_eq!(skipped, 1);
    assert_eq!(
        triples(&edges),
        set(&[(
            id("petclinic", "symbol", finder_name),
            "instantiates",
            id("petclinic", "symbol", entity)
        )])
    );
}

#[test]
fn insight_evidence_cites_the_model_class() {
    let insight_name = "God class: App\\Models\\User";
    let mut parts = default_model_facts();
    parts.push(json!({"kind": "insight", "name": insight_name,
                      "props": {"source": "god-class", "confidence": 0.8},
                      "relations": [{"kind": "evidences", "target": MODEL}]}));
    let (edges, skipped) = build_code_graph_edges(&all(parts), None);
    assert_eq!(skipped, 0);
    assert_eq!(
        triples(&edges),
        set(&[(
            id(REPO, "insight", insight_name),
            "evidences",
            id(REPO, "symbol", MODEL)
        )])
    );
}
