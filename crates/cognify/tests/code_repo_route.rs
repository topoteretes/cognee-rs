//! `add(<project dir>)` then `cognify()`: the CODE_REPO route (SDK-1053 O4),
//! and `add(<code file>)` then `cognify()`: the per-file CODE route (O9).
//!
//! A code project directory is added as one repo manifest item plus its
//! documents; cognify runs the manifest down the code graph load and the
//! documents down the standard route, in one pipeline run. The LLM panics on
//! any call: the code route never makes one, and the documents' route uses an
//! LLM-free graph backend with summarization off. enola is the shell script
//! `cognee-code-graph` tests use (`tests/fixtures/bin/enola`), which copies a
//! snapshot fixture into the repository's `.enola/`.
//!
//! Offline throughout.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cognee_code_graph::{CodeGraphConfig, CodeNodeType};
use cognee_cognify::graph_backend::MockChunkGraphExtractor;
use cognee_cognify::routing::{CognifyRoute, CognifyRouteError, cognify_route_for};
use cognee_cognify::tasks::run_code_routes;
use cognee_cognify::{
    CognifyConfig, CognifyError, CognifyInput, CognifyResult, FailurePolicy, FailureStage,
    FailureStop, RollbackScope, cognify,
};
use cognee_database::ops::data::get_cognify_completed_data_ids;
use cognee_database::ops::datasets::get_dataset_by_name_in_tenant;
use cognee_database::{
    DatabaseConnection, IngestDb, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_models::{Data, DataInput};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockVectorDB};
use cognee_vector::VectorDB;
use serde_json::Value;
use uuid::Uuid;

/// An LLM that fails the test on any call.
struct PanickingLlm;

#[async_trait::async_trait]
impl cognee_llm::Llm for PanickingLlm {
    async fn generate(
        &self,
        _messages: Vec<cognee_llm::Message>,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
        panic!("the code route must never call the LLM (generate)")
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<cognee_llm::Message>,
        _json_schema: &serde_json::Value,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<serde_json::Value> {
        panic!("the code route must never call the LLM (structured output)")
    }

    fn model(&self) -> &str {
        "panicking"
    }
}

fn fake_enola() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../code-graph/tests/fixtures/bin/enola")
}

/// A Python project like `code_repo_fixture`, whose fake enola run copies the
/// `basic` snapshot. `.fake-enola` is a dotfile, so the manifest skips it.
fn project() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("shop");
    std::fs::create_dir_all(repo.join("inventory")).unwrap();
    std::fs::write(repo.join("pyproject.toml"), "[project]\nname = \"shop\"\n").unwrap();
    std::fs::write(repo.join("main.py"), "def main():\n    pass\n").unwrap();
    std::fs::write(repo.join("inventory/store.py"), "class Store:\n    pass\n").unwrap();
    std::fs::write(
        repo.join("README.md"),
        "Shop keeps an inventory of items.\n",
    )
    .unwrap();
    std::fs::write(repo.join(".fake-enola"), "snapshot:basic").unwrap();
    let repo = repo.canonicalize().unwrap();
    (root, repo)
}

fn config(with_backend: bool) -> CognifyConfig {
    let mut config = CognifyConfig::default()
        .with_chunk_size(1500)
        .with_summarization(false)
        .with_web_page_nodes(false)
        .with_code_graph(CodeGraphConfig {
            enola_path: fake_enola().to_string_lossy().into_owned(),
            ..CodeGraphConfig::default()
        });
    if with_backend {
        config = config.with_graph_backend(Arc::new(MockChunkGraphExtractor::new()));
    }
    config
}

struct Stack {
    db: Arc<DatabaseConnection>,
    repo: Arc<dyn PipelineRunRepository>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<MockGraphDB>,
    vector_db: Arc<MockVectorDB>,
    owner_id: Uuid,
}

