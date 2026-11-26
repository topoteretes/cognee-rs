//! The CODE operations over a graph the code-graph writer itself loaded, and
//! the writer's invalidation of the process-wide snapshot cache.
//!
//! A shell script stands in for enola (`tests/fixtures/bin/enola`), as in
//! `load.rs`. Everything that reads or writes the process-wide cache is one
//! test, so no other test of this binary races it.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cognee_code_graph::query::{
    CodeRequest, SnapshotCacheKey, code_graph_snapshot_cache, load_code_graph_snapshot,
    run_code_operation,
};
use cognee_code_graph::{CodeGraphConfig, load_code_graph};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use serde_json::{Value, json};
use uuid::Uuid;

fn config() -> CodeGraphConfig {
    CodeGraphConfig {
        enola_path: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bin/enola")
            .to_string_lossy()
            .into_owned(),
        ..CodeGraphConfig::default()
    }
}

fn repo(mode: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("shop");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join(".fake-enola"), mode).unwrap();
    (root, repo)
}

fn run(
    snapshot: &cognee_code_graph::query::CodeGraphSnapshot,
    config: Value,
    query: &str,
) -> Value {
    let request = CodeRequest::parse(config.as_object().unwrap().clone()).unwrap();
    run_code_operation(snapshot, &request, query).unwrap()
}

#[tokio::test]
async fn an_ingested_repository_answers_and_writes_invalidate_its_snapshot() {
    let (_root, repo_path) = repo("snapshot:docs_and_flows");
    let mock = MockGraphDB::new();
    load_code_graph(&mock, None, &config(), &repo_path, None, None)
        .await
        .unwrap();
    let graph: Arc<dyn GraphDBTrait> = Arc::new(mock.clone());

    let snapshot = load_code_graph_snapshot(graph.as_ref()).await.unwrap();

    // delta reads the stamp the load left on the repository node.
    let delta = run(&snapshot, json!({"operation": "delta"}), "");
    let repositories = delta["repositories"].as_array().unwrap();
    assert_eq!(repositories.len(), 1);
    assert_eq!(repositories[0]["repo"], json!("shop"));
    assert_eq!(
        repositories[0]["last_snapshot_id"],
        json!("sha256:5e1f0c0ffee0d0c5")
    );
    assert_eq!(
        repositories[0]["delta"]["snapshot_id"],
        json!("sha256:5e1f0c0ffee0d0c5")
    );
    assert!(repositories[0]["delta"]["nodes_added"].as_u64().unwrap() > 0);
    assert_eq!(repositories[0]["receipt"]["enola_version"], json!("dev"));

    // capabilities sees the docs and flows the load wrote.
    let capabilities = run(&snapshot, json!({"operation": "capabilities"}), "");
    assert_eq!(
        capabilities["features"],
        json!({"docs": true, "flows": true})
    );
    assert_eq!(capabilities["enola_versions"], json!(["dev"]));
    assert_eq!(capabilities["counts"]["repositories"], json!(1));

    // query_facts and neighbors over the written symbols and edges.
    let facts = run(
        &snapshot,
        json!({"operation": "query_facts", "kind": "route"}),
        "",
    );
    assert_eq!(facts["total"], json!(1));
    assert_eq!(facts["facts"][0]["name"], json!("/checkout"));
    assert_eq!(facts["facts"][0]["properties"]["method"], json!("POST"));
    let neighbors = run(
        &snapshot,
        json!({"operation": "neighbors", "name": "app.main", "relation": "calls"}),
        "",
    );
    assert_eq!(neighbors["anchor"]["name"], json!("app.main"));
    assert_eq!(
        neighbors["anchor"]["doc"],
        json!("Start the shop's HTTP server and register its routes.")
    );
    assert_eq!(
        neighbors["neighbors"][0]["node"]["name"],
        json!("app.serve")
    );

    // The process-wide cache serves the snapshot until a write of the
    // dataset, which the writer invalidates.
    let cache = code_graph_snapshot_cache();
    let dataset = Uuid::new_v4();
    let loads = AtomicUsize::new(0);
    let loader = || async {
        loads.fetch_add(1, Ordering::SeqCst);
        load_code_graph_snapshot(graph.as_ref()).await
    };
    cache
        .get_or_load(Some(dataset), &graph, loader)
        .await
        .unwrap();
    cache
        .get_or_load(Some(dataset), &graph, loader)
        .await
        .unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    let key = SnapshotCacheKey::new(Some(dataset), &graph);
    assert!(cache.contains(&key));

    // A reload outside every dataset scope and without a ledger cannot name
    // its dataset: every snapshot is invalidated.
    let (_root2, repo_path2) = repo("snapshot:basic");
    load_code_graph(&mock, None, &config(), &repo_path2, None, None)
        .await
        .unwrap();
    assert!(!cache.contains(&key));
    let reloaded = cache
        .get_or_load(Some(dataset), &graph, loader)
        .await
        .unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 2);
    assert!(reloaded.nodes.len() >= snapshot.nodes.len());

    // Inside a dataset scope the writer invalidates that dataset only (and
    // the unscoped snapshots).
    let other = Uuid::new_v4();
    cache
        .get_or_load(Some(other), &graph, loader)
        .await
        .unwrap();
    assert!(cache.contains(&SnapshotCacheKey::new(Some(other), &graph)));
    let (_root3, repo_path3) = repo("snapshot:all_kinds");
    cognee_models::dataset_scope::with_dataset(dataset, async {
        load_code_graph(&mock, None, &config(), &repo_path3, None, None)
            .await
            .unwrap();
    })
    .await;
    assert!(!cache.contains(&key));
    assert!(cache.contains(&SnapshotCacheKey::new(Some(other), &graph)));
}
