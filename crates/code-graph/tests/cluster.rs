//! A dataset's repositories as one enola cluster (SDK-1053 O8): the cluster
//! run, member labels, and each member's load from the one snapshot.
//!
//! Ports of Python's `test_cluster.py` and the cluster cases of
//! `test_enola_runner.py`. The fake enola (`tests/fixtures/bin/enola`) reads
//! the piped cluster config and copies the `cluster` snapshot (Python's
//! `CLUSTER_FACTS`: a frontend `ui` whose route reaches a backend `golf`)
//! into every member.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cognee_code_graph::{
    CodeGraphConfig, CodeGraphError, ParsedSnapshot, cluster_output_dir, fact_node_id,
    load_cluster_member, render_cluster_config, run_enola_cluster, snapshot_dataset_code_repos,
};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use serde_json::Value;
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

fn dataset_id() -> Uuid {
    Uuid::parse_str("7d4e1c2a-0000-4000-8000-000000000000").unwrap()
}

/// `golf` and `ui` under one root; the first member's `.fake-enola` says
/// what the cluster run does.
fn members(mode: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let golf = root.path().join("golf");
    let ui = root.path().join("ui");
    std::fs::create_dir(&golf).unwrap();
    std::fs::create_dir(&ui).unwrap();
    std::fs::write(golf.join(".fake-enola"), mode).unwrap();
    let golf = golf.canonicalize().unwrap();
    let ui = ui.canonicalize().unwrap();
    (root, golf, ui)
}

fn id(repo: &str, kind: &str, name: &str) -> String {
    fact_node_id(repo, kind, name).to_string()
}

fn reaches() -> (String, String, String) {
    (
        id("ui", "route", "/api/rounds"),
        id("golf", "route", "/api/rounds"),
        "reaches_route".to_string(),
    )
}

async fn edge_keys(graph: &MockGraphDB) -> BTreeSet<(String, String, String)> {
    let (_, edges) = graph.get_graph_data().await.unwrap();
    edges
        .into_iter()
        .map(|(source, target, relationship, _)| (source, target, relationship))
        .collect()
}

async fn node(graph: &MockGraphDB, id: &str) -> Option<cognee_graph::NodeData> {
    graph.get_node(id).await.unwrap()
}

#[test]
fn the_cluster_config_quotes_absolute_paths_in_order_and_disables_history() {
    let golf = PathBuf::from("/tmp/x/golf");
    let odd = PathBuf::from("/tmp/x/re: p\u{e9}o");
    let output_dir = cluster_output_dir(dataset_id());
    assert_eq!(
        render_cluster_config(&[golf, odd], &output_dir),
        "output:\n  dir: \".enola-7d4e1c2a-0000-4000-8000-000000000000\"\nrepos:\n  - \
         \"/tmp/x/golf\"\n  - \"/tmp/x/re: p\\u00e9o\"\nhistory:\n  enabled: false\n"
    );
}

#[tokio::test]
async fn run_enola_cluster_pipes_the_config_from_a_neutral_directory() {
    let (_root, golf, ui) = members("cluster:cluster");
    let output_dir = cluster_output_dir(dataset_id());

    let snapshot_dir = run_enola_cluster(
        &config(),
        &[golf.clone(), ui.clone()],
        &output_dir,
        std::time::Duration::from_secs(30),
    )
    .await
    .unwrap();

    assert_eq!(snapshot_dir, golf.join(&output_dir));
    // enola read exactly the rendered config, on its standard input.
    let seen = std::fs::read_to_string(snapshot_dir.join("cluster-config.yaml")).unwrap();
    assert_eq!(
        seen,
        render_cluster_config(&[golf.clone(), ui.clone()], &output_dir)
    );
    // The overrides, and a working directory that is no member's.
    let run = std::fs::read_to_string(snapshot_dir.join("fake-run.txt")).unwrap();
    let run: Vec<&str> = run.lines().collect();
    assert_eq!(&run[..2], &["1", "1"]);
    let cwd = Path::new(run[2]);
    assert!(cwd != golf && cwd != ui, "cwd {}", cwd.display());
    // Every member got the snapshot; nothing was written next to them.
    assert!(ui.join(&output_dir).join("facts.jsonl").is_file());
    assert!(!golf.parent().unwrap().join("cluster.yaml").exists());
}

#[tokio::test]
async fn run_enola_cluster_rejects_a_missing_repository_before_running() {
    let (root, golf, _ui) = members("cluster:cluster");
    let error = run_enola_cluster(
        &config(),
        &[golf.clone(), root.path().join("gone")],
        &cluster_output_dir(dataset_id()),
        std::time::Duration::from_secs(30),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, CodeGraphError::EnolaSnapshot(_)), "{error}");
    assert!(!golf.join(cluster_output_dir(dataset_id())).exists());
}