impl Stack {
    async fn new() -> Self {
        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        let db = Arc::new(conn);
        Self {
            repo: Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db))),
            db,
            storage: Arc::new(MockStorage::new()),
            graph_db: Arc::new(MockGraphDB::new()),
            vector_db: Arc::new(MockVectorDB::new()),
            owner_id: Uuid::new_v4(),
        }
    }

    fn thread_pool() -> Arc<dyn cognee_core::CpuPool> {
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool"))
    }

    async fn add(&self, path: &Path, llm_available: bool, node_set: Option<&[&str]>) -> Vec<Data> {
        let pipeline = AddPipeline::new(
            Arc::clone(&self.storage),
            Arc::clone(&self.db) as Arc<dyn IngestDb>,
        )
        .with_thread_pool(Self::thread_pool())
        .with_graph_db(Arc::clone(&self.graph_db) as Arc<dyn GraphDBTrait>)
        .with_vector_db(Arc::clone(&self.vector_db) as Arc<dyn VectorDB>)
        .with_database(self.db.clone())
        .with_llm_available(llm_available);
        let params = AddParams {
            node_set: node_set.map(|names| names.iter().map(|name| name.to_string()).collect()),
            ..AddParams::default()
        };
        pipeline
            .add_with_params(
                vec![DataInput::FilePath(path.to_string_lossy().into_owned())],
                "code",
                self.owner_id,
                None,
                &params,
            )
            .await
            .expect("add the project directory")
    }

    async fn dataset_id(&self) -> Uuid {
        get_dataset_by_name_in_tenant(&self.db, "code", self.owner_id, None)
            .await
            .expect("read the dataset")
            .expect("add created the dataset")
            .id
    }

    async fn cognify(
        &self,
        items: Vec<Data>,
        config: &CognifyConfig,
    ) -> Result<CognifyResult, CognifyError> {
        cognify(
            items,
            self.dataset_id().await,
            Some(self.owner_id),
            None,
            None,
            Arc::new(PanickingLlm),
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db) as Arc<dyn GraphDBTrait>,
            Arc::clone(&self.vector_db) as Arc<dyn VectorDB>,
            Arc::new(MockEmbeddingEngine::new(8)),
            self.db.clone(),
            Arc::clone(&self.repo),
            Self::thread_pool(),
            Arc::new(NoOpOntologyResolver::new()),
            config,
        )
        .await
    }

    async fn nodes(&self) -> Vec<(String, cognee_graph::NodeData)> {
        self.graph_db.get_graph_data().await.unwrap().0
    }

    async fn code_node_ids(&self) -> BTreeSet<String> {
        let types: HashSet<&str> = CodeNodeType::ALL
            .iter()
            .map(|node_type| node_type.class_name())
            .collect();
        self.nodes()
            .await
            .into_iter()
            .filter(|(_, node)| {
                node.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|node_type| types.contains(node_type))
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Each cognify run's latest status, most recent run first.
    async fn cognify_runs(&self) -> Vec<PipelineRunStatus> {
        let dataset_id = self.dataset_id().await;
        let mut seen = HashSet::new();
        self.repo
            .list_recent(Some(dataset_id), 100)
            .await
            .expect("list pipeline runs")
            .into_iter()
            .filter(|row| row.pipeline_name == "cognify_pipeline")
            .filter(|row| seen.insert(row.pipeline_run_id))
            .map(|row| row.status)
            .collect()
    }
}

/// The report of a run that failed with `RunFailed`: a `FailedItems` run
/// errors whenever an item failed, as Python's does, and still keeps what it
/// completed (SDK-1694 O5b review).
fn failed_report(
    outcome: Result<CognifyResult, CognifyError>,
    label: &str,
) -> cognee_cognify::FailureReport {
    match outcome {
        Err(CognifyError::RunFailed { report }) => *report,
        other => panic!("{label}: expected RunFailed, got {other:?}"),
    }
}

fn split(items: &[Data]) -> (Data, Vec<Data>) {
    let (repos, documents): (Vec<_>, Vec<_>) = items
        .iter()
        .cloned()
        .partition(|item| cognify_route_for(item) == Ok(CognifyRoute::CodeRepo));
    assert_eq!(repos.len(), 1, "one repo manifest item: {items:?}");
    (repos.into_iter().next().unwrap(), documents)
}

#[tokio::test]
async fn a_project_directory_cognifies_into_a_code_graph_without_the_llm() {
    let (_root, repo) = project();
    let stack = Stack::new().await;

    let items = stack.add(&repo, true, None).await;
    assert_eq!(items.len(), 2, "the manifest and README.md: {items:?}");
    let (manifest, documents) = split(&items);
    let stamp: Value = serde_json::from_str(manifest.system_metadata.as_deref().unwrap()).unwrap();
    assert_eq!(stamp["source"], "code_repo");
    assert_eq!(stamp["repo_path"], repo.to_string_lossy().as_ref());
    assert_eq!(stamp["file_count"], 3, "pyproject.toml, main.py, store.py");
    assert_eq!(documents.len(), 1);
    assert!(
        documents[0].name.contains("README"),
        "{:?}",
        documents[0].name
    );
    assert!(documents[0].system_metadata.is_none());

    let result = stack
        .cognify(items.clone(), &config(true))
        .await
        .expect("cognify completes");

    // The code graph: a repository node and the snapshot's fact nodes.
    let code_nodes = stack.code_node_ids().await;
    let repository = stack
        .nodes()
        .await
        .into_iter()
        .find(|(_, node)| {
            node["type"] == "CodeRepository" && node["path"] == repo.to_string_lossy().as_ref()
        })
        .expect("the repository node");
    assert!(code_nodes.contains(&repository.0));
    assert!(code_nodes.len() > 1, "fact nodes too: {code_nodes:?}");
    assert!(
        repository.1["last_snapshot_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "the load stamped the repository"
    );

    // The README went down the standard route: it was chunked, and only it.
    assert!(!result.chunks.is_empty());
    assert!(
        result
            .chunks
            .iter()
            .all(|chunk| chunk.document_id == documents[0].id)
    );

    // One run, and every item marked complete by it.
    assert_eq!(
        stack.cognify_runs().await,
        vec![PipelineRunStatus::Completed]
    );
    let dataset_id = stack.dataset_id().await;
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let completed = get_cognify_completed_data_ids(&stack.db, dataset_id, &ids)
        .await
        .unwrap();
    assert_eq!(completed.len(), 2, "both items are marked complete");

    // The code nodes are owned by the manifest row in the ledger, under the run.
    let ledger = cognee_database::ops::graph_storage::get_nodes_by_dataset(&stack.db, dataset_id)
        .await
        .unwrap();
    let owned: BTreeSet<String> = ledger
        .iter()
        .filter(|row| row.data_id == manifest.id)
        .map(|row| row.slug.to_string())
        .collect();
    assert!(
        owned.contains(&repository.0),
        "the repository node is in the ledger"
    );
    assert!(
        ledger
            .iter()
            .filter(|row| row.data_id == manifest.id)
            .all(|row| row.pipeline_run_id.is_some())
    );

    // Cognify again: everything is complete, so nothing runs.
    let again = stack.cognify(items, &config(true)).await.unwrap();
    assert!(again.already_completed);
}

#[tokio::test]
async fn the_add_node_set_tags_every_code_node() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, true, Some(&["team-a"])).await;
    stack.cognify(items, &config(true)).await.unwrap();

    let code_nodes = stack.code_node_ids().await;
    assert!(!code_nodes.is_empty());
    let node_set = stack
        .nodes()
        .await
        .into_iter()
        .find(|(_, node)| node["type"] == "NodeSet" && node["name"] == "team-a")
        .expect("the team-a NodeSet node")
        .0;
    let (_, edges) = stack.graph_db.get_graph_data().await.unwrap();
    let tagged: BTreeSet<String> = edges
        .iter()
        .filter(|(_, target, relationship, _)| {
            relationship == "belongs_to_set" && *target == node_set
        })
        .map(|(source, _, _, _)| source.clone())
        .collect();
    assert!(
        code_nodes.is_subset(&tagged),
        "every code node belongs to team-a; untagged: {:?}",
        code_nodes.difference(&tagged).collect::<Vec<_>>()
    );
}

