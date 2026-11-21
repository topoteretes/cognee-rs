//! `remember(content_type="code")` (SDK-1053 O5): keyless code graphs of
//! local repositories.
//!
//! No LLM or embedding engine is given to `remember_code` (except by the
//! `index_vectors` tests), so the path is keyless by construction. enola is the shell script the
//! `cognee-code-graph` tests use (`tests/fixtures/bin/enola`), which copies a
//! snapshot fixture into the repository's `.enola/`; the `#[ignore]`d test at
//! the end runs the real enola 0.5.1 on Python's e2e fixture.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cognee::api::{ApiError, RememberCodeParams, RememberStatus, remember_code};
use cognee_code_graph::{CodeGraphConfig, CodeNodeType};
use cognee_cognify::CognifyConfig;
use cognee_cognify::routing::{CognifyRoute, cognify_route_for};
use cognee_database::ops::data::get_data;
use cognee_database::ops::datasets::{get_dataset_by_name_in_tenant, get_dataset_data};
use cognee_database::{
    DatabaseConnection, IngestDb, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize,
};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_ingestion::AddPipeline;
use cognee_models::DataInput;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{MockVectorDB, VectorDB};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const DATASET: &str = "code";

fn fake_enola() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../code-graph/tests/fixtures/bin/enola")
}

/// A Python project whose fake enola run follows `mode` (`.fake-enola`, a
/// dotfile the manifest skips). README.md is a document: never stored here.
fn project(root: &Path, name: &str, mode: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(repo.join("inventory")).unwrap();
    std::fs::write(repo.join("pyproject.toml"), "[project]\nname = \"shop\"\n").unwrap();
    std::fs::write(repo.join("main.py"), "def main():\n    pass\n").unwrap();
    std::fs::write(repo.join("inventory/store.py"), "class Store:\n    pass\n").unwrap();
    std::fs::write(repo.join("README.md"), "Shop keeps an inventory.\n").unwrap();
    std::fs::write(repo.join(".fake-enola"), mode).unwrap();
    repo.canonicalize().unwrap()
}

struct Stack {
    _temp: TempDir,
    db: Arc<DatabaseConnection>,
    runs: Arc<dyn PipelineRunRepository>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<MockGraphDB>,
    vector_db: Arc<MockVectorDB>,
    owner_id: Uuid,
    enola_path: String,
}

impl Stack {
    async fn new() -> Self {
        Self::with_enola(fake_enola().to_string_lossy().into_owned()).await
    }

