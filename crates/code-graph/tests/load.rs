//! The load end to end: run enola, map, write nodes and edges, stamp.
//!
//! A shell script stands in for enola (`tests/fixtures/bin/enola`); it copies
//! one of the snapshot fixtures into the repository's `.enola/`, so the graph
//! the load writes can be compared with the goldens Python produced for the
//! same snapshot.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use cognee_code_graph::{
    CODE_GRAPH_MAPPING_VERSION, CodeGraphConfig, CodeGraphError, CodeGraphLedger, CodeGraphOwner,
    CodeNodeType, load_code_graph, run_enola_generate,
};
use cognee_graph::{GraphDBTrait, MockGraphDB, NodeData};
use serde_json::{Map, Value, json};
use uuid::Uuid;

fn fake_enola() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bin/enola")
}

fn config() -> CodeGraphConfig {
    CodeGraphConfig {
        enola_path: fake_enola().to_string_lossy().into_owned(),
        ..CodeGraphConfig::default()
    }
}

/// A repository directory whose fake enola run does `mode`.
fn repo(mode: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("shop");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join(".fake-enola"), mode).unwrap();
    (root, repo)
}

fn golden(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/goldens")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn code_types() -> HashSet<&'static str> {
    CodeNodeType::ALL
        .iter()
        .map(|node_type| node_type.class_name())
        .collect()
}

/// A stored node's properties minus the run-dependent and null ones (the
/// golden script's `node_properties`), and minus the repository stamp.
fn logical(node: &NodeData) -> Map<String, Value> {
    node.iter()
        .filter(|(key, value)| {
            !value.is_null()
                && !matches!(key.as_ref(), "created_at" | "updated_at")
                && !key.starts_with("last_")
        })
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect()
}

async fn nodes_of_types(graph: &MockGraphDB, types: &HashSet<&str>) -> Vec<(String, NodeData)> {
    let (nodes, _) = graph.get_graph_data().await.unwrap();
    let mut nodes: Vec<_> = nodes
        .into_iter()
        .filter(|(_, node)| {
            node.get("type")
                .and_then(Value::as_str)
                .is_some_and(|node_type| types.contains(node_type))
        })
        .collect();
    nodes.sort_by(|left, right| left.0.cmp(&right.0));
    nodes
}

async fn edge_set(graph: &MockGraphDB) -> Vec<(String, String, String, Map<String, Value>)> {
    let (_, edges) = graph.get_graph_data().await.unwrap();
    edges
        .into_iter()
        .map(|(source, target, relationship, properties)| {
            (
                source,
                target,
                relationship,
                properties
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value))
                    .collect(),
            )
        })
        .collect()
}