/// Without an LLM the project's documents are left out at add, and cognify of
/// the repo item alone needs neither an LLM nor a graph extractor.
#[tokio::test]
async fn a_keyless_add_keeps_only_the_code_graph() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, false, None).await;
    assert_eq!(items.len(), 1, "the README is excluded without an LLM");
    assert!(!cognee_cognify::routing::routes_need_llm(&items));

    let result = stack.cognify(items, &config(false)).await.unwrap();
    assert!(result.chunks.is_empty());
    assert!(!stack.code_node_ids().await.is_empty());
    assert_eq!(
        stack.cognify_runs().await,
        vec![PipelineRunStatus::Completed]
    );
}

// ── The per-file CODE route (SDK-1053 O9, Python `code_files.py`) ─────────

/// Write `files` (`(relative path, content)`) under a new temporary
/// directory and return their absolute paths.
fn code_files(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<PathBuf>) {
    let root = tempfile::tempdir().unwrap();
    let paths = files
        .iter()
        .map(|(name, content)| {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            path.canonicalize().unwrap()
        })
        .collect();
    (root, paths)
}

impl Stack {
    /// Add each of `paths` (files or project directories) to the `code`
    /// dataset, in one add.
    async fn add_paths(&self, paths: &[PathBuf], node_set: Option<&[&str]>) -> Vec<Data> {
        let pipeline = AddPipeline::new(
            Arc::clone(&self.storage),
            Arc::clone(&self.db) as Arc<dyn IngestDb>,
        )
        .with_thread_pool(Self::thread_pool())
        .with_graph_db(Arc::clone(&self.graph_db) as Arc<dyn GraphDBTrait>)
        .with_vector_db(Arc::clone(&self.vector_db) as Arc<dyn VectorDB>)
        .with_database(self.db.clone())
        .with_llm_available(true);
        let params = AddParams {
            node_set: node_set.map(|names| names.iter().map(|name| name.to_string()).collect()),
            ..AddParams::default()
        };
        pipeline
            .add_with_params(
                paths
                    .iter()
                    .map(|path| DataInput::FilePath(path.to_string_lossy().into_owned()))
                    .collect(),
                "code",
                self.owner_id,
                None,
                &params,
            )
            .await
            .expect("add the paths")
    }

    /// The graph's nodes of class `node_type`.
    async fn nodes_of_type(&self, node_type: &str) -> Vec<(String, cognee_graph::NodeData)> {
        self.nodes()
            .await
            .into_iter()
            .filter(|(_, node)| node["type"] == node_type)
            .collect()
    }

    /// The ledger's node ids owned by `data_id`, each checked to name a run.
    async fn owned_node_ids(&self, data_id: Uuid) -> BTreeSet<String> {
        let ledger = cognee_database::ops::graph_storage::get_nodes_by_dataset(
            &self.db,
            self.dataset_id().await,
        )
        .await
        .unwrap();
        ledger
            .iter()
            .filter(|row| row.data_id == data_id)
            .inspect(|row| assert!(row.pipeline_run_id.is_some(), "{row:?}"))
            .map(|row| row.slug.to_string())
            .collect()
    }
}

/// A source file added on its own is a one-file repository: `add(file.py)`
/// + `cognify()` loads its code graph through a staged enola run, with no
/// LLM and no graph extractor, under a repository named
/// `<file name>_<data id>` (Python `_staged_repo_name`), owned by the row in
/// the ledger, in one completed run.
#[tokio::test]
async fn a_code_file_cognifies_into_a_one_file_repository_without_the_llm() {
    let (_root, paths) = code_files(&[("shop/payments.py", "def pay():\r\n    pass\r\n")]);
    let stack = Stack::new().await;
    let items = stack.add_paths(&paths, None).await;
    assert_eq!(items.len(), 1);
    let item = items[0].clone();
    assert_eq!(cognify_route_for(&item), Ok(CognifyRoute::Code));
    assert!(!cognee_cognify::routing::routes_need_llm(&items));

    let result = stack
        .cognify(items.clone(), &config(false))
        .await
        .expect("cognify completes");
    assert!(
        result.chunks.is_empty(),
        "nothing went down the standard route"
    );

    let repositories = stack.nodes_of_type("CodeRepository").await;
    assert_eq!(repositories.len(), 1, "{repositories:?}");
    let (repository_id, repository) = &repositories[0];
    assert_eq!(repository["name"], format!("payments.py_{}", item.id));
    assert!(
        repository["last_snapshot_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("sha256:staged-")),
        "the load stamped the repository: {repository:?}"
    );
    let modules = stack.nodes_of_type("CodeModule").await;
    assert_eq!(modules.len(), 1, "{modules:?}");
    assert_eq!(modules[0].1["name"], "payments");

    let owned = stack.owned_node_ids(item.id).await;
    assert!(owned.contains(repository_id), "{owned:?}");
    assert!(owned.contains(&modules[0].0), "{owned:?}");

    assert_eq!(
        stack.cognify_runs().await,
        vec![PipelineRunStatus::Completed]
    );
    assert_eq!(stack.completed(&items).await, HashSet::from([item.id]));

    // The staging directory is gone.
    let staged = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("cognee_code_")
        })
        .any(|entry| {
            entry
                .path()
                .join(format!("payments.py_{}", item.id))
                .exists()
        });
    assert!(!staged, "the staged copy is removed after the load");
}

