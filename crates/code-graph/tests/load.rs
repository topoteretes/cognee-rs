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
use cognee_database::ClaimantScope;
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

    let load = load_code_graph(&graph, None, &config(), &repo_path, node_set, None)
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
    load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()), None)
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

/// The stored repository node of the primary repository.
async fn repository_node(graph: &MockGraphDB, repo_path: &Path) -> NodeData {
    nodes_of_types(graph, &HashSet::from(["CodeRepository"]))
        .await
        .into_iter()
        .map(|(_, node)| node)
        .find(|node| node["path"] == json!(repo_path.to_string_lossy()))
        .unwrap()
}

type EdgeKeySet = BTreeSet<(String, String, String)>;

/// Node ids and edge keys of the whole graph.
async fn graph_shape(graph: &MockGraphDB) -> (BTreeSet<String>, EdgeKeySet) {
    let (nodes, edges) = graph.get_graph_data().await.unwrap();
    (
        nodes.into_iter().map(|(id, _)| id).collect(),
        edges
            .into_iter()
            .map(|(source, target, relationship, _)| (source, target, relationship))
            .collect(),
    )
}

/// Switch the repository's fake enola run to another snapshot fixture.
fn switch_snapshot(repo_path: &Path, snapshot: &str) {
    std::fs::write(
        repo_path.join(".fake-enola"),
        format!("snapshot:{snapshot}"),
    )
    .unwrap();
    std::fs::remove_dir_all(repo_path.join(".enola")).unwrap();
}

/// Python's skip (`test_extract_skips_when_stored_snapshot_id_matches`): an
/// untagged repository whose node already carries this snapshot id and
/// mapping version is not loaded again: no write, no new stamp.
#[tokio::test]
async fn an_unchanged_untagged_reload_is_skipped() {
    let (_root, repo_path) = repo("snapshot:docs_and_flows");
    let graph = MockGraphDB::new();
    let first = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert!(!first.skipped);
    let shape = graph_shape(&graph).await;
    let stamped = repository_node(&graph, &repo_path).await;

    let second = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert!(second.skipped);
    assert_eq!(second.delta, None);
    assert!(second.state.data_points.is_empty());
    assert_eq!(graph_shape(&graph).await, shape);
    let again = repository_node(&graph, &repo_path).await;
    assert_eq!(again["last_delta"], stamped["last_delta"], "not restamped");
    assert_eq!(again["last_snapshot_id"], stamped["last_snapshot_id"]);
}

/// A tagged load is never skipped, but an unchanged one writes no fact node
/// and no edge (the delta write).
#[tokio::test]
async fn an_unchanged_tagged_reload_writes_no_fact_node_and_no_edge() {
    let (_root, repo_path) = repo("snapshot:docs_and_flows");
    let graph = MockGraphDB::new();
    let first = load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()), None)
        .await
        .unwrap();
    let shape = graph_shape(&graph).await;

    let second = load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()), None)
        .await
        .unwrap();
    assert!(!second.skipped);
    let delta = second.delta.unwrap();
    assert_eq!(delta["nodes_added"], json!(0));
    assert_eq!(delta["nodes_updated"], json!(0));
    assert_eq!(
        delta["nodes_unchanged"],
        first.delta.unwrap()["nodes_added"]
    );
    assert_eq!(delta["edges_added"], json!(0));
    assert_eq!(delta["nodes_removed"], json!(0));
    assert_eq!(delta["edges_removed"], json!(0));
    assert_eq!(delta["node_set_detagged"], json!([]));
    assert_eq!(graph_shape(&graph).await, shape);
}

/// A tagged reload restores a node set the graph lost, with its
/// `belongs_to_set` edges, though every code node is unchanged: the node set
/// is written before the edges that point to it (a Postgres graph refuses an
/// edge to a missing node).
#[tokio::test]
async fn a_tagged_reload_restores_a_lost_node_set_and_its_edges() {
    let (_root, repo_path) = repo("snapshot:docs_and_flows");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()), None)
        .await
        .unwrap();
    let shape = graph_shape(&graph).await;
    let (nodes, _) = graph.get_graph_data().await.unwrap();
    let node_set = nodes
        .iter()
        .find(|(_, node)| node["type"] == "NodeSet" && node["name"] == "team-a")
        .map(|(id, _)| id.clone())
        .expect("the team-a node set");
    // Deleting the node set drops every edge into it too.
    graph
        .delete_nodes(std::slice::from_ref(&node_set))
        .await
        .unwrap();

    load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()), None)
        .await
        .unwrap();
    assert_eq!(graph_shape(&graph).await, shape);
}

