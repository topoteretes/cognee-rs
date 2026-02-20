use assert_cmd::Command;
use chrono::{DateTime, Utc};
use cognee_lib::database::{ArtifactReference, DatabaseTrait, SqliteDatabase};
use cognee_lib::graph::{GraphDBTrait, LadybugAdapter};
use cognee_lib::vector::{QdrantAdapter, VectorDB, VectorPoint};
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

#[derive(serde::Serialize)]
struct TestGraphNode {
    id: String,
    name: String,
    data_type: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

struct DeleteFixture {
    target_data_id: Uuid,
    keep_data_id: Uuid,
    target_graph_node_id: String,
    keep_graph_node_id: String,
    target_vector_point_id: Uuid,
    keep_vector_point_id: Uuid,
}

fn make_cmd(config_home: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"));
    command.env("XDG_CONFIG_HOME", config_home.path());
    command
}

fn make_cmd_in(config_home: &TempDir, workdir: &Path) -> Command {
    let mut command = make_cmd(config_home);
    command.current_dir(workdir);
    command
}

fn config_set(config_home: &TempDir, workdir: &Path, key: &str, json_value: &str) {
    make_cmd_in(config_home, workdir)
        .args(["config", "set", key, json_value])
        .assert()
        .success();
}

fn seed_scoped_delete_fixture(
    db_url: &str,
    owner_id: Uuid,
    target_dataset_name: &str,
    keep_dataset_name: &str,
    graph_path: &Path,
    vector_path: &Path,
) -> DeleteFixture {
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    runtime.block_on(async {
        let database = SqliteDatabase::new(db_url)
            .await
            .expect("sqlite should initialize");
        database
            .initialize()
            .await
            .expect("schema should initialize");

        let datasets = database
            .list_datasets_by_owner(owner_id)
            .await
            .expect("datasets should load");
        let target_dataset = datasets
            .iter()
            .find(|dataset| dataset.name == target_dataset_name)
            .expect("target dataset should exist");
        let keep_dataset = datasets
            .iter()
            .find(|dataset| dataset.name == keep_dataset_name)
            .expect("keep dataset should exist");

        let target_data = database
            .get_dataset_data(target_dataset.id)
            .await
            .expect("target dataset data should load")
            .into_iter()
            .next()
            .expect("target data should exist");
        let keep_data = database
            .get_dataset_data(keep_dataset.id)
            .await
            .expect("keep dataset data should load")
            .into_iter()
            .next()
            .expect("keep data should exist");

        let graph_db = LadybugAdapter::new(graph_path.to_str().expect("valid graph path"))
            .await
            .expect("graph db should initialize");
        graph_db
            .initialize()
            .await
            .expect("graph schema should initialize");

        let target_graph_node_id = Uuid::new_v4().to_string();
        let keep_graph_node_id = Uuid::new_v4().to_string();
        let now = Utc::now();

        let target_node = TestGraphNode {
            id: target_graph_node_id.clone(),
            name: "target-node".to_string(),
            data_type: "Entity".to_string(),
            created_at: now,
            updated_at: now,
        };
        let keep_node = TestGraphNode {
            id: keep_graph_node_id.clone(),
            name: "keep-node".to_string(),
            data_type: "Entity".to_string(),
            created_at: now,
            updated_at: now,
        };

        graph_db
            .add_node(&target_node)
            .await
            .expect("target graph node should be added");
        graph_db
            .add_node(&keep_node)
            .await
            .expect("keep graph node should be added");

        let vector_db = QdrantAdapter::new(vector_path.to_path_buf(), 2);
        if !vector_db
            .has_collection("Entity", "name")
            .await
            .expect("collection existence should be checked")
        {
            vector_db
                .create_collection("Entity", "name", 2)
                .await
                .expect("vector collection should be created");
        }

        let target_vector_point_id = Uuid::from_u128(101);
        let keep_vector_point_id = Uuid::from_u128(202);
        let points = vec![
            VectorPoint::new(target_vector_point_id, vec![1.0, 0.0]),
            VectorPoint::new(keep_vector_point_id, vec![0.0, 1.0]),
        ];
        vector_db
            .index_points("Entity", "name", &points)
            .await
            .expect("vector points should be indexed");

        let references = vec![
            ArtifactReference {
                id: Uuid::new_v4(),
                owner_id,
                dataset_id: target_dataset.id,
                data_id: Some(target_data.id),
                artifact_kind: "graph_node".to_string(),
                artifact_id: target_graph_node_id.clone(),
                collection_name: None,
                created_at: Utc::now(),
            },
            ArtifactReference {
                id: Uuid::new_v4(),
                owner_id,
                dataset_id: target_dataset.id,
                data_id: Some(target_data.id),
                artifact_kind: "vector_point".to_string(),
                artifact_id: target_vector_point_id.to_string(),
                collection_name: Some("Entity_name".to_string()),
                created_at: Utc::now(),
            },
            ArtifactReference {
                id: Uuid::new_v4(),
                owner_id,
                dataset_id: keep_dataset.id,
                data_id: Some(keep_data.id),
                artifact_kind: "graph_node".to_string(),
                artifact_id: keep_graph_node_id.clone(),
                collection_name: None,
                created_at: Utc::now(),
            },
            ArtifactReference {
                id: Uuid::new_v4(),
                owner_id,
                dataset_id: keep_dataset.id,
                data_id: Some(keep_data.id),
                artifact_kind: "vector_point".to_string(),
                artifact_id: keep_vector_point_id.to_string(),
                collection_name: Some("Entity_name".to_string()),
                created_at: Utc::now(),
            },
        ];

        database
            .upsert_artifact_references(&references)
            .await
            .expect("artifact references should be persisted");

        DeleteFixture {
            target_data_id: target_data.id,
            keep_data_id: keep_data.id,
            target_graph_node_id,
            keep_graph_node_id,
            target_vector_point_id,
            keep_vector_point_id,
        }
    })
}

fn seed_cross_owner_delete_fixture(
    db_url: &str,
    target_owner_id: Uuid,
    target_dataset_name: &str,
    keep_owner_id: Uuid,
    keep_dataset_name: &str,
    graph_path: &Path,
    vector_path: &Path,
) -> DeleteFixture {
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    runtime.block_on(async {
        let database = SqliteDatabase::new(db_url)
            .await
            .expect("sqlite should initialize");
        database
            .initialize()
            .await
            .expect("schema should initialize");

        let target_dataset = database
            .list_datasets_by_owner(target_owner_id)
            .await
            .expect("target owner datasets should load")
            .into_iter()
            .find(|dataset| dataset.name == target_dataset_name)
            .expect("target dataset should exist");
        let keep_dataset = database
            .list_datasets_by_owner(keep_owner_id)
            .await
            .expect("keep owner datasets should load")
            .into_iter()
            .find(|dataset| dataset.name == keep_dataset_name)
            .expect("keep dataset should exist");

        let target_data = database
            .get_dataset_data(target_dataset.id)
            .await
            .expect("target dataset data should load")
            .into_iter()
            .next()
            .expect("target data should exist");
        let keep_data = database
            .get_dataset_data(keep_dataset.id)
            .await
            .expect("keep dataset data should load")
            .into_iter()
            .next()
            .expect("keep data should exist");

        let graph_db = LadybugAdapter::new(graph_path.to_str().expect("valid graph path"))
            .await
            .expect("graph db should initialize");
        graph_db
            .initialize()
            .await
            .expect("graph schema should initialize");

        let target_graph_node_id = Uuid::new_v4().to_string();
        let keep_graph_node_id = Uuid::new_v4().to_string();
        let now = Utc::now();

        graph_db
            .add_node(&TestGraphNode {
                id: target_graph_node_id.clone(),
                name: "target-owner-node".to_string(),
                data_type: "Entity".to_string(),
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("target graph node should be added");
        graph_db
            .add_node(&TestGraphNode {
                id: keep_graph_node_id.clone(),
                name: "keep-owner-node".to_string(),
                data_type: "Entity".to_string(),
                created_at: now,
                updated_at: now,
            })
            .await
            .expect("keep graph node should be added");

        let vector_db = QdrantAdapter::new(vector_path.to_path_buf(), 2);
        if !vector_db
            .has_collection("Entity", "name")
            .await
            .expect("collection existence should be checked")
        {
            vector_db
                .create_collection("Entity", "name", 2)
                .await
                .expect("vector collection should be created");
        }

        let target_vector_point_id = Uuid::from_u128(303);
        let keep_vector_point_id = Uuid::from_u128(404);
        vector_db
            .index_points(
                "Entity",
                "name",
                &[
                    VectorPoint::new(target_vector_point_id, vec![1.0, 0.0]),
                    VectorPoint::new(keep_vector_point_id, vec![0.0, 1.0]),
                ],
            )
            .await
            .expect("vector points should be indexed");

        let references = vec![
            ArtifactReference {
                id: Uuid::new_v4(),
                owner_id: target_owner_id,
                dataset_id: target_dataset.id,
                data_id: Some(target_data.id),
                artifact_kind: "graph_node".to_string(),
                artifact_id: target_graph_node_id.clone(),
                collection_name: None,
                created_at: Utc::now(),
            },
            ArtifactReference {
                id: Uuid::new_v4(),
                owner_id: target_owner_id,
                dataset_id: target_dataset.id,
                data_id: Some(target_data.id),
                artifact_kind: "vector_point".to_string(),
                artifact_id: target_vector_point_id.to_string(),
                collection_name: Some("Entity_name".to_string()),
                created_at: Utc::now(),
            },
            ArtifactReference {
                id: Uuid::new_v4(),
                owner_id: keep_owner_id,
                dataset_id: keep_dataset.id,
                data_id: Some(keep_data.id),
                artifact_kind: "graph_node".to_string(),
                artifact_id: keep_graph_node_id.clone(),
                collection_name: None,
                created_at: Utc::now(),
            },
            ArtifactReference {
                id: Uuid::new_v4(),
                owner_id: keep_owner_id,
                dataset_id: keep_dataset.id,
                data_id: Some(keep_data.id),
                artifact_kind: "vector_point".to_string(),
                artifact_id: keep_vector_point_id.to_string(),
                collection_name: Some("Entity_name".to_string()),
                created_at: Utc::now(),
            },
        ];

        database
            .upsert_artifact_references(&references)
            .await
            .expect("artifact references should be persisted");

        DeleteFixture {
            target_data_id: target_data.id,
            keep_data_id: keep_data.id,
            target_graph_node_id,
            keep_graph_node_id,
            target_vector_point_id,
            keep_vector_point_id,
        }
    })
}

fn assert_scoped_delete_results(
    db_url: &str,
    graph_path: &Path,
    vector_path: &Path,
    fixture: &DeleteFixture,
) {
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    runtime.block_on(async {
        let database = SqliteDatabase::new(db_url)
            .await
            .expect("sqlite should initialize");
        database
            .initialize()
            .await
            .expect("schema should initialize");

        let deleted_data = database
            .get_data(fixture.target_data_id)
            .await
            .expect("target data query should succeed");
        assert!(deleted_data.is_none(), "target data should be deleted");

        let remaining_data = database
            .get_data(fixture.keep_data_id)
            .await
            .expect("keep data query should succeed");
        assert!(remaining_data.is_some(), "keep data should remain");

        let graph_db = LadybugAdapter::new(graph_path.to_str().expect("valid graph path"))
            .await
            .expect("graph db should initialize");
        graph_db
            .initialize()
            .await
            .expect("graph schema should initialize");

        let deleted_node = graph_db
            .get_node(&fixture.target_graph_node_id)
            .await
            .expect("target graph node query should succeed");
        assert!(
            deleted_node.is_none(),
            "target graph node should be deleted"
        );

        let remaining_node = graph_db
            .get_node(&fixture.keep_graph_node_id)
            .await
            .expect("keep graph node query should succeed");
        assert!(remaining_node.is_some(), "keep graph node should remain");

        let vector_db = QdrantAdapter::new(vector_path.to_path_buf(), 2);
        let size = vector_db
            .collection_size("Entity", "name")
            .await
            .expect("vector collection size should be available");
        assert_eq!(size, 1, "only one vector point should remain");

        let results = vector_db
            .search_similar("Entity", "name", &[0.0, 1.0], 10)
            .await
            .expect("vector search should succeed");

        assert!(
            !results
                .iter()
                .any(|result| result.id == fixture.target_vector_point_id),
            "target vector point should be deleted"
        );
        assert!(
            results
                .iter()
                .any(|result| result.id == fixture.keep_vector_point_id),
            "keep vector point should remain"
        );
    });
}

#[test]
fn config_set_get_roundtrip_chunk_size() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["config", "set", "chunk_size", "2048"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Success: Set chunk_size"));

    make_cmd(&config_home)
        .args(["config", "get", "chunk_size"])
        .assert()
        .success()
        .stdout(predicate::str::contains("chunk_size: 2048"));
}

#[test]
fn config_list_contains_expected_keys() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["config", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("llm_provider"))
        .stdout(predicate::str::contains("default_user_id"))
        .stdout(predicate::str::contains("default_system_prompt_path"));
}

#[test]
fn config_unset_restores_default_llm_provider() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["config", "set", "llm_provider", "\"custom\""])
        .assert()
        .success();

