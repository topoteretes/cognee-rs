//! `add(<project dir>)` then `cognify()`: the CODE_REPO route (SDK-1053 O4).
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
use cognee_cognify::{CognifyConfig, CognifyError, CognifyResult, cognify};
use cognee_database::ops::data::{get_cognify_completed_data_ids, update_data_system_metadata};
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

/// A per-file code item has no Rust route yet: the run fails on it before any
/// enola work, and nothing is marked complete.
#[tokio::test]
async fn a_per_file_code_item_fails_the_run() {
    let (_root, repo) = project();
    let stack = Stack::new().await;
    let items = stack.add(&repo, true, None).await;
    let (_, documents) = split(&items);
    let mut document = documents[0].clone();
    let stamp = r#"{"source": "code"}"#;
    update_data_system_metadata(&stack.db, document.id, Some(stamp))
        .await
        .unwrap();
    document.system_metadata = Some(stamp.to_string());

    let error = stack
        .cognify(
            items
                .iter()
                .map(|item| {
                    if item.id == document.id {
                        document.clone()
                    } else {
                        item.clone()
                    }
                })
                .collect(),
            &config(true),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            CognifyError::Route(CognifyRouteError::UnsupportedRoute {
                route: CognifyRoute::Code,
                ..
            })
        ),
        "{error}"
    );
    assert!(
        stack.code_node_ids().await.is_empty(),
        "no enola work was done"
    );
    assert_eq!(stack.cognify_runs().await, vec![PipelineRunStatus::Errored]);
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