#[tokio::test]
async fn a_failed_cluster_run_is_an_enola_error() {
    let (_root, golf, ui) = members("fail");
    let error = snapshot_dataset_code_repos(&config(), dataset_id(), vec![(1, golf), (2, ui)])
        .await
        .unwrap_err();
    assert_eq!(error.name(), "EnolaSnapshotError");
    assert!(error.to_string().contains("exited with code 3"), "{error}");
    assert!(
        error
            .to_string()
            .contains("the 2-repository cluster ['golf', 'ui']"),
        "{error}"
    );
}

#[tokio::test]
async fn the_cluster_snapshot_runs_enola_once_and_labels_each_member() {
    let (_root, golf, ui) = members("cluster:cluster");
    // enola's own record of the label wins over the directory name.
    std::fs::write(golf.join(".fake-enola-label"), "golf").unwrap();

    let snapshot = snapshot_dataset_code_repos(
        &config(),
        dataset_id(),
        vec![("g", golf.clone()), ("u", ui.clone())],
    )
    .await
    .unwrap()
    .expect("two repositories are a cluster");

    let output_dir = cluster_output_dir(dataset_id());
    assert_eq!(snapshot.snapshot_dir, golf.join(&output_dir));
    let members: Vec<_> = snapshot
        .members
        .iter()
        .map(|member| (member.data, member.repo_path.clone(), member.label.as_str()))
        .collect();
    assert_eq!(members, vec![("g", golf, "golf"), ("u", ui, "ui")]);
    // Parsed once, with the edges resolved once, for every member's load.
    assert_eq!(snapshot.parsed.facts.len(), 4);
    let edges = snapshot.parsed.edges.as_ref().expect("edges resolved");
    assert!(edges.iter().any(|edge| {
        (
            edge.source_id.to_string(),
            edge.target_id.to_string(),
            edge.relationship_name.clone(),
        ) == reaches()
    }));
}

#[tokio::test]
async fn members_that_cannot_be_told_apart_are_not_a_cluster() {
    let (_root, golf, ui) = members("cluster:cluster");
    std::fs::write(golf.join(".fake-enola-label"), "web").unwrap();
    std::fs::write(ui.join(".fake-enola-label"), "web").unwrap();
    let snapshot = snapshot_dataset_code_repos(&config(), dataset_id(), vec![(1, golf), (2, ui)])
        .await
        .unwrap();
    assert!(snapshot.is_none());
}

/// The `cluster` snapshot as a member load gets it.
async fn cluster_snapshot() -> (tempfile::TempDir, PathBuf, PathBuf, Arc<ParsedSnapshot>) {
    let (root, golf, ui) = members("cluster:cluster");
    let snapshot = snapshot_dataset_code_repos(
        &config(),
        dataset_id(),
        vec![((), golf.clone()), ((), ui.clone())],
    )
    .await
    .unwrap()
    .unwrap();
    (root, golf, ui, snapshot.parsed)
}

#[tokio::test]
async fn an_edge_into_a_member_not_loaded_yet_waits_for_that_member() {
    let (_root, golf, ui, snapshot) = cluster_snapshot().await;
    let graph = MockGraphDB::new();

    // The frontend loads first: the backend route it reaches is not there.
    let load = load_cluster_member(&graph, None, &snapshot, &ui, "ui", None, None)
        .await
        .unwrap();
    assert!(!load.skipped);
    let written = edge_keys(&graph).await;
    assert!(!written.contains(&reaches()));
    let typed: BTreeSet<&str> = written
        .iter()
        .map(|(_, _, relationship)| relationship.as_str())
        .filter(|relationship| *relationship != "part_of")
        .collect();
    assert_eq!(typed, BTreeSet::from(["makes_request"]));
    // Only the member that loaded is stamped (and its path is its own).
    let ui_repo = node(&graph, &id("ui", "repository", "ui")).await.unwrap();
    assert_eq!(ui_repo["last_snapshot_id"], "sha256:c1u5te4");
    assert_eq!(ui_repo["path"], Value::from(ui.to_string_lossy().as_ref()));
    assert!(
        node(&graph, &id("golf", "repository", "golf"))
            .await
            .is_none()
    );
    assert!(
        node(&graph, &id("golf", "route", "/api/rounds"))
            .await
            .is_none()
    );

    // The backend loads next, with the frontend in the graph: it writes the
    // edge between them along with its own.
    load_cluster_member(&graph, None, &snapshot, &golf, "golf", None, None)
        .await
        .unwrap();
    let written = edge_keys(&graph).await;
    assert!(written.contains(&reaches()));
    let typed: BTreeSet<&str> = written
        .iter()
        .map(|(_, _, relationship)| relationship.as_str())
        .filter(|relationship| *relationship != "part_of")
        .collect();
    assert_eq!(
        typed,
        BTreeSet::from(["makes_request", "reaches_route", "handled_by"])
    );
    let golf_repo = node(&graph, &id("golf", "repository", "golf"))
        .await
        .unwrap();
    assert_eq!(
        golf_repo["path"],
        Value::from(golf.to_string_lossy().as_ref())
    );

    // Loading the frontend again changes nothing: its own stamp matches.
    let again = load_cluster_member(&graph, None, &snapshot, &ui, "ui", None, None)
        .await
        .unwrap();
    assert!(again.skipped);
}