/// A repository whose node still carries a tag is loaded again even by an
/// untagged call, which then removes the tag.
#[tokio::test]
async fn an_untagged_reload_of_a_tagged_repository_loads_and_detags() {
    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()), None)
        .await
        .unwrap();

    let reload = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert!(!reload.skipped);
    let delta = reload.delta.unwrap();
    assert_eq!(delta["node_set_detagged"], json!(["shared", "team-a"]));

    let (_, edges) = graph_shape(&graph).await;
    assert!(
        edges
            .iter()
            .all(|(_, _, relationship)| relationship != "belongs_to_set"),
        "no code node belongs to a set any more"
    );
    for (_, node) in nodes_of_types(&graph, &code_types()).await {
        let tags = node.get("belongs_to_set").cloned().unwrap_or(Value::Null);
        assert!(
            tags.is_null() || tags == json!([]),
            "{}: {tags}",
            node["name"]
        );
    }
    // The next untagged reload of the now untagged repository is skipped.
    let third = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert!(third.skipped);
}

/// A retag removes the old node sets from every code node, edges and
/// property, and keeps the new one (`_detag_stale_code_graph`).
#[tokio::test]
async fn a_retag_replaces_the_old_node_sets() {
    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()), None)
        .await
        .unwrap();

    let retag = vec!["team-b".to_string(), "shared".to_string()];
    let delta = load_code_graph(&graph, None, &config(), &repo_path, Some(&retag), None)
        .await
        .unwrap()
        .delta
        .unwrap();
    assert_eq!(delta["node_set_detagged"], json!(["team-a"]));

    let sets: Vec<(String, NodeData)> = nodes_of_types(&graph, &HashSet::from(["NodeSet"])).await;
    let set_id = |name: &str| {
        sets.iter()
            .find(|(_, node)| node["name"] == name)
            .map(|(id, _)| id.clone())
            .unwrap()
    };
    let code_nodes = nodes_of_types(&graph, &code_types()).await;
    let (_, edges) = graph_shape(&graph).await;
    let members = |set: &str| {
        let id = set_id(set);
        edges
            .iter()
            .filter(|(_, target, relationship)| relationship == "belongs_to_set" && *target == id)
            .count()
    };
    assert_eq!(members("team-a"), 0);
    assert_eq!(members("team-b"), code_nodes.len());
    assert_eq!(members("shared"), code_nodes.len());
    for (_, node) in &code_nodes {
        let mut tags: Vec<&str> = node["belongs_to_set"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tag| tag.as_str().unwrap())
            .collect();
        tags.sort_unstable();
        assert_eq!(tags, ["shared", "team-b"], "{}", node["name"]);
    }
}

/// A respelled tag (`Team-A` to `team-a`) names the same node set: the
/// membership edges stay, the old spelling is reported as detagged (Python
/// reads it off the pre-write node property), and the nodes carry the new one.
#[tokio::test]
async fn a_respelled_tag_keeps_the_node_set_and_reports_the_old_spelling() {
    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    let old = vec!["Team-A".to_string()];
    load_code_graph(&graph, None, &config(), &repo_path, Some(&old), None)
        .await
        .unwrap();

    let new = vec!["team-a".to_string()];
    let delta = load_code_graph(&graph, None, &config(), &repo_path, Some(&new), None)
        .await
        .unwrap()
        .delta
        .unwrap();
    assert_eq!(delta["node_set_detagged"], json!(["Team-A"]));

    let sets = nodes_of_types(&graph, &HashSet::from(["NodeSet"])).await;
    assert_eq!(sets.len(), 1, "one node set for both spellings");
    let set_id = sets[0].0.clone();
    let code_nodes = nodes_of_types(&graph, &code_types()).await;
    let (_, edges) = graph_shape(&graph).await;
    let members = edges
        .iter()
        .filter(|(_, target, relationship)| relationship == "belongs_to_set" && *target == set_id)
        .count();
    assert_eq!(members, code_nodes.len(), "membership edges survive");
    for (_, node) in &code_nodes {
        assert_eq!(
            node["belongs_to_set"],
            json!(["team-a"]),
            "{}",
            node["name"]
        );
    }
}