/// The load against the mock graph matches Python's goldens: every node with
/// its properties, every typed edge, and a `part_of` edge per fact node.
async fn check_against_golden(snapshot: &str, tag: &str, node_set: Option<&[String]>) {
    let expected = golden(&format!("{snapshot}__{tag}"));
    let (_root, repo_path) = repo(&format!("snapshot:{snapshot}"));
    let graph = MockGraphDB::new();

    let load = load_code_graph(&graph, None, &config(), &repo_path, node_set)
        .await
        .unwrap();

    // The run got cognee's environment overrides and the repository as cwd.
    let run = std::fs::read_to_string(repo_path.join(".enola/fake-run.txt")).unwrap();
    let run: Vec<&str> = run.lines().collect();
    assert_eq!(&run[..2], &["1", "1"]);
    assert_eq!(
        Path::new(run[2]),
        repo_path.canonicalize().unwrap().as_path()
    );

    // Nodes: the goldens were produced with repo_path "/tmp/shop", so the
    // primary repository's `path` is compared separately.
    let nodes = nodes_of_types(&graph, &code_types()).await;
    let expected_nodes = expected["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), expected_nodes.len(), "node count");
    for ((id, node), wanted) in nodes.iter().zip(expected_nodes) {
        assert_eq!(id, wanted["id"].as_str().unwrap());
        let mut actual = logical(node);
        let mut wanted = wanted["properties"].as_object().unwrap().clone();
        if wanted["type"] == "CodeRepository" && wanted["path"] == "/tmp/shop" {
            assert_eq!(actual["path"], json!(repo_path.to_string_lossy()));
            actual.remove("path");
            wanted.remove("path");
        }
        assert_eq!(actual, wanted, "node {}", wanted["name"]);
    }

    let edges = edge_set(&graph).await;
    let typed: Vec<Value> = edges
        .iter()
        .filter(|(_, _, relationship, _)| {
            !matches!(relationship.as_str(), "part_of" | "belongs_to_set")
        })
        .map(|(source, target, relationship, properties)| {
            let properties: Map<String, Value> = properties
                .iter()
                .filter(|(key, _)| key.as_str() != "updated_at")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            json!({
                "source": source,
                "target": target,
                "relationship_name": relationship,
                "properties": properties,
            })
        })
        .collect();
    assert_eq!(
        Value::Array(typed),
        expected["edges"],
        "typed edges, in order"
    );

    // One part_of edge per fact node, to its repository.
    let part_of: BTreeSet<&str> = edges
        .iter()
        .filter(|(_, _, relationship, _)| relationship == "part_of")
        .map(|(source, _, _, _)| source.as_str())
        .collect();
    let facts: BTreeSet<&str> = expected_nodes
        .iter()
        .filter(|node| node["type"] != "CodeRepository")
        .map(|node| node["id"].as_str().unwrap())
        .collect();
    assert_eq!(part_of, facts);

    // The stamp: identity, mapping version, receipt projection and delta.
    let repository = nodes
        .iter()
        .find(|(_, node)| {
            node["type"] == "CodeRepository" && node["path"] == json!(repo_path.to_string_lossy())
        })
        .map(|(_, node)| node)
        .unwrap();
    let delta = load.delta.unwrap();
    assert_eq!(repository["last_snapshot_id"], delta["snapshot_id"]);
    assert!(
        delta["snapshot_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(
        repository["last_mapping_version"],
        json!(CODE_GRAPH_MAPPING_VERSION)
    );
    assert_eq!(repository["last_receipt"]["format_version"], json!(1));
    assert_eq!(repository["last_delta"], Value::Object(delta.clone()));
    assert_eq!(delta["nodes_added"], json!(facts.len()));
    assert_eq!(delta["nodes_updated"], json!(0));
    assert_eq!(
        delta["edges_added"],
        json!(expected["edges"].as_array().unwrap().len())
    );
}

fn tag() -> Vec<String> {
    vec!["team-a".to_string(), "shared".to_string()]
}

#[tokio::test]
async fn basic_snapshot_loads_as_python_maps_it() {
    check_against_golden("basic", "untagged", None).await;
}

#[tokio::test]
async fn docs_and_flows_snapshot_loads_as_python_maps_it() {
    check_against_golden("docs_and_flows", "untagged", None).await;
}

#[tokio::test]
async fn a_tagged_load_matches_python_and_links_node_sets() {
    check_against_golden("basic", "tagged", Some(&tag())).await;

    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()))
        .await
        .unwrap();

    let node_sets = nodes_of_types(&graph, &HashSet::from(["NodeSet"])).await;
    let names: BTreeSet<&str> = node_sets
        .iter()
        .map(|(_, node)| node["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, BTreeSet::from(["shared", "team-a"]));

    // Every code node, repositories included, belongs to both sets.
    let code_nodes = nodes_of_types(&graph, &code_types()).await;
    let edges = edge_set(&graph).await;
    for (set_id, set) in &node_sets {
        let members: BTreeSet<&str> = edges
            .iter()
            .filter(|(_, target, relationship, _)| {
                relationship == "belongs_to_set" && target == set_id
            })
            .map(|(source, _, _, _)| source.as_str())
            .collect();
        assert_eq!(
            members.len(),
            code_nodes.len(),
            "members of {}",
            set["name"]
        );
        let (_, _, _, properties) = edges
            .iter()
            .find(|(_, target, relationship, _)| {
                relationship == "belongs_to_set" && target == set_id
            })
            .unwrap();
        let edge_text = properties["edge_text"].as_str().unwrap();
        assert!(
            edge_text.ends_with(&format!(
                " belongs to set {}.",
                set["name"].as_str().unwrap()
            )),
            "{edge_text}"
        );
    }
}

#[tokio::test]
async fn an_unchanged_reload_writes_no_fact_node_and_no_edge() {
    let (_root, repo_path) = repo("snapshot:docs_and_flows");
    let graph = MockGraphDB::new();
    let first = load_code_graph(&graph, None, &config(), &repo_path, None)
        .await
        .unwrap();
    let edges_after_first = graph.edge_count();

    let second = load_code_graph(&graph, None, &config(), &repo_path, None)
        .await
        .unwrap();
    let delta = second.delta.unwrap();
    assert_eq!(delta["nodes_added"], json!(0));
    assert_eq!(delta["nodes_updated"], json!(0));
    assert_eq!(
        delta["nodes_unchanged"],
        first.delta.unwrap()["nodes_added"]
    );
    assert_eq!(delta["edges_added"], json!(0));
    assert_eq!(graph.edge_count(), edges_after_first);
}

#[tokio::test]
async fn the_ledger_records_every_node_and_edge_under_the_data_row() {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let owner = CodeGraphOwner {
        tenant_id: None,
        user_id: Uuid::new_v4(),
        dataset_id: Uuid::new_v4(),
        data_id: Uuid::new_v4(),
        pipeline_run_id: Some(Uuid::new_v4()),
    };
    cognee_database::ops::datasets::create_dataset(
        &db,
        cognee_models::Dataset::new("code".into(), owner.user_id, None, owner.dataset_id),
    )
    .await
    .unwrap();
    let ledger = CodeGraphLedger { db: &db, owner };

    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, Some(&ledger), &config(), &repo_path, Some(&tag()))
        .await
        .unwrap();

    let (nodes, edges) = graph.get_graph_data().await.unwrap();
    let node_rows = cognee_database::ops::graph_storage::get_nodes_by_data(
        &db,
        owner.data_id,
        owner.dataset_id,
    )
    .await
    .unwrap();
    let recorded: BTreeSet<String> = node_rows.iter().map(|row| row.slug.to_string()).collect();
    let stored: BTreeSet<String> = nodes.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(recorded, stored);
    assert!(
        node_rows
            .iter()
            .all(|row| row.pipeline_run_id == owner.pipeline_run_id)
    );
    let node_set_row = node_rows
        .iter()
        .find(|row| row.node_type == "NodeSet")
        .unwrap();
    assert_eq!(node_set_row.indexed_fields, json!([]));
    let symbol_row = node_rows
        .iter()
        .find(|row| row.node_type == "CodeSymbol")
        .unwrap();
    assert_eq!(symbol_row.indexed_fields, json!(["name"]));

    let edge_rows = cognee_database::ops::graph_storage::get_edges_by_data(
        &db,
        owner.data_id,
        owner.dataset_id,
    )
    .await
    .unwrap();
    let recorded: BTreeSet<(String, String, String)> = edge_rows
        .iter()
        .map(|row| {
            (
                row.source_node_id.to_string(),
                row.destination_node_id.to_string(),
                row.relationship_name.clone(),
            )
        })
        .collect();
    let stored: BTreeSet<(String, String, String)> = edges
        .into_iter()
        .map(|(source, target, relationship, _)| (source, target, relationship))
        .collect();
    assert_eq!(recorded, stored);
}

#[tokio::test]
async fn a_missing_configured_binary_is_enola_not_installed() {
    let (_root, repo_path) = repo("snapshot:basic");
    let config = CodeGraphConfig {
        enola_path: repo_path
            .join("no-such-enola")
            .to_string_lossy()
            .into_owned(),
        ..CodeGraphConfig::default()
    };
    let error = load_code_graph(&MockGraphDB::new(), None, &config, &repo_path, None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, CodeGraphError::EnolaNotInstalled(_)),
        "{error}"
    );
    assert_eq!(error.status_code(), 500);
}