/// Plain JavaScript is only claimed by enola next to a `tsconfig.json`, so
/// staging fabricates one for `.js` / `.jsx` (Python `_DETECTION_MARKERS`);
/// without it the fake enola, like the real one, finds no module.
#[tokio::test]
async fn a_javascript_file_is_staged_with_its_tsconfig_marker() {
    let (_root, paths) = code_files(&[
        ("web/app.js", "export const app = 1;\n"),
        ("web/view.jsx", "export const View = () => null;\n"),
    ]);
    let stack = Stack::new().await;
    let items = stack.add_paths(&paths, None).await;
    assert_eq!(items.len(), 2);
    stack.cognify(items, &config(false)).await.unwrap();

    let names: BTreeSet<String> = stack
        .nodes_of_type("CodeModule")
        .await
        .into_iter()
        .map(|(_, node)| node["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(["app".to_string(), "view".to_string()])
    );
    assert!(
        stack
            .nodes()
            .await
            .iter()
            .all(|(_, node)| node["name"] != "tsconfig"),
        "the marker itself yields no node"
    );
}

/// Two files with one name are two repositories: the row id in the staged
/// repository name keeps their graphs (and sweeps) apart.
#[tokio::test]
async fn same_named_files_are_separate_repositories() {
    let (_root, paths) = code_files(&[
        ("a/utils.py", "def a():\n    pass\n"),
        ("b/utils.py", "def b():\n    pass\n"),
    ]);
    let stack = Stack::new().await;
    let items = stack.add_paths(&paths, None).await;
    assert_eq!(items.len(), 2);
    stack.cognify(items.clone(), &config(false)).await.unwrap();

    let names: BTreeSet<String> = stack
        .nodes_of_type("CodeRepository")
        .await
        .into_iter()
        .map(|(_, node)| node["name"].as_str().unwrap().to_string())
        .collect();
    let expected: BTreeSet<String> = items
        .iter()
        .map(|item| format!("utils.py_{}", item.id))
        .collect();
    assert_eq!(names, expected);
    assert_eq!(stack.nodes_of_type("CodeModule").await.len(), 2);
    let first = stack.owned_node_ids(items[0].id).await;
    let second = stack.owned_node_ids(items[1].id).await;
    assert!(!first.is_empty() && !second.is_empty());
    assert!(first.is_disjoint(&second), "{first:?} / {second:?}");
}

/// The per-file route is untagged, as in Python: an add's node set does not
/// reach the file's code nodes.
#[tokio::test]
async fn the_add_node_set_does_not_tag_a_code_file() {
    let (_root, paths) = code_files(&[("payments.py", "def pay():\n    pass\n")]);
    let stack = Stack::new().await;
    let items = stack.add_paths(&paths, Some(&["team-a"])).await;
    stack.cognify(items, &config(false)).await.unwrap();

    let code_nodes = stack.code_node_ids().await;
    assert!(!code_nodes.is_empty());
    let (_, edges) = stack.graph_db.get_graph_data().await.unwrap();
    assert!(
        edges.iter().all(|(source, _, relationship, _)| {
            relationship != "belongs_to_set" || !code_nodes.contains(source)
        }),
        "no code node belongs to a node set"
    );
}

/// A project directory, a lone code file and the project's README in one
/// dataset are one run: the repository and the file each get their code
/// graph, the README goes down the standard route, and every item is marked
/// complete by that run.
#[tokio::test]
async fn a_code_file_beside_a_project_runs_in_one_pipeline_run() {
    let (_root, repo) = project();
    let (_files, paths) = code_files(&[("scripts/tool.py", "def tool():\n    pass\n")]);
    let stack = Stack::new().await;
    let items = stack
        .add_paths(&[repo.clone(), paths[0].clone()], None)
        .await;
    assert_eq!(
        items.len(),
        3,
        "the manifest, README.md and tool.py: {items:?}"
    );
    let file = items
        .iter()
        .find(|item| cognify_route_for(item) == Ok(CognifyRoute::Code))
        .expect("the code file item")
        .clone();

    let result = stack.cognify(items.clone(), &config(true)).await.unwrap();
    assert!(!result.chunks.is_empty(), "the README was chunked");
    let names: BTreeSet<String> = stack
        .nodes_of_type("CodeRepository")
        .await
        .into_iter()
        .map(|(_, node)| node["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&format!("tool.py_{}", file.id)), "{names:?}");
    assert_eq!(names.len(), 2, "the project and the file: {names:?}");
    assert_eq!(
        stack.cognify_runs().await,
        vec![PipelineRunStatus::Completed]
    );
    assert_eq!(stack.completed(&items).await.len(), 3);
}

/// A code file whose stored text cannot be read is a per-item failure, as
/// an item's tasks fail in Python's per-item run: under `RunToEnd` +
/// `FailedItems` the other file survives and is marked complete.
#[tokio::test]
async fn an_unreadable_code_file_is_a_per_item_failure() {
    let (_root, paths) = code_files(&[
        ("good.py", "def good():\n    pass\n"),
        ("bad.py", "def bad():\n    pass\n"),
    ]);
    let stack = Stack::new().await;
    let mut items = stack.add_paths(&paths, None).await;
    let bad = items
        .iter_mut()
        .find(|item| item.original_data_location.ends_with("bad.py"))
        .unwrap();
    bad.raw_data_location = format!("{}.missing", bad.raw_data_location);
    let bad_id = bad.id;

    let report = failed_report(
        stack
            .cognify(
                items.clone(),
                &policy(
                    config(false),
                    FailureStop::RunToEnd,
                    RollbackScope::FailedItems,
                ),
            )
            .await,
        "the run errors and the good file survives",
    );
    let failed: Vec<Uuid> = report.failed_items().iter().copied().collect();
    assert_eq!(failed, vec![bad_id]);
    assert_eq!(report.total_items(), 2);
    let good_id = items.iter().find(|item| item.id != bad_id).unwrap().id;
    assert_eq!(stack.completed(&items).await, HashSet::from([good_id]));
    assert_eq!(stack.nodes_of_type("CodeModule").await.len(), 1);

    // Under `FailFast` + `WholeRun` the item's own error fails the run.
    let stack = Stack::new().await;
    let mut items = stack.add_paths(&paths, None).await;
    for item in &mut items {
        item.raw_data_location = format!("{}.missing", item.raw_data_location);
    }
    let error = stack
        .cognify(
            items,
            &config(false).with_rollback_scope(RollbackScope::WholeRun),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, CognifyError::StorageError(_)), "{error}");
}

/// Stored text that is not UTF-8 fails its item as Python's strict text-mode
/// read does (`UnicodeDecodeError`, a `ValueError`): the run errors on it
/// under `FailFast` + `WholeRun`, and no code node is written for it.
#[tokio::test]
async fn a_code_file_that_is_not_utf8_fails_its_item() {
    let (_root, paths) = code_files(&[("latin.py", "x = 1\n")]);
    let stack = Stack::new().await;
    let mut items = stack.add_paths(&paths, None).await;
    items[0].raw_data_location = stack
        .storage
        .store(b"x = '\xe9'\n", "latin_not_utf8.py")
        .await
        .unwrap();

    let error = stack
        .cognify(
            items,
            &config(false).with_rollback_scope(RollbackScope::WholeRun),
        )
        .await
        .unwrap_err();
    match &error {
        CognifyError::CodeGraph(inner) => {
            assert!(
                matches!(inner, cognee_code_graph::CodeGraphError::InvalidCodeItem(_)),
                "{inner}"
            );
            assert_eq!(inner.name(), "ValueError");
            assert!(inner.to_string().contains("not UTF-8"), "{inner}");
        }
        other => panic!("expected a code graph error, got {other}"),
    }
    assert!(stack.code_node_ids().await.is_empty());
}

/// The stored text is read with universal newlines, as Python's text mode
/// reads it: a CRLF file and its LF twin stage byte-identical content, so
/// enola sees the same snapshot (the fake enola's snapshot id is a checksum
/// of the staged files).
#[tokio::test]
async fn crlf_text_is_staged_with_universal_newlines() {
    let (_root, paths) = code_files(&[
        ("crlf/mod.py", "def f():\r\n    pass\r\n"),
        ("lf/mod.py", "def f():\n    pass\n"),
    ]);
    let stack = Stack::new().await;
    let items = stack.add_paths(&paths, None).await;
    assert_eq!(items.len(), 2, "{items:?}");
    stack.cognify(items, &config(false)).await.unwrap();

    let snapshots: BTreeSet<String> = stack
        .nodes_of_type("CodeRepository")
        .await
        .into_iter()
        .map(|(_, node)| node["last_snapshot_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(snapshots.len(), 1, "{snapshots:?}");
}

/// A repository that moved after the add fails the run with Python's message.
#[tokio::test]
async fn a_vanished_repository_fails_the_run() {
    let (root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, false, None).await;
    std::fs::rename(&repo, root.path().join("moved")).unwrap();

    let error = stack.cognify(items, &config(false)).await.unwrap_err();
    assert!(error.to_string().contains("no longer exists"), "{error}");
}

// ── Per-item failures (Python `run_tasks_data_item.py:279-305`) ─────────────

fn policy(config: CognifyConfig, stop: FailureStop, scope: RollbackScope) -> CognifyConfig {
    config.with_failure_stop(stop).with_rollback_scope(scope)
}

/// `items` with the README pointed at a storage location that does not
/// exist, so its chunking fails (a file-unit failure, no chunks).
fn with_unreadable_documents(items: &[Data]) -> Vec<Data> {
    items
        .iter()
        .cloned()
        .map(|mut item| {
            if cognify_route_for(&item) == Ok(CognifyRoute::Standard) {
                item.raw_data_location = format!("{}.missing", item.raw_data_location);
            }
            item
        })
        .collect()
}

impl Stack {
    async fn completed(&self, items: &[Data]) -> HashSet<Uuid> {
        let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
        get_cognify_completed_data_ids(&self.db, self.dataset_id().await, &ids)
            .await
            .unwrap()
            .into_iter()
            .collect()
    }
}

/// The repo loads and the README fails. Under `WholeRun` the run errors and
/// the code graph is swept with it; under `FailedItems` the loaded repo item
/// is a survivor — the run errors, as Python's does, but the code graph stays and the repo item
/// keeps its completion marker, as Python's per-item rollback keeps a
/// completed item (`modules/cognify/rollback.py`).
#[tokio::test]
async fn a_failing_document_beside_a_loaded_repo_follows_the_policy() {
    for stop in [FailureStop::FailFast, FailureStop::RunToEnd] {
        for scope in [RollbackScope::WholeRun, RollbackScope::FailedItems] {
            let (_root, repo) = project();
            let stack = Stack::new().await;
            let items = stack.add(&repo, true, None).await;
            let (manifest, documents) = split(&items);
            let outcome = stack
                .cognify(
                    with_unreadable_documents(&items),
                    &policy(config(true), stop, scope),
                )
                .await;
            let label = format!("{stop:?}/{scope:?}");

            match scope {
                RollbackScope::WholeRun => {
                    let error = outcome.expect_err(&label);
                    assert!(
                        matches!(error, CognifyError::RunFailed { .. }),
                        "{label}: {error}"
                    );
                    assert!(
                        stack.code_node_ids().await.is_empty(),
                        "{label}: the whole run is swept, code graph included"
                    );
                    assert!(stack.completed(&items).await.is_empty(), "{label}");
                    assert_eq!(
                        stack.cognify_runs().await,
                        vec![PipelineRunStatus::Errored],
                        "{label}"
                    );
                }
                _ => {
                    let report = failed_report(outcome, &label);
                    let failed: Vec<Uuid> = report.failed_items().iter().copied().collect();
                    assert_eq!(failed, vec![documents[0].id], "{label}");
                    assert_eq!(report.total_items(), 2, "{label}");
                    assert!(
                        !stack.code_node_ids().await.is_empty(),
                        "{label}: the loaded code graph is kept"
                    );
                    assert_eq!(
                        stack.completed(&items).await,
                        HashSet::from([manifest.id]),
                        "{label}: only the repo item is marked complete"
                    );
                    assert_eq!(
                        stack.cognify_runs().await,
                        vec![PipelineRunStatus::Errored],
                        "{label}"
                    );
                }
            }
        }
    }
}

/// Code items never reach the document stage's writer, so the code route
/// completes each one itself, the moment its graph is loaded (SDK-1694 O5b):
/// when the run then fails — the README's extraction fails, one chunk in one,
/// far over the ratio — a `FailedItems` run errors but keeps the loaded repo
/// item, code graph and completion marker, and sweeps the README. Before O5b
/// the escalation swept the code graph too.
#[tokio::test]
async fn a_loaded_repo_is_kept_when_the_run_fails_after_it() {
    for stop in [FailureStop::FailFast, FailureStop::RunToEnd] {
        let (_root, repo) = project();
        let stack = Stack::new().await;
        let items = stack.add(&repo, true, None).await;
        let (manifest, documents) = split(&items);
        let backend = Arc::new(MockChunkGraphExtractor::new());
        backend.set_failure_after(0);
        let config = policy(
            config(false).with_graph_backend(backend),
            stop,
            RollbackScope::FailedItems,
        );

        let error = stack
            .cognify(items.clone(), &config)
            .await
            .expect_err("the README's failed chunk is over the ratio");
        let label = format!("{stop:?}");
        assert!(
            matches!(error, CognifyError::RunFailed { .. }),
            "{label}: {error}"
        );
        assert!(
            !stack.code_node_ids().await.is_empty(),
            "{label}: the code graph the run completed is kept"
        );
        assert_eq!(
            stack.completed(&items).await,
            HashSet::from([manifest.id]),
            "{label}: and its repo item stays marked; the README does not"
        );
        let node_ids: BTreeSet<String> =
            stack.nodes().await.into_iter().map(|(id, _)| id).collect();
        assert!(
            !node_ids.contains(&documents[0].id.to_string()),
            "{label}: the README is swept"
        );
        assert_eq!(
            stack.cognify_runs().await,
            vec![PipelineRunStatus::Errored],
            "{label}"
        );
    }
}

/// The repository moved after the add; the README is fine.
///
/// - `FailFast` + `WholeRun` (the default): the repo item's own error fails
///   the run before the README is touched, as Python re-raises it.
/// - `FailFast` + `FailedItems`: the run stops at the repo item, the README
///   is never reached, and with no survivor the run fails.
/// - `RunToEnd` + `WholeRun`: the README is cognified, then swept with the
///   failed run.
/// - `RunToEnd` + `FailedItems`: the README survives and is marked complete;
///   the repo item is reported failed and left unmarked, and the run errors.
#[tokio::test]
async fn a_failing_repo_beside_good_documents_follows_the_policy() {
    for (stop, scope) in [
        (FailureStop::FailFast, RollbackScope::WholeRun),
        (FailureStop::FailFast, RollbackScope::FailedItems),
        (FailureStop::RunToEnd, RollbackScope::WholeRun),
        (FailureStop::RunToEnd, RollbackScope::FailedItems),
    ] {
        let (root, repo) = project();
        let stack = Stack::new().await;
        let items = stack.add(&repo, true, None).await;
        let (manifest, documents) = split(&items);
        std::fs::rename(&repo, root.path().join("moved")).unwrap();
        let label = format!("{stop:?}/{scope:?}");

        let outcome = stack
            .cognify(items.clone(), &policy(config(true), stop, scope))
            .await;

        if (stop, scope) == (FailureStop::RunToEnd, RollbackScope::FailedItems) {
            let report = failed_report(outcome, &label);
            let failed: Vec<Uuid> = report.failed_items().iter().copied().collect();
            assert_eq!(failed, vec![manifest.id], "{label}");
            let entry = &report.entries()[0];
            assert_eq!(entry.stage, FailureStage::CodeGraph, "{label}");
            assert!(entry.error.contains("no longer exists"), "{label}: {entry}");
            assert_eq!(
                stack.completed(&items).await,
                HashSet::from([documents[0].id]),
                "{label}"
            );
            assert_eq!(
                stack.cognify_runs().await,
                vec![PipelineRunStatus::Errored],
                "{label}"
            );
            continue;
        }

        let error = outcome.expect_err(&label);
        assert!(
            error.to_string().contains("no longer exists"),
            "{label}: {error}"
        );
        if (stop, scope) == (FailureStop::FailFast, RollbackScope::WholeRun) {
            assert!(
                matches!(error, CognifyError::CodeGraph(_)),
                "{label}: the item's own error, {error:?}"
            );
        } else {
            assert!(
                matches!(error, CognifyError::RunFailed { .. }),
                "{label}: {error:?}"
            );
        }
        assert!(stack.completed(&items).await.is_empty(), "{label}");
        assert_eq!(
            stack.cognify_runs().await,
            vec![PipelineRunStatus::Errored],
            "{label}"
        );
        // Whatever the README wrote was swept with the run.
        let ledger = cognee_database::ops::graph_storage::get_nodes_by_dataset(
            &stack.db,
            stack.dataset_id().await,
        )
        .await
        .unwrap();
        assert!(ledger.is_empty(), "{label}: {ledger:?}");
    }
}

/// A legacy per-row `dlt` stamp fails the whole run before any item runs, as
/// Python's task resolver raises for it (`classify_documents`), even beside a
/// good repo item.
#[tokio::test]
async fn a_legacy_dlt_row_fails_the_whole_run() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, true, None).await;
    let items: Vec<Data> = items
        .into_iter()
        .map(|mut item| {
            if cognify_route_for(&item) == Ok(CognifyRoute::Standard) {
                item.system_metadata = Some(r#"{"source": "dlt"}"#.to_string());
            }
            item
        })
        .collect();

    let error = stack
        .cognify(
            items.clone(),
            &policy(
                config(true),
                FailureStop::RunToEnd,
                RollbackScope::FailedItems,
            ),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            CognifyError::Route(CognifyRouteError::LegacyDltRecord { .. })
        ),
        "{error:?}"
    );
    assert!(stack.code_node_ids().await.is_empty(), "no enola work");
    assert!(stack.completed(&items).await.is_empty());
    assert_eq!(stack.cognify_runs().await, vec![PipelineRunStatus::Errored]);
}

/// A caller that builds the pipeline itself (`build_cognify_pipeline` +
/// `cognee_core::execute`) gets the code routes too: a repository manifest
/// goes down its code graph route, never down the LLM path.
#[tokio::test]
async fn the_public_pipeline_routes_a_code_repository() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, false, None).await;
    assert_eq!(items.len(), 1, "the manifest only: {items:?}");
    let dataset_id = stack.dataset_id().await;

    let pipeline = cognee_cognify::build_cognify_pipeline(
        Arc::clone(&stack.storage),
        Arc::clone(&stack.graph_db) as Arc<dyn GraphDBTrait>,
        Arc::clone(&stack.vector_db) as Arc<dyn VectorDB>,
        Arc::new(MockEmbeddingEngine::new(8)),
        Arc::new(PanickingLlm),
        stack.db.clone(),
        Arc::new(NoOpOntologyResolver::new()),
        config(false),
    );
    let pipeline_ctx = cognee_core::PipelineContext {
        pipeline_id: pipeline.id,
        pipeline_name: pipeline.name.clone().unwrap_or_default(),
        user_id: Some(stack.owner_id),
        tenant_id: None,
        dataset_id: Some(dataset_id),
        current_data: None,
        run_id: None,
        user_email: None,
        provenance_visited: Arc::new(std::sync::Mutex::new(HashSet::new())),
    };
    let (_cancel, ctx) = cognee_core::TaskContextBuilder::new()
        .thread_pool(Stack::thread_pool())
        .database(stack.db.clone())
        .graph_db(Arc::clone(&stack.graph_db) as Arc<dyn GraphDBTrait>)
        .vector_db(Arc::clone(&stack.vector_db) as Arc<dyn VectorDB>)
        .pipeline_context(pipeline_ctx)
        .build()
        .unwrap();
    let input = CognifyInput {
        data_items: items,
        dataset_id,
        user_id: Some(stack.owner_id),
        tenant_id: None,
    };
    cognee_core::execute(
        &pipeline,
        vec![Arc::new(input) as Arc<dyn cognee_core::Value>],
        Arc::new(ctx),
        &cognee_core::NoopWatcher,
    )
    .await
    .expect("the pipeline runs the code route");
    assert!(
        stack.code_node_ids().await.len() > 1,
        "the repository was loaded as a code graph"
    );
}