/// Python's `test_sweep_removes_stale_nodes_and_edges_and_stamps_snapshot`,
/// end to end: a changed snapshot removes the facts and relations it no
/// longer has, and the graph equals a fresh load of the new snapshot.
#[tokio::test]
async fn a_changed_snapshot_sweeps_what_it_no_longer_derives() {
    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();

    switch_snapshot(&repo_path, "basic_v2");
    let reload = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert!(!reload.skipped);
    let delta = reload.delta.unwrap();
    assert_eq!(delta["nodes_removed"], json!(2));
    assert_eq!(delta["samples_removed"], json!(["Color", "orders_table"]));
    assert_eq!(
        delta["edges_removed"],
        json!(1),
        "shop-api injects PostgresStore"
    );
    assert_eq!(delta["nodes_added"], json!(1));
    assert_eq!(delta["samples_added"], json!(["Save"]));
    assert_eq!(delta["samples_updated"], json!(["Load"]));
    assert_eq!(delta["snapshot_id"], json!("sha256:abc123def789"));
    let repository = repository_node(&graph, &repo_path).await;
    assert_eq!(repository["last_snapshot_id"], json!("sha256:abc123def789"));

    let (_fresh_root, fresh_path) = repo("snapshot:basic_v2");
    let fresh = MockGraphDB::new();
    load_code_graph(&fresh, None, &config(), &fresh_path, None, None)
        .await
        .unwrap();
    assert_eq!(graph_shape(&graph).await, graph_shape(&fresh).await);
}

/// Python's `test_sweep_failure_prevents_snapshot_stamp`: a load that fails
/// in the sweep records no new snapshot id, so the next load is not skipped
/// and finishes the job.
#[tokio::test]
async fn a_failed_sweep_records_no_new_stamp() {
    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();

    switch_snapshot(&repo_path, "basic_v2");
    graph.set_delete_nodes_error("graph down");
    load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap_err();
    // The node write rewrote the repository node without a stamp (as
    // Python's does); what matters is that the new snapshot id is not on it.
    let repository = repository_node(&graph, &repo_path).await;
    assert_ne!(repository["last_snapshot_id"], json!("sha256:abc123def789"));

    graph.clear_delete_nodes_error();
    let retry = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert!(!retry.skipped);
    assert_eq!(retry.delta.unwrap()["nodes_removed"], json!(2));
}

/// A load whose edge write fails records no new snapshot id, so the next
/// load is not skipped and writes the edges: the stamp comes only after the
/// edges (SDK-1702). Python stamps before its edge write
/// (`extract_code_graph.py`), so a failed `add_edges` there is never retried.
#[tokio::test]
async fn a_failed_edge_write_records_no_new_stamp_and_a_retry_writes_the_edges() {
    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();

    switch_snapshot(&repo_path, "basic_v2");
    graph.set_add_edges_error("graph down");
    load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap_err();
    let repository = repository_node(&graph, &repo_path).await;
    assert_ne!(repository["last_snapshot_id"], json!("sha256:abc123def789"));

    graph.clear_add_edges_error();
    let retry = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert!(!retry.skipped, "the failed load was not recorded as done");

    // The retry leaves the graph a fresh load of the new snapshot leaves.
    let (_fresh_root, fresh_path) = repo("snapshot:basic_v2");
    let fresh = MockGraphDB::new();
    load_code_graph(&fresh, None, &config(), &fresh_path, None, None)
        .await
        .unwrap();
    assert_eq!(
        graph_shape(&graph).await.1,
        graph_shape(&fresh).await.1,
        "every edge of the new snapshot is written"
    );
}

/// Another repository in the same graph is outside the sweep.
#[tokio::test]
async fn the_sweep_leaves_other_repositories_alone() {
    let (_other_root, other_path) = repo("snapshot:docs_and_flows");
    let graph = MockGraphDB::new();
    load_code_graph(&graph, None, &config(), &other_path, None, None)
        .await
        .unwrap();
    let other = graph_shape(&graph).await;

    let (_root, repo_path) = repo("snapshot:basic");
    load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    switch_snapshot(&repo_path, "basic_v2");
    load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();

    let (nodes, edges) = graph_shape(&graph).await;
    assert!(other.0.is_subset(&nodes));
    assert!(other.1.is_subset(&edges));
}