#[tokio::test]
async fn a_binary_that_cannot_be_found_is_enola_not_installed() {
    // No configured path: the bare `enola` is resolved on PATH at spawn, and
    // a missing binary reports NotFound, which maps to EnolaNotInstalled.
    let (_root, repo_path) = repo("snapshot:basic");
    let config = CodeGraphConfig {
        enola_path: String::new(),
        ..CodeGraphConfig::default()
    };
    // Only meaningful where no real enola is installed on PATH.
    if which_enola_on_path() {
        return;
    }
    let error = run_enola_generate(&config, &repo_path).await.unwrap_err();
    assert!(
        matches!(error, CodeGraphError::EnolaNotInstalled(_)),
        "{error}"
    );
}

/// Whether `enola` resolves on this machine's PATH, asked of the shell so
/// the test reads no environment itself.
fn which_enola_on_path() -> bool {
    std::process::Command::new("sh")
        .args(["-c", "command -v enola"])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[tokio::test]
async fn a_timeout_kills_enola() {
    let (_root, repo_path) = repo("hang");
    let config = CodeGraphConfig {
        enola_timeout_secs: 0.5,
        ..config()
    };
    let started = std::time::Instant::now();
    let error = run_enola_generate(&config, &repo_path).await.unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(
        error.to_string(),
        format!(
            "enola timed out after 0.5 seconds on '{}'.",
            repo_path.display()
        )
    );
    assert_eq!(error.name(), "EnolaSnapshotError");
}

#[tokio::test]
async fn a_non_zero_exit_carries_the_stderr_tail() {
    let (_root, repo_path) = repo("fail");
    let error = run_enola_generate(&config(), &repo_path).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "enola exited with code 3 on '{}'. stderr tail: boom: extractor crashed\n",
            repo_path.display()
        )
    );
    assert_eq!(error.name(), "EnolaSnapshotError");
}

#[tokio::test]
async fn a_run_without_facts_is_a_snapshot_error() {
    let (_root, repo_path) = repo("empty");
    let error = run_enola_generate(&config(), &repo_path).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "enola completed but no facts.jsonl was found in '{}'.",
            repo_path.join(".enola").display()
        )
    );
}