/// The legacy refusal runs before the completion markers drop anything: a
/// legacy `dlt` row an earlier run completed is still refused, rather than
/// skipped as already done.
#[tokio::test]
async fn a_completed_legacy_dlt_row_is_still_refused() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, true, None).await;
    stack.cognify(items.clone(), &config(true)).await.unwrap();
    assert_eq!(stack.completed(&items).await.len(), 2);

    let items: Vec<Data> = items
        .into_iter()
        .map(|mut item| {
            if cognify_route_for(&item) == Ok(CognifyRoute::Standard) {
                item.system_metadata = Some(r#"{"source": "dlt"}"#.to_string());
            }
            item
        })
        .collect();
    let error = stack
        .cognify(items, &config(true))
        .await
        .expect_err("a legacy row is refused, not skipped");
    assert!(
        matches!(
            error,
            CognifyError::Route(CognifyRouteError::LegacyDltRecord { .. })
        ),
        "{error:?}"
    );
    assert_eq!(
        stack.cognify_runs().await,
        vec![PipelineRunStatus::Errored, PipelineRunStatus::Completed],
        "newest first: the refusal ran as its own run"
    );
}

/// Only the items a run will process decide whether it needs the LLM: once
/// the standard items are complete, a dataset of them plus a new code
/// repository needs none.
#[tokio::test]
async fn completed_standard_items_need_no_llm() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, true, None).await;
    let dataset_id = stack.dataset_id().await;
    let pending = |incremental| {
        let items = items.clone();
        let db = Arc::clone(&stack.db);
        async move {
            cognee_cognify::rollback::pending_routes_need_llm(&*db, dataset_id, &items, incremental)
                .await
                .unwrap()
        }
    };
    assert!(pending(true).await, "the README is not cognified yet");

    stack.cognify(items.clone(), &config(true)).await.unwrap();
    assert!(
        !pending(true).await,
        "only completed items take the standard route"
    );
    // Without incremental loading every item runs again.
    assert!(pending(false).await);
}