/// A skipped load still records the snapshot under its owner, so a second
/// owner (another dataset over the same graph) keeps the graph when the
/// first owner is deleted.
#[tokio::test]
async fn a_skipped_load_still_claims_the_snapshot_for_its_owner() {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let user_id = Uuid::new_v4();
    let owner = |dataset_id, data_id| CodeGraphOwner {
        tenant_id: None,
        user_id,
        dataset_id,
        data_id,
        pipeline_run_id: None,
    };
    let (first, second) = (
        owner(Uuid::new_v4(), Uuid::new_v4()),
        owner(Uuid::new_v4(), Uuid::new_v4()),
    );
    for owner in [first, second] {
        cognee_database::ops::datasets::create_dataset(
            &db,
            cognee_models::Dataset::new("code".into(), user_id, None, owner.dataset_id),
        )
        .await
        .unwrap();
    }

    let (_root, repo_path) = repo("snapshot:basic");
    let graph = MockGraphDB::new();
    load_code_graph(
        &graph,
        Some(&CodeGraphLedger {
            db: &db,
            owner: first,
        }),
        &config(),
        &repo_path,
        None,
        None,
    )
    .await
    .unwrap();
    let skipped = load_code_graph(
        &graph,
        Some(&CodeGraphLedger {
            db: &db,
            owner: second,
        }),
        &config(),
        &repo_path,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(skipped.skipped);

    let (nodes, edges) = graph_shape(&graph).await;
    let claimed: BTreeSet<String> = cognee_database::ops::graph_storage::get_nodes_by_data(
        &db,
        second.data_id,
        second.dataset_id,
    )
    .await
    .unwrap()
    .iter()
    .map(|row| row.slug.to_string())
    .collect();
    assert_eq!(claimed, nodes);
    let claimed: EdgeKeySet = cognee_database::ops::graph_storage::get_edges_by_data(
        &db,
        second.data_id,
        second.dataset_id,
    )
    .await
    .unwrap()
    .iter()
    .map(|row| {
        (
            row.source_node_id.to_string(),
            row.destination_node_id.to_string(),
            row.relationship_name.clone(),
        )
    })
    .collect();
    assert_eq!(claimed, edges);
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
    load_code_graph(
        &graph,
        Some(&ledger),
        &config(),
        &repo_path,
        Some(&tag()),
        None,
    )
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

/// A second owner whose load finds every node already in the graph still
/// records the whole snapshot, so deleting the first owner (what delete
/// removes: the rows only it claims) leaves the graph the second one needs.
#[tokio::test]
async fn an_unchanged_load_claims_the_whole_snapshot_for_its_owner() {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let (user_id, dataset_id) = (Uuid::new_v4(), Uuid::new_v4());
    cognee_database::ops::datasets::create_dataset(
        &db,
        cognee_models::Dataset::new("code".into(), user_id, None, dataset_id),
    )
    .await
    .unwrap();
    let owner = |data_id| CodeGraphOwner {
        tenant_id: None,
        user_id,
        dataset_id,
        data_id,
        pipeline_run_id: None,
    };
    let (first, second) = (owner(Uuid::new_v4()), owner(Uuid::new_v4()));

    let (_root, repo_path) = repo("snapshot:docs_and_flows");
    let graph = MockGraphDB::new();
    load_code_graph(
        &graph,
        Some(&CodeGraphLedger {
            db: &db,
            owner: first,
        }),
        &config(),
        &repo_path,
        Some(&tag()),
        None,
    )
    .await
    .unwrap();
    let reload = load_code_graph(
        &graph,
        Some(&CodeGraphLedger {
            db: &db,
            owner: second,
        }),
        &config(),
        &repo_path,
        Some(&tag()),
        None,
    )
    .await
    .unwrap();
    let delta = reload.delta.unwrap();
    assert_eq!(delta["nodes_added"], json!(0));
    assert_eq!(delta["edges_added"], json!(0));

    let (nodes, edges) = graph.get_graph_data().await.unwrap();
    let second_nodes: BTreeSet<String> =
        cognee_database::ops::graph_storage::get_nodes_by_data(&db, second.data_id, dataset_id)
            .await
            .unwrap()
            .iter()
            .map(|row| row.slug.to_string())
            .collect();
    let stored: BTreeSet<String> = nodes.iter().map(|(id, _)| id.clone()).collect();
    assert_eq!(second_nodes, stored);
    let second_edges =
        cognee_database::ops::graph_storage::get_edges_by_data(&db, second.data_id, dataset_id)
            .await
            .unwrap();
    let second_edges: BTreeSet<(String, String, String)> = second_edges
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
    assert_eq!(second_edges, stored);

    for scope in [ClaimantScope::SameDataset, ClaimantScope::AnyDataset] {
        let only_first = cognee_database::ops::graph_storage::get_unique_nodes_for_data(
            &db,
            first.data_id,
            dataset_id,
            scope,
        )
        .await
        .unwrap();
        assert!(only_first.is_empty(), "{only_first:?}");
        let only_first = cognee_database::ops::graph_storage::get_unique_edges_for_data(
            &db,
            first.data_id,
            dataset_id,
            scope,
        )
        .await
        .unwrap();
        assert!(only_first.is_empty(), "{only_first:?}");
    }
}

/// The incremental load against Ladybug, whose nodes keep their properties
/// in a JSON blob: a retag and a changed snapshot leave the same graph as a
/// fresh load of the new snapshot with the new tag, and an unchanged untagged
/// reload is skipped.
#[tokio::test]
async fn the_incremental_load_on_ladybug_matches_a_fresh_load() {
    async fn ladybug(dir: &Path) -> cognee_graph::LadybugAdapter {
        let adapter = cognee_graph::LadybugAdapter::new(dir.join("graph.db").to_str().unwrap())
            .await
            .unwrap();
        adapter.initialize().await.unwrap();
        adapter
    }
    async fn shape(graph: &dyn GraphDBTrait) -> (BTreeSet<String>, EdgeKeySet) {
        let (nodes, edges) = graph.get_graph_data().await.unwrap();
        (
            nodes.into_iter().map(|(id, _)| id).collect(),
            edges
                .into_iter()
                .map(|(source, target, relationship, _)| (source, target, relationship))
                .collect(),
        )
    }

    let store = tempfile::tempdir().unwrap();
    let graph = ladybug(store.path()).await;
    let (_root, repo_path) = repo("snapshot:basic");
    load_code_graph(&graph, None, &config(), &repo_path, Some(&tag()), None)
        .await
        .unwrap();
    switch_snapshot(&repo_path, "basic_v2");
    let retag = vec!["team-b".to_string()];
    let delta = load_code_graph(&graph, None, &config(), &repo_path, Some(&retag), None)
        .await
        .unwrap()
        .delta
        .unwrap();
    assert_eq!(delta["nodes_removed"], json!(2));
    assert_eq!(delta["node_set_detagged"], json!(["shared", "team-a"]));

    let fresh_store = tempfile::tempdir().unwrap();
    let fresh = ladybug(fresh_store.path()).await;
    let (_fresh_root, fresh_path) = repo("snapshot:basic_v2");
    load_code_graph(&fresh, None, &config(), &fresh_path, Some(&retag), None)
        .await
        .unwrap();
    let (nodes, edges) = shape(&graph).await;
    let (fresh_nodes, fresh_edges) = shape(&fresh).await;
    // The old node sets stay as nodes (Python never deletes them); nothing
    // links to them any more.
    let old_sets: BTreeSet<String> = nodes.difference(&fresh_nodes).cloned().collect();
    assert_eq!(old_sets.len(), 2, "{old_sets:?}");
    assert_eq!(edges, fresh_edges);
    for (_, node) in graph.get_graph_data().await.unwrap().0 {
        if node["type"] == "NodeSet" {
            continue;
        }
        let tags = node.get("belongs_to_set").cloned().unwrap_or(Value::Null);
        assert_eq!(tags, json!(["team-b"]), "{}", node["name"]);
    }

    // Untagging, then an unchanged untagged reload, which is skipped.
    let untag = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert_eq!(untag.delta.unwrap()["node_set_detagged"], json!(["team-b"]));
    let skipped = load_code_graph(&graph, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    assert!(skipped.skipped);
}

/// Two loads of one repository at once take turns: the fake enola fails a
/// run that overlaps another on the same directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_loads_of_one_repository_serialise() {
    let (_root, repo_path) = repo("slow:basic");
    let graph = MockGraphDB::new();
    let config = config();
    let (first, second) = tokio::join!(
        load_code_graph(&graph, None, &config, &repo_path, None, None),
        load_code_graph(&graph, None, &config, &repo_path, None, None),
    );
    first.unwrap();
    second.unwrap();
}

/// A relative `enola_path` names a file under the process's working
/// directory (cargo runs tests from the package root), although enola runs
/// with the repository as its working directory.
#[tokio::test]
async fn a_relative_enola_path_is_resolved_against_the_process_cwd() {
    let relative = Path::new("tests/fixtures/bin/enola");
    assert!(relative.is_file(), "run from the package root");
    let config = CodeGraphConfig {
        enola_path: relative.to_string_lossy().into_owned(),
        ..CodeGraphConfig::default()
    };
    let (_root, repo_path) = repo("snapshot:basic");
    let load = load_code_graph(&MockGraphDB::new(), None, &config, &repo_path, None, None)
        .await
        .unwrap();
    assert!(load.delta.is_some());
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
    let error = load_code_graph(&MockGraphDB::new(), None, &config, &repo_path, None, None)
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
    // Killed and reaped before the error returns: not even a zombie is left
    // (`kill -0` succeeds on a zombie), so enola writes nothing afterwards.
    let pid = std::fs::read_to_string(repo_path.join(".fake-enola-pid")).unwrap();
    let alive = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "enola ({}) is still running", pid.trim());
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
    load_code_graph(&graph, None, &config, &repo_path, None, None)
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