#[tokio::test]
async fn a_repository_that_is_not_a_directory_is_rejected() {
    let (_root, repo_path) = repo("snapshot:basic");
    let missing = repo_path.join("missing");
    let error = run_enola_generate(&config(), &missing).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "Repository path '{}' is not a directory.",
            missing.display()
        )
    );
}

/// The real enola 0.5.1 on Python's e2e fixture, with Python's known answers
/// (`cognee/tests/test_code_graph_e2e.py`). Run with
/// `ENOLA_PATH=<enola> cargo test -p cognee-code-graph --test load -- --ignored`.
#[tokio::test]
#[ignore = "needs a real enola 0.5.1 binary (ENOLA_PATH or PATH)"]
#[allow(
    clippy::disallowed_methods,
    reason = "test-only: locates the developer's enola binary"
)]
async fn real_enola_on_the_python_e2e_fixture() {
    let enola_path = std::env::var("ENOLA_PATH").unwrap_or_default();
    let config = CodeGraphConfig {
        enola_path,
        ..CodeGraphConfig::default()
    };
    let root = tempfile::tempdir().unwrap();
    let repo_path = root.path().join("code_repo_fixture");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code_repo_fixture"),
        &repo_path,
    );

    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config, &repo_path, None)
        .await
        .unwrap();
    let (nodes, edges) = graph.get_graph_data().await.unwrap();

    let types: HashSet<&str> = nodes
        .iter()
        .filter_map(|(_, node)| node.get("type").and_then(Value::as_str))
        .collect();
    assert!(types.is_subset(&code_types()), "{types:?}");

    let of_type = |class: &str| -> Vec<&NodeData> {
        nodes
            .iter()
            .map(|(_, node)| node)
            .filter(|node| node["type"] == class)
            .collect()
    };
    let repositories = of_type("CodeRepository");
    assert_eq!(repositories.len(), 1);
    let repository = repositories[0];
    assert_eq!(repository["name"], "code_repo_fixture");
    assert!(
        repository["last_snapshot_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(repository["last_receipt"]["format_version"], json!(1));
    assert_eq!(repository["last_receipt"]["enola_version"], json!("0.5.1"));

    let names = |class: &str| -> BTreeSet<String> {
        of_type(class)
            .iter()
            .filter_map(|node| node["name"].as_str().map(str::to_string))
            .collect()
    };
    assert!(names("CodeModule").is_superset(&BTreeSet::from([".".into(), "inventory".into()])));
    let symbols: Vec<(&str, &str)> = of_type("CodeSymbol")
        .iter()
        .filter_map(|node| Some((node["name"].as_str()?, node["symbol_kind"].as_str()?)))
        .collect();
    for (name, kind) in [
        ("main.main", "function"),
        ("inventory/pricing.line_total", "function"),
        ("inventory/pricing.compute_total", "function"),
        ("inventory/pricing.apply_discount", "function"),
        ("inventory/store.InventoryStore", "class"),
        ("inventory/store.InventoryStore.total", "method"),
        ("inventory/store.InventoryStore.discounted_total", "method"),
    ] {
        assert!(
            symbols.contains(&(name, kind)),
            "missing symbol {name} ({kind})"
        );
    }
    let files: BTreeSet<String> = of_type("CodeSymbol")
        .iter()
        .filter_map(|node| node["file_path"].as_str().map(str::to_string))
        .collect();
    for file in ["main.py", "inventory/pricing.py", "inventory/store.py"] {
        assert!(files.contains(file), "missing file {file}");
    }
    let dependencies = names("ExternalDependency");
    for name in [
        "inventory/store -> requests",
        "inventory/pricing -> math",
        "inventory/store -> inventory.pricing",
    ] {
        assert!(dependencies.contains(name), "missing dependency {name}");
    }

    let name_of = |id: &str| -> String {
        nodes
            .iter()
            .find(|(node_id, _)| node_id == id)
            .and_then(|(_, node)| node["name"].as_str().map(str::to_string))
            .unwrap_or_default()
    };
    let calls: BTreeSet<(String, String)> = edges
        .iter()
        .filter(|(_, _, relationship, _)| relationship == "calls")
        .map(|(source, target, _, _)| (name_of(source), name_of(target)))
        .collect();
    for (source, target) in [
        (
            "inventory/pricing.apply_discount",
            "inventory/pricing.compute_total",
        ),
        (
            "inventory/pricing.compute_total",
            "inventory/pricing.line_total",
        ),
        (
            "inventory/store.InventoryStore.total",
            "inventory/pricing.compute_total",
        ),
        (
            "inventory/store.InventoryStore.discounted_total",
            "inventory/pricing.apply_discount",
        ),
        (
            "main.main",
            "inventory/store.InventoryStore.discounted_total",
        ),
    ] {
        assert!(
            calls.contains(&(source.to_string(), target.to_string())),
            "missing call {source} -> {target}"
        );
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