/// A row stamped `system_metadata.source == "dlt_source"` is refused rather
/// than sent down the LLM pipeline: Rust reads DLT stamps from
/// `external_metadata` only.
#[tokio::test]
async fn a_dlt_source_row_is_refused() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, true, None).await;
    let (_, documents) = split(&items);
    let mut row = documents[0].clone();
    row.system_metadata = Some(r#"{"source": "dlt_source"}"#.to_string());
    assert!(!cognee_cognify::routing::routes_need_llm(
        std::slice::from_ref(&row)
    ));

    let error = stack
        .cognify(vec![row.clone()], &config(true))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            CognifyError::Route(CognifyRouteError::UnsupportedRoute {
                route: CognifyRoute::DltSource,
                data_id,
            }) if data_id == row.id
        ),
        "{error:?}"
    );
    assert!(stack.completed(&[row]).await.is_empty());
    assert_eq!(stack.cognify_runs().await, vec![PipelineRunStatus::Errored]);
}

/// A repo row whose `external_metadata["node_set"]` is malformed fails as
/// that item, inside the route (Python reads it in `extract_code_repo_graph`).
#[tokio::test]
async fn a_malformed_node_set_fails_its_repo_item_in_run_code_routes() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, true, None).await;
    let (mut manifest, documents) = split(&items);
    manifest.external_metadata = Some(r#"{"node_set": "team-a"}"#.to_string());
    let input = CognifyInput {
        data_items: vec![manifest.clone(), documents[0].clone()],
        dataset_id: stack.dataset_id().await,
        user_id: Some(stack.owner_id),
        tenant_id: None,
    };
    let code_graph = config(true).code_graph;
    let run = |failure_policy: FailurePolicy| {
        let input = input.clone();
        let code_graph = code_graph.clone();
        let stack = &stack;
        async move {
            run_code_routes(
                &input,
                &*stack.storage,
                &*stack.graph_db,
                &*stack.db,
                &code_graph,
                None,
                failure_policy,
                &cognee_cognify::CodeRepoLoadMode::Dataset,
                None,
            )
            .await
        }
    };

    // Run to the end: the repo item is recorded, the README goes on.
    let routed = run(FailurePolicy {
        stop: FailureStop::RunToEnd,
        ..FailurePolicy::default()
    })
    .await
    .expect("a per-item failure does not fail the routes");
    assert_eq!(routed.input.data_items.len(), 1);
    assert_eq!(routed.input.data_items[0].id, documents[0].id);
    assert_eq!(routed.failures.routed_items(), 1);
    let failed: Vec<Uuid> = routed.failures.failed_items().iter().copied().collect();
    assert_eq!(failed, vec![manifest.id]);
    let entry = &routed.failures.entries()[0];
    assert_eq!(entry.stage, FailureStage::CodeGraph);
    assert!(entry.error.contains("Invalid node_set"), "{entry}");

    // Fail fast, keeping failed items only: the README is never reached.
    let routed = run(FailurePolicy {
        stop: FailureStop::FailFast,
        scope: RollbackScope::FailedItems,
        ..FailurePolicy::default()
    })
    .await
    .expect("FailedItems keeps the run going");
    assert!(routed.input.data_items.is_empty());
    assert!(routed.failures.unreached_items().contains(&documents[0].id));

    // The default fails fast with the item's own error.
    let error = run(FailurePolicy::default()).await.unwrap_err();
    assert!(
        matches!(
            error,
            CognifyError::Route(CognifyRouteError::InvalidNodeSet { data_id, .. })
                if data_id == manifest.id
        ),
        "{error:?}"
    );
    assert!(stack.code_node_ids().await.is_empty(), "nothing was loaded");
}

