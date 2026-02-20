use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

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