    make_cmd(&config_home)
        .args(["config", "unset", "llm_provider", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Success: Unset llm_provider"));

    make_cmd(&config_home)
        .args(["config", "get", "llm_provider"])
        .assert()
        .success()
        .stdout(predicate::str::contains("llm_provider: \"openai\""));
}

#[test]
fn search_rejects_invalid_top_k() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["search", "test", "--top-k", "0"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--top-k must be between 1 and 100",
        ));
}

#[test]
fn delete_rejects_missing_scope() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["delete"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Specify exactly one delete scope"));
}

#[test]
fn add_fails_fast_on_invalid_configured_default_user_id() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["config", "set", "default_user_id", "\"not-a-uuid\""])
        .assert()
        .success();

    make_cmd(&config_home)
        .args(["add", "hello"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Invalid default_user_id"));
}

#[test]
fn add_succeeds_with_local_temp_paths() {
    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        "\"00000000-0000-0000-0000-000000000000\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "data_root_directory",
        "\"./cognee_data\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        "\"sqlite::memory:\"",
    );

    make_cmd_in(&config_home, workdir.path())
        .args(["add", "hello from test", "--dataset-name", "e2e_dataset"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Success: Added 1 item(s) to dataset 'e2e_dataset'.",
        ));
}

#[test]
fn delete_all_preview_and_force_execution() {
    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        "\"00000000-0000-0000-0000-000000000000\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "data_root_directory",
        "\"./cognee_data\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        "\"sqlite::memory:\"",
    );

    make_cmd_in(&config_home, workdir.path())
        .args(["delete", "--all", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("datasets_to_delete: 0"))
        .stdout(predicate::str::contains("data_to_delete: 0"));

    make_cmd_in(&config_home, workdir.path())
        .args(["delete", "--all", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Success: Deleted datasets=0"));
}

#[test]
fn delete_data_scope_removes_only_targeted_graph_and_vector_artifacts() {
    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");

    let owner_id = Uuid::from_u128(0);
    let db_file_path = workdir.path().join("cognee.db");
    let db_url = format!("sqlite://{}", db_file_path.display());
    let graph_path = workdir.path().join("graph");
    let vector_path = workdir.path().join("vectors");
    std::fs::File::create(&db_file_path).expect("sqlite database file should be created");

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        &format!("\"{}\"", owner_id),
    );
    config_set(
        &config_home,
        workdir.path(),
        "data_root_directory",
        &format!("\"{}\"", workdir.path().join("cognee_data").display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        &format!("\"{}\"", db_url),
    );
    config_set(
        &config_home,
        workdir.path(),
        "graph_file_path",
        &format!("\"{}\"", graph_path.display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "vector_db_url",
        &format!("\"{}\"", vector_path.display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "graph_database_provider",
        "\"ladybug\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "vector_db_provider",
        "\"qdrant\"",
    );
    config_set(&config_home, workdir.path(), "embedding_dimensions", "2");

    make_cmd_in(&config_home, workdir.path())
        .args([
            "add",
            "delete this target item",
            "--dataset-name",
            "target_ds",
        ])
        .assert()
        .success();

    make_cmd_in(&config_home, workdir.path())
        .args(["add", "keep this item", "--dataset-name", "keep_ds"])
        .assert()
        .success();

    let fixture = seed_scoped_delete_fixture(
        &db_url,
        owner_id,
        "target_ds",
        "keep_ds",
        &graph_path,
        &vector_path,
    );

    make_cmd_in(&config_home, workdir.path())
        .args([
            "delete",
            "--data-id",
            &fixture.target_data_id.to_string(),
            "--force",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Success: Deleted"))
        .stdout(predicate::str::contains("data=1"));

    assert_scoped_delete_results(&db_url, &graph_path, &vector_path, &fixture);
}

#[test]
fn delete_dataset_scope_removes_only_targeted_graph_and_vector_artifacts() {
    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");

    let owner_id = Uuid::from_u128(0);
    let db_file_path = workdir.path().join("cognee.db");
    let db_url = format!("sqlite://{}", db_file_path.display());
    let graph_path = workdir.path().join("graph");
    let vector_path = workdir.path().join("vectors");
    std::fs::File::create(&db_file_path).expect("sqlite database file should be created");

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        &format!("\"{}\"", owner_id),
    );
    config_set(
        &config_home,
        workdir.path(),
        "data_root_directory",
        &format!("\"{}\"", workdir.path().join("cognee_data").display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        &format!("\"{}\"", db_url),
    );
    config_set(
        &config_home,
        workdir.path(),
        "graph_file_path",
        &format!("\"{}\"", graph_path.display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "vector_db_url",
        &format!("\"{}\"", vector_path.display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "graph_database_provider",
        "\"ladybug\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "vector_db_provider",
        "\"qdrant\"",
    );
    config_set(&config_home, workdir.path(), "embedding_dimensions", "2");

    make_cmd_in(&config_home, workdir.path())
        .args([
            "add",
            "delete this target item",
            "--dataset-name",
            "target_ds",
        ])
        .assert()
        .success();

    make_cmd_in(&config_home, workdir.path())
        .args(["add", "keep this item", "--dataset-name", "keep_ds"])
        .assert()
        .success();

    let fixture = seed_scoped_delete_fixture(
        &db_url,
        owner_id,
        "target_ds",
        "keep_ds",
        &graph_path,
        &vector_path,
    );

    make_cmd_in(&config_home, workdir.path())
        .args(["delete", "--dataset-name", "target_ds", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Success: Deleted"))
        .stdout(predicate::str::contains("datasets=1"))
        .stdout(predicate::str::contains("data=1"));

    assert_scoped_delete_results(&db_url, &graph_path, &vector_path, &fixture);
}

#[test]
fn delete_user_scope_removes_only_targeted_graph_and_vector_artifacts() {
    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");

    let target_owner_id = Uuid::from_u128(11);
    let keep_owner_id = Uuid::from_u128(22);
    let db_file_path = workdir.path().join("cognee.db");
    let db_url = format!("sqlite://{}", db_file_path.display());
    let graph_path = workdir.path().join("graph");
    let vector_path = workdir.path().join("vectors");
    std::fs::File::create(&db_file_path).expect("sqlite database file should be created");

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        &format!("\"{}\"", target_owner_id),
    );
    config_set(
        &config_home,
        workdir.path(),
        "data_root_directory",
        &format!("\"{}\"", workdir.path().join("cognee_data").display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        &format!("\"{}\"", db_url),
    );
    config_set(
        &config_home,
        workdir.path(),
        "graph_file_path",
        &format!("\"{}\"", graph_path.display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "vector_db_url",
        &format!("\"{}\"", vector_path.display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "graph_database_provider",
        "\"ladybug\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "vector_db_provider",
        "\"qdrant\"",
    );
    config_set(&config_home, workdir.path(), "embedding_dimensions", "2");

    make_cmd_in(&config_home, workdir.path())
        .args([
            "add",
            "delete target owner item",
            "--dataset-name",
            "target_owner_ds",
        ])
        .assert()
        .success();

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        &format!("\"{}\"", keep_owner_id),
    );

    make_cmd_in(&config_home, workdir.path())
        .args([
            "add",
            "keep other owner item",
            "--dataset-name",
            "keep_owner_ds",
        ])
        .assert()
        .success();

    let fixture = seed_cross_owner_delete_fixture(
        &db_url,
        target_owner_id,
        "target_owner_ds",
        keep_owner_id,
        "keep_owner_ds",
        &graph_path,
        &vector_path,
    );

    make_cmd_in(&config_home, workdir.path())
        .args([
            "delete",
            "--user-id",
            &target_owner_id.to_string(),
            "--force",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Success: Deleted"))
        .stdout(predicate::str::contains("datasets=1"))
        .stdout(predicate::str::contains("data=1"));

    assert_scoped_delete_results(&db_url, &graph_path, &vector_path, &fixture);
}

#[test]
fn cognify_without_datasets_fails_with_explicit_message() {
    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        "\"00000000-0000-0000-0000-000000000000\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "data_root_directory",
        "\"./cognee_data\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        "\"sqlite::memory:\"",
    );

    make_cmd_in(&config_home, workdir.path())
        .args(["cognify"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("No datasets found for owner"));
}

#[test]
#[ignore = "requires OPENAI_TOKEN/OPENAI_URL and local ONNX model/tokenizer files"]
fn cognify_live_smoke() {
    let api_key = std::env::var("OPENAI_TOKEN").expect("OPENAI_TOKEN must be set");
    let api_url = std::env::var("OPENAI_URL").expect("OPENAI_URL must be set");
    let embedding_model_path = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        .expect("COGNEE_E2E_EMBED_MODEL_PATH must be set");
    let embedding_tokenizer_path =
        std::env::var("COGNEE_E2E_TOKENIZER_PATH").expect("COGNEE_E2E_TOKENIZER_PATH must be set");

    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        "\"00000000-0000-0000-0000-000000000000\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "data_root_directory",
        "\"./cognee_data\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        "\"sqlite:./cognee.db\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "vector_db_url",
        "\"./vectors\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "graph_file_path",
        "\"./graph\"",
    );
    config_set(&config_home, workdir.path(), "llm_provider", "\"openai\"");
    config_set(&config_home, workdir.path(), "llm_model", "\"gpt-4o-mini\"");
    config_set(
        &config_home,
        workdir.path(),
        "llm_endpoint",
        &format!("\"{}\"", api_url),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_api_key",
        &format!("\"{}\"", api_key),
    );
    config_set(
        &config_home,
        workdir.path(),
        "embedding_model_path",
        &format!("\"{}\"", embedding_model_path),
    );
    config_set(
        &config_home,
        workdir.path(),
        "embedding_tokenizer_path",
        &format!("\"{}\"", embedding_tokenizer_path),
    );

    make_cmd_in(&config_home, workdir.path())
        .args([
            "add",
            "Cognee test content for live cognify smoke test.",
            "--dataset-name",
            "live_dataset",
        ])
        .assert()
        .success();

    make_cmd_in(&config_home, workdir.path())
        .args(["cognify", "--datasets", "live_dataset", "--verbose"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Success: Cognify completed."));
}

#[test]
#[ignore = "requires OPENAI_TOKEN/OPENAI_URL and local ONNX model/tokenizer files"]
fn search_live_smoke() {
    let api_key = std::env::var("OPENAI_TOKEN").expect("OPENAI_TOKEN must be set");
    let api_url = std::env::var("OPENAI_URL").expect("OPENAI_URL must be set");
    let embedding_model_path = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        .expect("COGNEE_E2E_EMBED_MODEL_PATH must be set");
    let embedding_tokenizer_path =
        std::env::var("COGNEE_E2E_TOKENIZER_PATH").expect("COGNEE_E2E_TOKENIZER_PATH must be set");

    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");

    config_set(
        &config_home,
        workdir.path(),
        "default_user_id",
        "\"00000000-0000-0000-0000-000000000000\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "data_root_directory",
        "\"./cognee_data\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        "\"sqlite:./cognee.db\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "vector_db_url",
        "\"./vectors\"",
    );
    config_set(
        &config_home,
        workdir.path(),
        "graph_file_path",
        "\"./graph\"",
    );
    config_set(&config_home, workdir.path(), "llm_provider", "\"openai\"");
    config_set(&config_home, workdir.path(), "llm_model", "\"gpt-4o-mini\"");
    config_set(
        &config_home,
        workdir.path(),
        "llm_endpoint",
        &format!("\"{}\"", api_url),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_api_key",
        &format!("\"{}\"", api_key),
    );
    config_set(
        &config_home,
        workdir.path(),
        "embedding_model_path",
        &format!("\"{}\"", embedding_model_path),
    );
    config_set(
        &config_home,
        workdir.path(),
        "embedding_tokenizer_path",
        &format!("\"{}\"", embedding_tokenizer_path),
    );

    make_cmd_in(&config_home, workdir.path())
        .args([
            "add",
            "Cognee search smoke test content.",
            "--dataset-name",
            "live_dataset",
        ])
        .assert()
        .success();

    make_cmd_in(&config_home, workdir.path())
        .args(["cognify", "--datasets", "live_dataset"])
        .assert()
        .success();

    make_cmd_in(&config_home, workdir.path())
        .args([
            "search",
            "What is this dataset about?",
            "--query-type",
            "CHUNKS",
            "--datasets",
            "live_dataset",
            "--output-format",
            "json",
        ])
        .assert()
        .success();
}