/// A Python project named `name` under `root` whose fake enola cluster run
/// copies the `cluster` snapshot (a frontend `ui` reaching a backend `golf`).
fn cluster_project(root: &Path, name: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join("pyproject.toml"),
        format!("[project]\nname = \"{name}\"\n"),
    )
    .unwrap();
    std::fs::write(repo.join("main.py"), "def main():\n    pass\n").unwrap();
    // Whichever row comes first leads the cluster run.
    std::fs::write(repo.join(".fake-enola"), "cluster:cluster").unwrap();
    repo.canonicalize().unwrap()
}

#[tokio::test]
async fn a_dataset_of_two_repositories_cognifies_as_one_enola_cluster() {
    let root = tempfile::tempdir().unwrap();
    let golf = cluster_project(root.path(), "golf");
    let ui = cluster_project(root.path(), "ui");
    let stack = Stack::new().await;
    let mut items = stack.add(&golf, false, None).await;
    items.extend(stack.add(&ui, false, None).await);
    assert_eq!(items.len(), 2, "one manifest per repository: {items:?}");
    let dataset_id = stack.dataset_id().await;

    stack
        .cognify(items.clone(), &config(false))
        .await
        .expect("cognify completes");

    // One cluster run into the dataset's own directory of every member; no
    // per-repository run.
    let output_dir = cognee_code_graph::cluster_output_dir(dataset_id);
    for repo in [&golf, &ui] {
        assert!(repo.join(&output_dir).join("facts.jsonl").is_file());
        assert!(!repo.join(".enola").exists(), "no single run on {repo:?}");
    }
    let config_seen = [&golf, &ui]
        .iter()
        .filter(|repo| repo.join(&output_dir).join("cluster-config.yaml").is_file())
        .count();
    assert_eq!(config_seen, 1, "enola ran once");

    // The edge between the repositories.
    let id = |repo: &str, kind: &str, name: &str| {
        cognee_code_graph::fact_node_id(repo, kind, name).to_string()
    };
    let (_, edges) = stack.graph_db.get_graph_data().await.unwrap();
    assert!(edges.iter().any(|(source, target, relationship, _)| {
        *source == id("ui", "route", "/api/rounds")
            && *target == id("golf", "route", "/api/rounds")
            && relationship == "reaches_route"
    }));

    // Each repository's nodes are owned by its own row.
    let row_of = |repo: &Path| {
        items
            .iter()
            .find(|item| {
                cognee_cognify::code_cluster::code_repo_path(item).as_deref() == Some(repo)
            })
            .unwrap()
            .id
    };
    let ledger = cognee_database::ops::graph_storage::get_nodes_by_dataset(&stack.db, dataset_id)
        .await
        .unwrap();
    let owner = |node_id: &str| -> BTreeSet<Uuid> {
        ledger
            .iter()
            .filter(|row| row.slug.to_string() == node_id)
            .map(|row| row.data_id)
            .collect()
    };
    assert_eq!(
        owner(&id("golf", "repository", "golf")),
        BTreeSet::from([row_of(&golf)])
    );
    assert_eq!(
        owner(&id("ui", "symbol", "api.fetchRounds")),
        BTreeSet::from([row_of(&ui)])
    );

    // Both repo items are complete.
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let completed = get_cognify_completed_data_ids(&stack.db, dataset_id, &ids)
        .await
        .unwrap();
    assert_eq!(completed.len(), 2);

    // After a delete, the survivors are marked for the next cognify
    // (Python's `mark_dataset_code_repos_for_rebuild`): their completion
    // stamps go, so the next run re-extracts the cluster instead of skipping.
    cognee_cognify::code_cluster::mark_dataset_code_repos_for_rebuild(&*stack.db, dataset_id).await;
    let completed = get_cognify_completed_data_ids(&stack.db, dataset_id, &ids)
        .await
        .unwrap();
    assert!(completed.is_empty(), "{completed:?}");
}