#[tokio::test]
async fn a_member_load_does_not_sweep_the_other_members_nodes_or_edges() {
    let (_root, golf, ui, snapshot) = cluster_snapshot().await;
    let graph = MockGraphDB::new();
    load_cluster_member(&graph, None, &snapshot, &golf, "golf", None, None)
        .await
        .unwrap();
    let handled_by = (
        id("golf", "route", "/api/rounds"),
        id("golf", "symbol", "handlers.ListRounds"),
        "handled_by".to_string(),
    );
    assert!(edge_keys(&graph).await.contains(&handled_by));

    let load = load_cluster_member(&graph, None, &snapshot, &ui, "ui", None, None)
        .await
        .unwrap();

    let delta = load.delta.unwrap();
    assert_eq!(delta["nodes_removed"], 0);
    assert_eq!(delta["edges_removed"], 0);
    let edges = edge_keys(&graph).await;
    assert!(edges.contains(&handled_by));
    assert!(edges.contains(&reaches()));
    assert!(
        node(&graph, &id("golf", "symbol", "handlers.ListRounds"))
            .await
            .is_some()
    );
}

#[tokio::test]
async fn a_member_load_keeps_the_other_members_node_set_tags() {
    let (_root, golf, ui, snapshot) = cluster_snapshot().await;
    let graph = MockGraphDB::new();
    let tags = |name: &str| vec![name.to_string()];
    load_cluster_member(
        &graph,
        None,
        &snapshot,
        &ui,
        "ui",
        Some(&tags("team-ui")),
        None,
    )
    .await
    .unwrap();
    load_cluster_member(
        &graph,
        None,
        &snapshot,
        &golf,
        "golf",
        Some(&tags("old-golf")),
        None,
    )
    .await
    .unwrap();

    // golf is re-tagged: only its own stale tag goes.
    let load = load_cluster_member(
        &graph,
        None,
        &snapshot,
        &golf,
        "golf",
        Some(&tags("team-golf")),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        load.delta.unwrap()["node_set_detagged"],
        serde_json::json!(["old-golf"])
    );

    let tags_of = |node: &cognee_graph::NodeData| -> Vec<String> {
        match node.get("belongs_to_set") {
            Some(Value::Array(tags)) => tags
                .iter()
                .filter_map(|tag| tag.as_str().map(str::to_string))
                .collect(),
            _ => Vec::new(),
        }
    };
    let ui_symbol = node(&graph, &id("ui", "symbol", "api.fetchRounds"))
        .await
        .unwrap();
    assert_eq!(tags_of(&ui_symbol), vec!["team-ui".to_string()]);
    let golf_symbol = node(&graph, &id("golf", "symbol", "handlers.ListRounds"))
        .await
        .unwrap();
    assert_eq!(tags_of(&golf_symbol), vec!["team-golf".to_string()]);

    let set_edges: BTreeSet<(String, String)> = edge_keys(&graph)
        .await
        .into_iter()
        .filter(|(_, _, relationship)| relationship == "belongs_to_set")
        .map(|(source, target, _)| (source, target))
        .collect();
    let ui_symbol_id = id("ui", "symbol", "api.fetchRounds");
    let golf_symbol_id = id("golf", "symbol", "handlers.ListRounds");
    assert!(set_edges.iter().any(|(source, _)| *source == ui_symbol_id));
    let golf_sets: BTreeSet<&String> = set_edges
        .iter()
        .filter(|(source, _)| *source == golf_symbol_id)
        .map(|(_, target)| target)
        .collect();
    assert_eq!(
        golf_sets.len(),
        1,
        "only team-golf is left on golf: {golf_sets:?}"
    );
}