    async fn with_enola(enola_path: String) -> Self {
        let temp = TempDir::new().unwrap();
        let conn = connect("sqlite::memory:").await.unwrap();
        initialize(&conn).await.unwrap();
        let db = Arc::new(conn);
        let storage: Arc<dyn StorageTrait> =
            Arc::new(LocalStorage::new(temp.path().join("storage")));
        storage.initialize().await.unwrap();
        Self {
            _temp: temp,
            runs: Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db))),
            db,
            storage,
            graph_db: Arc::new(MockGraphDB::new()),
            vector_db: Arc::new(MockVectorDB::new()),
            owner_id: Uuid::new_v4(),
            enola_path,
        }
    }

    fn params(&self, data: Vec<DataInput>, raise_on_error: bool) -> RememberCodeParams {
        let add_pipeline = AddPipeline::new(
            Arc::clone(&self.storage),
            Arc::clone(&self.db) as Arc<dyn IngestDb>,
        )
        .with_thread_pool(Arc::new(
            cognee::core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(Arc::clone(&self.graph_db) as Arc<dyn GraphDBTrait>)
        .with_vector_db(Arc::clone(&self.vector_db) as Arc<dyn VectorDB>)
        .with_database(self.db.clone())
        // Even an add pipeline told no LLM is usable stores the repository.
        .with_llm_available(false);
        let config = CognifyConfig::default().with_code_graph(CodeGraphConfig {
            enola_path: self.enola_path.clone(),
            ..CodeGraphConfig::default()
        });
        RememberCodeParams {
            data,
            dataset_name: DATASET.to_string(),
            owner_id: self.owner_id,
            tenant_id: None,
            dataset_id: None,
            node_set: None,
            repo_credentials: None,
            index_vectors: false,
            embedding_engine: None,
            raise_on_error,
            add_pipeline: Arc::new(add_pipeline),
            storage: Arc::clone(&self.storage),
            graph_db: Arc::clone(&self.graph_db) as Arc<dyn GraphDBTrait>,
            vector_db: Arc::clone(&self.vector_db) as Arc<dyn VectorDB>,
            db: self.db.clone(),
            cognify_config: Arc::new(config),
        }
    }

    async fn remember(
        &self,
        params: RememberCodeParams,
    ) -> Result<cognee::api::RememberResult, ApiError> {
        // A deadlock on the dataset lock would hang: fail it instead.
        tokio::time::timeout(Duration::from_secs(60), remember_code(params))
            .await
            .expect("remember_code finished")
    }

    async fn dataset_id(&self) -> Uuid {
        get_dataset_by_name_in_tenant(&self.db, DATASET, self.owner_id, None)
            .await
            .unwrap()
            .expect("the dataset exists")
            .id
    }

    async fn nodes(&self) -> Vec<(String, cognee_graph::NodeData)> {
        self.graph_db.get_graph_data().await.unwrap().0
    }

    async fn node_types(&self) -> HashSet<String> {
        self.nodes()
            .await
            .into_iter()
            .filter_map(|(_, node)| node.get("type").and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    /// `(pipeline_name, status)` of every run row of the dataset.
    async fn run_rows(&self) -> Vec<(String, PipelineRunStatus, Uuid)> {
        self.runs
            .list_recent(Some(self.dataset_id().await), 100)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.pipeline_name, row.status, row.pipeline_run_id))
            .collect()
    }
}

fn code_types() -> HashSet<String> {
    CodeNodeType::ALL
        .iter()
        .map(|kind| kind.class_name().to_string())
        .collect()
}

fn path_input(path: &Path) -> DataInput {
    DataInput::FilePath(path.to_string_lossy().into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_repository_is_remembered_as_a_code_graph_without_an_llm() {
    let root = TempDir::new().unwrap();
    let repo = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;

    let result = stack
        .remember(stack.params(vec![path_input(&repo)], true))
        .await
        .expect("remember the repository");

    assert_eq!(result.status, RememberStatus::Completed, "{result}");
    assert_eq!(result.items_processed, 1);
    assert_eq!(result.error, None);
    let dataset_id = stack.dataset_id().await;
    assert_eq!(result.dataset_id, Some(dataset_id));
    assert_eq!(result.dataset_name, DATASET);

    // One item, in Python's shape.
    assert_eq!(result.items.len(), 1);
    let item = &result.items[0];
    let data_id = item.id.expect("the repository's data id");
    let run_id = item.pipeline_run_id.expect("the run id");
    assert_eq!(result.pipeline_run_id, Some(run_id));
    let json = serde_json::to_value(&result).unwrap();
    assert_eq!(
        json["items"],
        json!([{
            "kind": "code_repository",
            "source": repo.to_string_lossy(),
            "path": repo.to_string_lossy(),
            "id": data_id,
            "pipeline_run_id": run_id,
        }])
    );

    // One code-repo row, without the README.
    let rows = get_dataset_data(&stack.db, dataset_id).await.unwrap();
    assert_eq!(rows.len(), 1, "the repository only: {rows:?}");
    assert_eq!(rows[0].id, data_id);
    assert_eq!(cognify_route_for(&rows[0]), Ok(CognifyRoute::CodeRepo));

    // Only code nodes, the repository among them.
    let types = stack.node_types().await;
    assert!(types.contains("CodeRepository"), "{types:?}");
    assert!(types.is_subset(&code_types()), "{types:?}");

    // The run is recorded as `code_graph_pipeline`, with the run id reported.
    let rows = stack.run_rows().await;
    assert!(
        rows.iter()
            .any(|(name, status, id)| name == "code_graph_pipeline"
                && *status == PipelineRunStatus::Completed
                && *id == run_id),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|(name, _, _)| name == "cognify_pipeline"),
        "no cognify run: {rows:?}"
    );

    // Stamped complete under both pipeline names.
    let row = get_data(&stack.db, data_id).await.unwrap().unwrap();
    let status: Value = serde_json::from_str(row.pipeline_status.as_deref().unwrap()).unwrap();
    let key = dataset_id.to_string();
    assert_eq!(
        status["cognify_pipeline"][&key],
        "DATA_ITEM_PROCESSING_COMPLETED"
    );
    assert_eq!(
        status["code_graph_pipeline"][&key],
        "DATA_ITEM_PROCESSING_COMPLETED"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_set_is_ignored() {
    let root = TempDir::new().unwrap();
    let repo = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;

    let mut params = stack.params(vec![path_input(&repo)], true);
    params.node_set = Some(vec!["team_a".to_string()]);
    stack.remember(params).await.unwrap();

    for (id, node) in stack.nodes().await {
        let tagged = node
            .get("belongs_to_set")
            .and_then(Value::as_array)
            .is_some_and(|sets| !sets.is_empty());
        assert!(!tagged, "node {id} is tagged: {node:?}");
    }
    assert!(
        !stack.node_types().await.contains("NodeSet"),
        "no NodeSet node"
    );
}

/// The stored repository node: its stamp.
async fn repository_stamp(stack: &Stack) -> cognee_graph::NodeData {
    stack
        .nodes()
        .await
        .into_iter()
        .map(|(_, node)| node)
        .find(|node| node["type"] == "CodeRepository")
        .expect("a repository node")
}

/// Re-remembering an unchanged repository is a no-op (SDK-1053 O6): a second
/// run, but the same graph, the same snapshot id and no new stamp.
#[tokio::test(flavor = "multi_thread")]
async fn remembering_an_unchanged_repository_again_writes_nothing() {
    let root = TempDir::new().unwrap();
    let repo = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;

    let first = stack
        .remember(stack.params(vec![path_input(&repo)], true))
        .await
        .unwrap();
    let (nodes, edges) = (stack.graph_db.node_count(), stack.graph_db.edge_count());
    let stamp = repository_stamp(&stack).await;
    let second = stack
        .remember(stack.params(vec![path_input(&repo)], true))
        .await
        .unwrap();

    assert_eq!(second.status, RememberStatus::Completed);
    assert_eq!(
        first.items[0].id, second.items[0].id,
        "one row per repository"
    );
    assert_ne!(
        first.pipeline_run_id, second.pipeline_run_id,
        "a second run"
    );
    assert_eq!(stack.graph_db.node_count(), nodes);
    assert_eq!(stack.graph_db.edge_count(), edges);
    let again = repository_stamp(&stack).await;
    assert_eq!(again["last_snapshot_id"], stamp["last_snapshot_id"]);
    assert_eq!(again["last_delta"], stamp["last_delta"], "not restamped");
}

/// A changed repository is loaded incrementally: what left the snapshot
/// leaves the graph.
#[tokio::test(flavor = "multi_thread")]
async fn remembering_a_changed_repository_sweeps_what_left_it() {
    let root = TempDir::new().unwrap();
    let repo = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;
    stack
        .remember(stack.params(vec![path_input(&repo)], true))
        .await
        .unwrap();

    std::fs::write(repo.join(".fake-enola"), "snapshot:basic_v2").unwrap();
    stack
        .remember(stack.params(vec![path_input(&repo)], true))
        .await
        .unwrap();

    let names: BTreeSet<String> = stack
        .nodes()
        .await
        .into_iter()
        .filter_map(|(_, node)| node.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert!(names.contains("Save"), "{names:?}");
    assert!(!names.contains("Color"), "{names:?}");
    assert!(!names.contains("orders_table"), "{names:?}");
    let stamp = repository_stamp(&stack).await;
    assert_eq!(stamp["last_snapshot_id"], json!("sha256:abc123def789"));
    assert_eq!(stamp["last_delta"]["nodes_removed"], json!(2));
}

/// Forgetting one of two remembered repositories removes that repository's
/// code graph through its ledger rows and leaves the other one whole.
#[tokio::test(flavor = "multi_thread")]
async fn forgetting_one_of_two_repositories_keeps_the_other() {
    use cognee::api::forget::{DatasetRef, ForgetTarget, forget};
    use cognee::delete::DeleteService;
    use cognee_database::DeleteDb;

    let root = TempDir::new().unwrap();
    let shop = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;
    let kept = stack
        .remember(stack.params(vec![path_input(&shop)], true))
        .await
        .unwrap();
    let kept_shape = graph_shape(&stack).await;
    let other = project(root.path(), "other", "snapshot:docs_and_flows");
    let forgotten = stack
        .remember(stack.params(vec![path_input(&other)], true))
        .await
        .unwrap();
    let forgotten_id = forgotten.items[0].id.unwrap();
    assert_ne!(kept.items[0].id.unwrap(), forgotten_id);
    assert!(graph_shape(&stack).await.0.len() > kept_shape.0.len());

    let service = DeleteService::new(
        Arc::clone(&stack.storage),
        Arc::clone(&stack.db) as Arc<dyn DeleteDb>,
    )
    .with_graph_db(Arc::clone(&stack.graph_db) as Arc<dyn GraphDBTrait>)
    .with_vector_db(Arc::clone(&stack.vector_db) as Arc<dyn VectorDB>);
    let dataset_id = stack.dataset_id().await;
    forget(
        ForgetTarget::Item {
            data_id: forgotten_id,
            dataset: DatasetRef::Id(dataset_id),
        },
        stack.owner_id,
        None,
        &service,
        Some(stack.db.as_ref() as &dyn IngestDb),
    )
    .await
    .expect("forget the second repository");

    assert_eq!(graph_shape(&stack).await, kept_shape);
    let rows = get_dataset_data(&stack.db, dataset_id).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(Some(rows[0].id), kept.items[0].id);
}

type EdgeKeys = BTreeSet<(String, String, String)>;

/// Node ids and edge keys of the stack's graph.
async fn graph_shape(stack: &Stack) -> (BTreeSet<String>, EdgeKeys) {
    let (nodes, edges) = stack.graph_db.get_graph_data().await.unwrap();
    (
        nodes.into_iter().map(|(id, _)| id).collect(),
        edges
            .into_iter()
            .map(|(source, target, relationship, _)| (source, target, relationship))
            .collect(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dataset_id_targets_that_dataset() {
    let root = TempDir::new().unwrap();
    let first = project(root.path(), "shop", "snapshot:basic");
    let second = project(root.path(), "store", "snapshot:basic");
    let stack = Stack::new().await;

    stack
        .remember(stack.params(vec![path_input(&first)], true))
        .await
        .unwrap();
    let dataset_id = stack.dataset_id().await;

    let mut params = stack.params(vec![path_input(&second)], true);
    params.dataset_id = Some(dataset_id);
    params.dataset_name = "another name".to_string();
    let result = stack.remember(params).await.unwrap();

    assert_eq!(result.dataset_id, Some(dataset_id));
    assert_ne!(
        result.dataset_name, "another name",
        "the dataset answers under its own name, as in Python"
    );
    let rows = get_dataset_data(&stack.db, dataset_id).await.unwrap();
    assert_eq!(
        rows.len(),
        2,
        "both repositories in the one dataset: {rows:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_path_string_names_a_repository_too() {
    let root = TempDir::new().unwrap();
    let repo = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;

    let result = stack
        .remember(stack.params(
            vec![DataInput::Text(repo.to_string_lossy().into_owned())],
            true,
        ))
        .await
        .unwrap();
    assert_eq!(result.status, RememberStatus::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn data_that_names_no_repository_is_refused_before_anything_is_stored() {
    let stack = Stack::new().await;
    for data in [
        vec![],
        vec![DataInput::Binary {
            data: b"x".to_vec(),
            name: "x.py".to_string(),
        }],
        vec![DataInput::DataItem {
            data: Box::new(DataInput::Text("x".to_string())),
            label: None,
            external_metadata: None,
            system_metadata: None,
        }],
    ] {
        let error = stack.remember(stack.params(data, true)).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "Invalid argument: content_type='code' expects a repository path or git URL \
             (or a list of them) as data."
        );
    }
    // An add pipeline that configured nothing refuses remote repositories
    // (Python's API default), before anything is stored or cloned.
    for url in ["https://github.com/o/r", "git@github.com:o/r.git"] {
        let error = stack
            .remember(stack.params(vec![DataInput::Text(url.to_string())], false))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ApiError::CodeRepositoryForbidden(m)
                if m.starts_with("Outbound HTTP requests are disabled by default")),
            "{error}"
        );
    }
    assert!(
        get_dataset_by_name_in_tenant(&stack.db, DATASET, stack.owner_id, None)
            .await
            .unwrap()
            .is_none(),
        "nothing was stored"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_directory_raises_or_is_recorded_on_its_item() {
    let root = TempDir::new().unwrap();
    let repo = project(root.path(), "shop", "snapshot:basic");
    let missing = root.path().join("gone");
    let stack = Stack::new().await;

    let error = stack
        .remember(stack.params(vec![path_input(&missing), path_input(&repo)], true))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "Repository path '{}' is not a directory. Pass a local repo path or a remote git URL.",
            missing.display()
        )
    );

    // raise_on_error = false: the batch goes on.
    let result = stack
        .remember(stack.params(vec![path_input(&missing), path_input(&repo)], false))
        .await
        .unwrap();
    assert_eq!(result.status, RememberStatus::Errored);
    assert_eq!(result.items_processed, 1);
    assert_eq!(result.items[0].status.as_deref(), Some("errored"));
    assert_eq!(result.items[0].id, None);
    assert_eq!(result.items[1].status, None);
    assert!(result.items[1].pipeline_run_id.is_some());
    assert_eq!(
        result.error.as_deref(),
        Some(
            format!(
                "{}: CodeRepositoryError: Repository path '{}' is not a directory. Pass a \
                 local repo path or a remote git URL. (Status code: 500)",
                missing.display(),
                missing.display()
            )
            .as_str()
        )
    );
    let item = serde_json::to_value(&result.items[0]).unwrap();
    assert_eq!(
        item.as_object().unwrap().keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            &"error".to_string(),
            &"kind".to_string(),
            &"source".to_string(),
            &"status".to_string()
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_enola_run_is_swept_and_reported_with_the_rows_id() {
    let root = TempDir::new().unwrap();
    let repo = project(root.path(), "shop", "fail");
    let stack = Stack::new().await;

    let error = stack
        .remember(stack.params(vec![path_input(&repo)], true))
        .await
        .unwrap_err();
    assert!(matches!(error, ApiError::Cognify(_)), "{error}");

    let result = stack
        .remember(stack.params(vec![path_input(&repo)], false))
        .await
        .unwrap();
    assert_eq!(result.status, RememberStatus::Errored);
    let item = &result.items[0];
    assert_eq!(item.status.as_deref(), Some("errored"));
    assert!(item.id.is_some(), "the stored row's id is still reported");
    assert!(item.error.as_deref().unwrap().contains("boom"), "{item:?}");
    assert!(stack.nodes().await.is_empty(), "nothing left behind");
    let rows = stack.run_rows().await;
    assert!(
        rows.iter()
            .any(|(name, status, _)| name == "code_graph_pipeline"
                && *status == PipelineRunStatus::Errored),
        "{rows:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_enola_run_fails_its_repository_under_any_failure_policy() {
    // A run-to-end, per-item cognify policy records a failed repo item instead
    // of raising it; the code graph pipeline must still fail the repository
    // rather than stamp it complete.
    let root = TempDir::new().unwrap();
    let repo = project(root.path(), "shop", "fail");
    let stack = Stack::new().await;
    let mut params = stack.params(vec![path_input(&repo)], false);
    params.cognify_config = Arc::new(
        (*params.cognify_config)
            .clone()
            .with_failure_stop(cognee_cognify::FailureStop::RunToEnd)
            .with_rollback_scope(cognee_cognify::RollbackScope::FailedItems),
    );

    let result = stack.remember(params).await.unwrap();
    assert_eq!(result.status, RememberStatus::Errored);
    let item = &result.items[0];
    assert_eq!(item.status.as_deref(), Some("errored"), "{item:?}");
    assert!(item.pipeline_run_id.is_none(), "{item:?}");
    assert!(item.error.as_deref().unwrap().contains("boom"), "{item:?}");
    assert!(stack.nodes().await.is_empty(), "nothing left behind");
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

/// The real enola 0.5.1 on Python's e2e fixture, through
/// `remember(content_type="code")`, with the graph assertions of
/// `cognee/tests/test_code_graph_e2e.py:143-256`. Run with
/// `ENOLA_PATH=<enola> cargo test -p cognee --test remember_code -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real enola 0.5.1 binary (ENOLA_PATH or PATH)"]
#[allow(
    clippy::disallowed_methods,
    reason = "test-only: locates the developer's enola binary"
)]
async fn real_enola_remembers_the_python_e2e_fixture() {
    let stack = Stack::with_enola(std::env::var("ENOLA_PATH").unwrap_or_default()).await;
    let root = TempDir::new().unwrap();
    let repo = root.path().join("code_repo_fixture");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../code-graph/tests/fixtures/code_repo_fixture"),
        &repo,
    );

    let result = stack
        .remember(stack.params(vec![path_input(&repo)], true))
        .await
        .unwrap();
    assert_eq!(result.status, RememberStatus::Completed, "{result}");
    assert_eq!(result.items_processed, 1);
    assert!(result.pipeline_run_id.is_some());
    assert!(repo.join(".enola/facts.jsonl").is_file());

    let (nodes, edges) = stack.graph_db.get_graph_data().await.unwrap();
    let edges: Vec<_> = edges.into_iter().filter(|e| e.0 != e.1).collect();
    let types: HashSet<String> = nodes
        .iter()
        .filter_map(|(_, n)| n.get("type").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert!(types.is_subset(&code_types()), "{types:?}");

    let of_type = |class: &str| -> Vec<&(String, cognee_graph::NodeData)> {
        nodes.iter().filter(|(_, n)| n["type"] == class).collect()
    };
    let repositories = of_type("CodeRepository");
    assert_eq!(repositories.len(), 1);
    let (repo_id, repository) = repositories[0];
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
            .filter_map(|(_, n)| n["name"].as_str().map(str::to_string))
            .collect()
    };
    assert!(
        edges
            .iter()
            .any(|(_, t, r, _)| r == "part_of" && t == repo_id),
        "no part_of edge points at the repository"
    );
    assert!(names("CodeModule").is_superset(&BTreeSet::from([".".into(), "inventory".into()])));
    let symbols: Vec<(String, String, String)> = of_type("CodeSymbol")
        .iter()
        .filter_map(|(_, n)| {
            Some((
                n["name"].as_str()?.to_string(),
                n["symbol_kind"].as_str()?.to_string(),
                n["file_path"].as_str().unwrap_or_default().to_string(),
            ))
        })
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
            symbols.iter().any(|(n, k, _)| n == name && k == kind),
            "missing symbol {name} ({kind})"
        );
    }
    let files: BTreeSet<&str> = symbols.iter().map(|(_, _, f)| f.as_str()).collect();
    for file in ["main.py", "inventory/pricing.py", "inventory/store.py"] {
        assert!(files.contains(file), "missing file {file}");
    }
    assert!(names("ExternalDependency").is_superset(&BTreeSet::from([
        "inventory/store -> requests".into(),
        "inventory/pricing -> math".into(),
        "inventory/store -> inventory.pricing".into(),
    ])));

    let ids: HashSet<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();
    assert!(
        edges
            .iter()
            .all(|(s, t, _, _)| ids.contains(s.as_str()) && ids.contains(t.as_str())),
        "dangling edges"
    );
    let name_of = |id: &str| -> String {
        nodes
            .iter()
            .find(|(node_id, _)| node_id == id)
            .and_then(|(_, n)| n["name"].as_str().map(str::to_string))
            .unwrap_or_default()
    };
    let relation = |rel: &str| -> BTreeSet<(String, String)> {
        edges
            .iter()
            .filter(|(_, _, r, _)| r == rel)
            .map(|(s, t, _, _)| (name_of(s), name_of(t)))
            .collect()
    };
    let calls = relation("calls");
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
    assert!(relation("declares").contains(&(
        "inventory/pricing.line_total".to_string(),
        "inventory".to_string()
    )));
    assert!(relation("imports").contains(&(
        "module-edge: . -> inventory".to_string(),
        "inventory".to_string()
    )));
    for (id, _) in &nodes {
        if id != repo_id {
            assert!(
                edges.iter().any(|(s, _, r, _)| s == id && r == "part_of"),
                "{} is not linked to its repository via part_of",
                name_of(id)
            );
        }
    }
}

// ─── Remote repositories (SDK-1053 O7) ────────────────────────────────────────

mod remote {
    use std::process::Command;

    use cognee_ingestion::AddParams;
    use cognee_ingestion::code_repo::code_repo_data_id;
    use cognee_ingestion::resolve_repo::{
        CodeRepositoryError, GitOutput, RepoSourceSettings, RepoTransport, SystemGit, clone_dir,
    };

    use super::*;

    const URL: &str = "https://github.com/org/shop";
    const TOKEN_URL: &str = "https://x-access-token:tok-SECRET@github.com/org/shop";

    fn git_installed() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    /// Git on a bare configuration (the developer's global config could
    /// sign or hook the commits made here).
    fn hermetic_env(root: &Path) -> Vec<(String, String)> {
        let config = root.join("gitconfig");
        if !config.exists() {
            std::fs::write(&config, "").unwrap();
        }
        vec![
            (
                "GIT_CONFIG_GLOBAL".to_string(),
                config.to_string_lossy().into_owned(),
            ),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
            ("GIT_AUTHOR_NAME".to_string(), "t".to_string()),
            ("GIT_AUTHOR_EMAIL".to_string(), "t@example.com".to_string()),
            ("GIT_COMMITTER_NAME".to_string(), "t".to_string()),
            (
                "GIT_COMMITTER_EMAIL".to_string(),
                "t@example.com".to_string(),
            ),
        ]
    }

    fn git(root: &Path, cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .envs(hermetic_env(root))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A bare origin holding the shop project on `main`, and on `dev` with
    /// one more module.
    fn origin(root: &Path) -> PathBuf {
        let work = project(root, "work", "snapshot:basic");
        let bare = root.join("origin.git");
        git(
            root,
            root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &bare.to_string_lossy(),
            ],
        );
        git(root, &work, &["init", "-q", "-b", "main"]);
        git(root, &work, &["add", "-A"]);
        git(root, &work, &["commit", "-q", "-m", "main"]);
        git(
            root,
            &work,
            &["push", "-q", &bare.to_string_lossy(), "main"],
        );
        git(root, &work, &["switch", "-q", "-c", "dev"]);
        std::fs::write(work.join("dev.py"), "def dev():\n    pass\n").unwrap();
        git(root, &work, &["add", "-A"]);
        git(root, &work, &["commit", "-q", "-m", "dev"]);
        git(root, &work, &["push", "-q", &bare.to_string_lossy(), "dev"]);
        bare
    }

    /// Real git with the github.com URL (with or without a token) mapped to
    /// the bare origin; the outbound check passes. Records every argv.
    struct LocalOrigin {
        remote: String,
        env: Vec<(String, String)>,
        argv: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl RepoTransport for LocalOrigin {
        async fn run_git(
            &self,
            args: &[String],
            cwd: Option<&Path>,
            env: &[(String, String)],
        ) -> Result<GitOutput, CodeRepositoryError> {
            self.argv.lock().unwrap().push(args.join(" "));
            let args: Vec<String> = args
                .iter()
                .map(|arg| {
                    if arg == URL || arg == TOKEN_URL {
                        self.remote.clone()
                    } else {
                        arg.clone()
                    }
                })
                .collect();
            let mut env = env.to_vec();
            env.extend(self.env.iter().cloned());
            SystemGit.run_git(&args, cwd, &env).await
        }

        async fn check_outbound_url(&self, _url: &str) -> Result<(), String> {
            Ok(())
        }
    }

    struct Remote {
        _root: TempDir,
        clones: PathBuf,
        transport: Arc<LocalOrigin>,
    }

    impl Remote {
        fn new() -> Option<Self> {
            if !git_installed() {
                eprintln!("skipped: git is required");
                return None;
            }
            let root = TempDir::new().unwrap();
            let base = root.path().canonicalize().unwrap();
            let bare = origin(&base);
            Some(Self {
                clones: base.join("clones"),
                transport: Arc::new(LocalOrigin {
                    remote: format!("file://{}", bare.to_string_lossy()),
                    env: hermetic_env(&base),
                    argv: std::sync::Mutex::new(Vec::new()),
                }),
                _root: root,
            })
        }

        fn wire(&self, pipeline: AddPipeline) -> AddPipeline {
            pipeline
                .with_repo_sources(RepoSourceSettings::new(
                    &self.clones.to_string_lossy(),
                    "",
                    false,
                ))
                .with_repo_transport(Arc::clone(&self.transport) as Arc<dyn RepoTransport>)
        }
    }

    fn with_pipeline(
        params: RememberCodeParams,
        f: impl FnOnce(AddPipeline) -> AddPipeline,
    ) -> RememberCodeParams {
        let pipeline = Arc::try_unwrap(params.add_pipeline)
            .ok()
            .expect("the only handle");
        RememberCodeParams {
            add_pipeline: Arc::new(f(pipeline)),
            ..params
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_remote_repository_is_cloned_and_remembered_without_its_token() {
        let Some(remote) = Remote::new() else { return };
        let stack = Stack::new().await;
        let mut params = with_pipeline(
            stack.params(vec![DataInput::Url(TOKEN_URL.to_string())], true),
            |p| remote.wire(p),
        );
        params.repo_credentials = Some("oob-SECRET".to_string());

        let result = stack.remember(params).await.unwrap();
        assert_eq!(result.status, RememberStatus::Completed, "{result}");
        let clone = clone_dir(URL, None, &remote.clones);
        let item = &result.items[0];
        assert_eq!(item.source.as_deref(), Some(URL));
        assert_eq!(item.path.as_deref(), Some(clone.to_string_lossy().as_ref()));
        assert!(clone.join("main.py").is_file());
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("SECRET"), "{json}");

        // The row records the credential-free URL and is pinned to the clone.
        let data_id = item.id.unwrap();
        let dataset_id = stack.dataset_id().await;
        assert_eq!(
            data_id,
            code_repo_data_id(&clone.to_string_lossy(), stack.owner_id, None, dataset_id)
        );
        let row = get_data(&stack.db, data_id).await.unwrap().unwrap();
        let stamp: Value = serde_json::from_str(row.system_metadata.as_deref().unwrap()).unwrap();
        assert_eq!(stamp["repo_url"], URL);
        assert_eq!(stamp["repo_path"], clone.to_string_lossy().as_ref());
        assert!(stack.node_types().await.contains("CodeRepository"));

        // The out-of-band token never reached argv; the stored remote is clean.
        let argv = remote.transport.argv.lock().unwrap().join("\n");
        assert!(!argv.contains("oob-SECRET"), "{argv}");
        // (The transport maps the URL itself onto the local origin, so the
        // rewrite is read off the argv rather than the clone's config.)
        assert!(
            argv.lines()
                .any(|line| line == format!("remote set-url origin {URL}")),
            "{argv}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_disabled_gate_refuses_before_anything_is_cloned_or_stored() {
        let Some(remote) = Remote::new() else { return };
        let stack = Stack::new().await;
        let params = with_pipeline(
            stack.params(vec![DataInput::Url(URL.to_string())], false),
            |p| {
                remote.wire(p).with_repo_sources(RepoSourceSettings::new(
                    &remote.clones.to_string_lossy(),
                    "off",
                    false,
                ))
            },
        );
        let error = stack.remember(params).await.unwrap_err();
        assert!(
            matches!(&error, ApiError::CodeRepositoryForbidden(m)
                if m.contains("ALLOW_HTTP_REQUESTS=off")),
            "{error}"
        );
        assert!(remote.transport.argv.lock().unwrap().is_empty());
        assert!(
            get_dataset_by_name_in_tenant(&stack.db, DATASET, stack.owner_id, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    fn add_pipeline(stack: &Stack, remote: &Remote) -> AddPipeline {
        remote.wire(
            AddPipeline::new(
                Arc::clone(&stack.storage),
                Arc::clone(&stack.db) as Arc<dyn IngestDb>,
            )
            .with_thread_pool(Arc::new(
                cognee::core::RayonThreadPool::with_default_threads().unwrap(),
            ))
            .with_graph_db(Arc::clone(&stack.graph_db) as Arc<dyn GraphDBTrait>)
            .with_vector_db(Arc::clone(&stack.vector_db) as Arc<dyn VectorDB>)
            .with_database(stack.db.clone()),
        )
    }

    /// Python `add(<GitHub URL>, branch=...)`: the clone is resolved like a
    /// local project (one manifest plus its documents), and every branch of
    /// the URL shares the manifest's one row.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_clones_a_repository_url_on_its_branch() {
        let Some(remote) = Remote::new() else { return };
        let stack = Stack::new().await;
        let pipeline = add_pipeline(&stack, &remote);
        let add = |branch: Option<&str>| {
            let params = AddParams {
                branch: branch.map(str::to_string),
                ..AddParams::default()
            };
            let pipeline = &pipeline;
            let owner = stack.owner_id;
            async move {
                pipeline
                    .add_with_params(
                        vec![DataInput::Url(format!("{URL}/?tab=readme#top"))],
                        DATASET,
                        owner,
                        None,
                        &params,
                    )
                    .await
                    .unwrap()
            }
        };

        let rows = add(Some("dev")).await;
        let dataset_id = stack.dataset_id().await;
        let manifests: Vec<_> = rows
            .iter()
            .filter(|row| cognify_route_for(row) == Ok(CognifyRoute::CodeRepo))
            .collect();
        assert_eq!(manifests.len(), 1, "{rows:?}");
        assert!(
            rows.iter().any(|row| row.name.starts_with("README")),
            "the README is a document: {rows:?}"
        );
        let manifest = manifests[0];
        let default_clone = clone_dir(URL, None, &remote.clones);
        let dev_clone = clone_dir(URL, Some("dev"), &remote.clones);
        assert!(dev_clone.join("dev.py").is_file());
        let stamp: Value =
            serde_json::from_str(manifest.system_metadata.as_deref().unwrap()).unwrap();
        assert_eq!(stamp["repo_url"], URL);
        assert_eq!(stamp["repo_path"], dev_clone.to_string_lossy().as_ref());
        // Keyed on the default branch's clone, as Python's `identity`.
        assert_eq!(
            manifest.id,
            code_repo_data_id(
                &default_clone.to_string_lossy(),
                stack.owner_id,
                None,
                dataset_id
            )
        );

        // The default branch: its own clone, the same row.
        let rows = add(None).await;
        let manifest_main = rows
            .iter()
            .find(|row| cognify_route_for(row) == Ok(CognifyRoute::CodeRepo))
            .unwrap();
        assert_eq!(manifest_main.id, manifest.id);
        assert!(default_clone.join("main.py").is_file());
        assert!(!default_clone.join("dev.py").exists());
        let stamp: Value =
            serde_json::from_str(manifest_main.system_metadata.as_deref().unwrap()).unwrap();
        assert_eq!(stamp["repo_path"], default_clone.to_string_lossy().as_ref());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_refuses_a_bad_branch_before_anything_is_stored() {
        let Some(remote) = Remote::new() else { return };
        let stack = Stack::new().await;
        let params = AddParams {
            branch: Some("--upload-pack=touch /tmp/x".to_string()),
            ..AddParams::default()
        };
        let error = add_pipeline(&stack, &remote)
            .add_with_params(
                vec![DataInput::Url(URL.to_string())],
                DATASET,
                stack.owner_id,
                None,
                &params,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Invalid branch name '--upload-pack"),
            "{error}"
        );
        assert!(remote.transport.argv.lock().unwrap().is_empty());
        assert!(
            get_dataset_by_name_in_tenant(&stack.db, DATASET, stack.owner_id, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_of_a_repository_url_follows_the_gate() {
        let Some(remote) = Remote::new() else { return };
        let stack = Stack::new().await;
        let pipeline = add_pipeline(&stack, &remote).with_repo_sources(RepoSourceSettings::new(
            &remote.clones.to_string_lossy(),
            "",
            true,
        ));
        let error = pipeline
            .add(
                vec![DataInput::Url(URL.to_string())],
                DATASET,
                stack.owner_id,
                None,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Outbound HTTP requests are disabled by default"),
            "{error}"
        );
        assert!(remote.transport.argv.lock().unwrap().is_empty());
    }
}

// ---------------------------------------------------------------------------
// A dataset's repositories as one enola cluster (SDK-1053 O8)
// ---------------------------------------------------------------------------

/// The `pipeline_status` of a row.
async fn pipeline_status(stack: &Stack, data_id: Uuid) -> Value {
    get_data(&stack.db, data_id)
        .await
        .unwrap()
        .unwrap()
        .pipeline_status
        .map_or(Value::Null, |text| serde_json::from_str(&text).unwrap())
}

fn built(status: &Value, dataset_id: Uuid) -> bool {
    ["cognify_pipeline", "code_graph_pipeline"]
        .iter()
        .all(|name| {
            status[*name][dataset_id.to_string()] == json!("DATA_ITEM_PROCESSING_COMPLETED")
        })
}

/// The distinct `code_graph_pipeline` runs of the stack's dataset.
async fn code_graph_run_ids(stack: &Stack) -> BTreeSet<Uuid> {
    stack
        .run_rows()
        .await
        .into_iter()
        .filter(|(name, _, _)| name == "code_graph_pipeline")
        .map(|(_, _, run_id)| run_id)
        .collect()
}

fn reaches_route() -> (String, String, String) {
    (
        cognee_code_graph::fact_node_id("ui", "route", "/api/rounds").to_string(),
        cognee_code_graph::fact_node_id("golf", "route", "/api/rounds").to_string(),
        "reaches_route".to_string(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn two_repositories_are_remembered_as_one_cluster_with_edges_between_them() {
    let root = TempDir::new().unwrap();
    let golf = project(root.path(), "golf", "cluster:cluster");
    let ui = project(root.path(), "ui", "cluster:cluster");
    let stack = Stack::new().await;

    let result = stack
        .remember(stack.params(vec![path_input(&golf), path_input(&ui)], true))
        .await
        .unwrap();

    assert_eq!(result.status, RememberStatus::Completed, "{result}");
    assert_eq!(result.items_processed, 2);
    let dataset_id = stack.dataset_id().await;
    let output_dir = cognee_code_graph::cluster_output_dir(dataset_id);
    // One enola run, over both, into the dataset's own directory.
    assert!(golf.join(&output_dir).join("cluster-config.yaml").is_file());
    assert!(!ui.join(&output_dir).join("cluster-config.yaml").exists());
    assert!(!golf.join(".enola").exists() && !ui.join(".enola").exists());
    assert!(graph_shape(&stack).await.1.contains(&reaches_route()));
    // One code_graph_pipeline run per repository, each over its own row.
    let run_ids: BTreeSet<Uuid> = result
        .items
        .iter()
        .map(|item| item.pipeline_run_id.unwrap())
        .collect();
    assert_eq!(run_ids.len(), 2);
    for item in &result.items {
        assert!(built(
            &pipeline_status(&stack, item.id.unwrap()).await,
            dataset_id
        ));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_repository_is_built_together_with_the_ones_already_in_the_dataset() {
    let root = TempDir::new().unwrap();
    let golf = project(root.path(), "golf", "cluster:cluster");
    let ui = project(root.path(), "ui", "cluster:cluster");
    let stack = Stack::new().await;
    let first = stack
        .remember(stack.params(vec![path_input(&golf)], true))
        .await
        .unwrap();
    let golf_id = first.items[0].id.unwrap();
    // Alone in its dataset: the per-repository run.
    assert!(golf.join(".enola").is_dir());

    let second = stack
        .remember(stack.params(vec![path_input(&ui)], true))
        .await
        .unwrap();

    assert_eq!(second.status, RememberStatus::Completed, "{second}");
    // Only this call's repository is in its result, but golf was rebuilt in
    // the cluster too: the edge between them is there, and golf's own run
    // ran under golf's row.
    assert_eq!(second.items.len(), 1);
    assert!(graph_shape(&stack).await.1.contains(&reaches_route()));
    assert_eq!(
        code_graph_run_ids(&stack).await.len(),
        3,
        "golf, then golf and ui"
    );
    let dataset_id = stack.dataset_id().await;
    assert!(built(&pipeline_status(&stack, golf_id).await, dataset_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rebuilt_repository_that_fails_is_reported_not_raised() {
    let root = TempDir::new().unwrap();
    // Both carry the label `acme/shop`: not a cluster, each runs on its own.
    let shop = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;
    stack
        .remember(stack.params(vec![path_input(&shop)], true))
        .await
        .unwrap();
    std::fs::write(shop.join(".fake-enola"), "fail-single:basic").unwrap();
    let store = project(root.path(), "store", "snapshot:basic");

    let result = stack
        .remember(stack.params(vec![path_input(&store)], true))
        .await
        .expect("another repository's failure is never raised");

    assert_eq!(result.status, RememberStatus::Errored, "{result}");
    assert_eq!(result.items_processed, 1);
    assert_eq!(result.items[0].status, None);
    assert!(result.items[0].pipeline_run_id.is_some());
    let error = result.error.unwrap();
    assert!(error.starts_with("shop: "), "{error}");
    assert!(error.contains("exited with code 3"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn past_the_inline_limit_the_other_repositories_are_left_to_the_next_cognify() {
    let root = TempDir::new().unwrap();
    let stack = Stack::new().await;
    // Six repositories that are no cluster (one shared label): each run on
    // its own.
    let mut existing = Vec::new();
    for index in 0..=cognee_code_graph::CLUSTER_INLINE_REBUILD_LIMIT {
        let repo = project(root.path(), &format!("svc{index}"), "snapshot:basic");
        let result = stack
            .remember(stack.params(vec![path_input(&repo)], true))
            .await
            .unwrap();
        existing.push(result.items[0].id.unwrap());
    }
    let runs_before = code_graph_run_ids(&stack).await.len();
    let fresh = project(root.path(), "fresh", "snapshot:basic");

    let result = stack
        .remember(stack.params(vec![path_input(&fresh)], true))
        .await
        .unwrap();

    assert_eq!(result.status, RememberStatus::Completed, "{result}");
    let dataset_id = stack.dataset_id().await;
    assert!(built(
        &pipeline_status(&stack, result.items[0].id.unwrap()).await,
        dataset_id
    ));
    // The others were not rebuilt, only marked: no stamps left.
    for id in existing {
        let status = pipeline_status(&stack, id).await;
        assert!(
            status["cognify_pipeline"]
                .get(dataset_id.to_string())
                .is_none()
                && status["code_graph_pipeline"]
                    .get(dataset_id.to_string())
                    .is_none(),
            "{status}"
        );
    }
    assert_eq!(
        code_graph_run_ids(&stack).await.len(),
        runs_before + 1,
        "one more run, for the new repository only"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repository_whose_directory_is_gone_is_left_out_of_the_rebuild() {
    let root = TempDir::new().unwrap();
    let golf = project(root.path(), "golf", "cluster:cluster");
    let ui = project(root.path(), "ui", "cluster:cluster");
    let stack = Stack::new().await;
    stack
        .remember(stack.params(vec![path_input(&golf)], true))
        .await
        .unwrap();
    std::fs::remove_dir_all(&golf).unwrap();

    let result = stack
        .remember(stack.params(vec![path_input(&ui)], true))
        .await
        .expect("a stale row never fails a later remember");

    // Python's `test_refresh_leaves_out_a_repository_whose_directory_is_gone`:
    // with one repository left there is no cluster, and the gone one is
    // neither a member nor rebuilt on its own.
    assert_eq!(result.status, RememberStatus::Completed, "{result}");
    let dataset_id = stack.dataset_id().await;
    assert!(ui.join(".enola").is_dir());
    assert!(
        !ui.join(cognee_code_graph::cluster_output_dir(dataset_id))
            .exists()
    );
    assert_eq!(
        code_graph_run_ids(&stack).await.len(),
        2,
        "golf once, then ui alone"
    );
}

// ---------------------------------------------------------------------------
// index_vectors (SDK-1053 O10)
// ---------------------------------------------------------------------------

/// An embedding engine that fails the test on any call: the default
/// (graph-only) path must never reach it.
struct PanickingEmbedder;

#[async_trait::async_trait]
impl cognee_embedding::EmbeddingEngine for PanickingEmbedder {
    async fn embed(&self, _: &[&str]) -> cognee_embedding::EmbeddingResult<Vec<Vec<f32>>> {
        panic!("remember(content_type=\"code\") embedded without index_vectors");
    }
    fn dimension(&self) -> usize {
        8
    }
    fn batch_size(&self) -> usize {
        8
    }
    fn max_sequence_length(&self) -> usize {
        512
    }
}

/// The ids of the graph's nodes of `class`.
async fn node_ids_of(stack: &Stack, class: &str) -> Vec<Uuid> {
    stack
        .nodes()
        .await
        .into_iter()
        .filter(|(_, node)| node.get("type").and_then(Value::as_str) == Some(class))
        .map(|(id, _)| Uuid::parse_str(&id).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_makes_no_embedding_call() {
    let root = TempDir::new().unwrap();
    let shop = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;
    let mut params = stack.params(vec![path_input(&shop)], true);
    // Given an engine, but not asked to index: it must not be called.
    params.embedding_engine = Some(Arc::new(PanickingEmbedder));
    let result = stack.remember(params).await.unwrap();
    assert_eq!(result.status, RememberStatus::Completed);
    assert!(!node_ids_of(&stack, "CodeRepository").await.is_empty());
    assert!(stack.vector_db.list_collections().await.unwrap().is_empty());
    assert_eq!(stack.vector_db.index_points_call_count(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn index_vectors_embeds_the_code_nodes_and_their_edges() {
    let root = TempDir::new().unwrap();
    let shop = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;
    let mut params = stack.params(vec![path_input(&shop)], true);
    params.index_vectors = true;
    let engine = Arc::new(cognee_embedding::MockEmbeddingEngine::new(8));
    params.embedding_engine =
        Some(Arc::clone(&engine) as Arc<dyn cognee_embedding::EmbeddingEngine>);
    let result = stack.remember(params).await.unwrap();
    assert_eq!(result.status, RememberStatus::Completed);
    let dataset_id = stack.dataset_id().await;

    let collections: BTreeSet<(String, String)> = stack
        .vector_db
        .list_collections()
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert!(collections.contains(&("CodeRepository".into(), "name".into())));
    assert!(collections.contains(&("EdgeType".into(), "relationship_name".into())));
    assert!(!collections.iter().any(|(class, _)| class == "NodeSet"));

    // Every code node written has its `name` vector, under its own class.
    let mut indexed = 0;
    for (id, node) in stack.nodes().await {
        let class = node
            .get("type")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let id = Uuid::parse_str(&id).unwrap();
        let payload = stack
            .vector_db
            .get_payload(&class, "name", id)
            .unwrap_or_else(|| panic!("no {class}_name vector for {id}"));
        assert_eq!(payload["text"], node["name"]);
        assert_eq!(payload["id"], json!(id.to_string()));
        assert_eq!(payload["type"], json!("IndexSchema"));
        assert_eq!(payload["dataset_id"], json!(dataset_id.to_string()));
        indexed += 1;
    }
    assert!(indexed > 1);
    assert!(engine.call_count() > 0);

    // The structural `part_of` edges' retrieval texts are edge types.
    let (_, edges) = stack.graph_db.get_graph_data().await.unwrap();
    let part_of = edges
        .iter()
        .find(|(_, _, relationship, _)| relationship == "part_of")
        .expect("a part_of edge");
    let text = part_of.3["edge_text"].as_str().unwrap();
    let edge_type_id = cognee_models::EdgeType::deterministic_id(text);
    let payload = stack
        .vector_db
        .get_payload("EdgeType", "relationship_name", edge_type_id)
        .expect("the part_of edge text is indexed");
    assert_eq!(payload["text"], json!(text));
}

#[tokio::test(flavor = "multi_thread")]
async fn index_vectors_without_an_engine_is_refused() {
    let root = TempDir::new().unwrap();
    let shop = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;
    let mut params = stack.params(vec![path_input(&shop)], true);
    params.index_vectors = true;
    let error = stack.remember(params).await.unwrap_err();
    assert!(
        matches!(&error, ApiError::InvalidArgument(m) if m.contains("embedding engine")),
        "{error:?}"
    );
    assert!(node_ids_of(&stack, "CodeRepository").await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn index_vectors_applies_to_this_calls_repositories_only() {
    let root = TempDir::new().unwrap();
    let stack = Stack::new().await;
    let shop = project(root.path(), "shop", "snapshot:basic");
    stack
        .remember(stack.params(vec![path_input(&shop)], true))
        .await
        .unwrap();
    let first: HashSet<Uuid> = node_ids_of(&stack, "CodeRepository")
        .await
        .into_iter()
        .collect();

    // A second repository, with index_vectors: the first one is rebuilt with
    // it (its repository node is always rewritten) but not embedded.
    let other = project(root.path(), "other", "snapshot:docs_and_flows");
    let mut params = stack.params(vec![path_input(&other)], true);
    params.index_vectors = true;
    params.embedding_engine = Some(Arc::new(cognee_embedding::MockEmbeddingEngine::new(8)));
    let result = stack.remember(params).await.unwrap();
    assert_eq!(result.status, RememberStatus::Completed, "{result:?}");

    let repositories = node_ids_of(&stack, "CodeRepository").await;
    let mut embedded = 0;
    for id in repositories {
        let payload = stack.vector_db.get_payload("CodeRepository", "name", id);
        if first.contains(&id) {
            assert!(
                payload.is_none(),
                "the earlier repository {id} was embedded"
            );
        } else {
            assert!(
                payload.is_some(),
                "the new repository {id} was not embedded"
            );
            embedded += 1;
        }
    }
    assert!(embedded > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn forgetting_an_indexed_repository_removes_its_vectors() {
    use cognee::api::forget::{DatasetRef, ForgetTarget, forget};
    use cognee::delete::DeleteService;
    use cognee_database::DeleteDb;

    let root = TempDir::new().unwrap();
    let shop = project(root.path(), "shop", "snapshot:basic");
    let stack = Stack::new().await;
    let mut params = stack.params(vec![path_input(&shop)], true);
    params.index_vectors = true;
    params.embedding_engine = Some(Arc::new(cognee_embedding::MockEmbeddingEngine::new(8)));
    let result = stack.remember(params).await.unwrap();
    let data_id = result.items[0].id.unwrap();
    let repositories = node_ids_of(&stack, "CodeRepository").await;
    assert!(
        stack
            .vector_db
            .get_payload("CodeRepository", "name", repositories[0])
            .is_some()
    );

    let service = DeleteService::new(
        Arc::clone(&stack.storage),
        Arc::clone(&stack.db) as Arc<dyn DeleteDb>,
    )
    .with_graph_db(Arc::clone(&stack.graph_db) as Arc<dyn GraphDBTrait>)
    .with_vector_db(Arc::clone(&stack.vector_db) as Arc<dyn VectorDB>);
    let dataset_id = stack.dataset_id().await;
    forget(
        ForgetTarget::Item {
            data_id,
            dataset: DatasetRef::Id(dataset_id),
        },
        stack.owner_id,
        None,
        &service,
        Some(stack.db.as_ref() as &dyn IngestDb),
    )
    .await
    .expect("forget the repository");

    assert!(
        stack
            .vector_db
            .get_payload("CodeRepository", "name", repositories[0])
            .is_none()
    );
}