#[tokio::test]
async fn a_failed_cluster_run_fails_every_repo_item_of_the_run() {
    let root = tempfile::tempdir().unwrap();
    let golf = cluster_project(root.path(), "golf");
    let ui = cluster_project(root.path(), "ui");
    std::fs::write(golf.join(".fake-enola"), "fail").unwrap();
    std::fs::write(ui.join(".fake-enola"), "fail").unwrap();
    let stack = Stack::new().await;
    let mut items = stack.add(&golf, false, None).await;
    items.extend(stack.add(&ui, false, None).await);
    let dataset_id = stack.dataset_id().await;

    let input = CognifyInput {
        data_items: items.clone(),
        dataset_id,
        user_id: Some(stack.owner_id),
        tenant_id: None,
    };
    let routed = run_code_routes(
        &input,
        &*stack.storage,
        &*stack.graph_db,
        &*stack.db,
        &config(false).code_graph,
        None,
        FailurePolicy {
            stop: FailureStop::RunToEnd,
            ..FailurePolicy::default()
        },
        &cognee_cognify::CodeRepoLoadMode::Dataset,
        None,
    )
    .await
    .unwrap();

    // Both items fail with enola's error; enola ran once (the second item
    // replays the verdict instead of running it again).
    let failures: Vec<_> = routed
        .failures
        .entries()
        .iter()
        .filter(|failure| failure.stage == FailureStage::CodeGraph)
        .collect();
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures
            .iter()
            .all(|failure| failure.error.contains("exited with code 3")),
        "{failures:?}"
    );
}
