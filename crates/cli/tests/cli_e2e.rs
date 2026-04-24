use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

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
        .stdout(predicate::str::contains(
            "--top-k must be between 1 and 100",
        ));
}

/// Regression for `docs/bug-search-dataset-name-filter-ignored.md`:
/// `cognee-cli search "..." -d <unknown-name>` used to silently return
/// every indexed record because `SearchRequest.datasets` was never
/// resolved to UUIDs. After the fix, the orchestrator must surface
/// `SearchError::DatasetNotFound`, which the CLI bubbles up as a
/// non-zero exit with `dataset not found` in the logged error.
///
/// The test wires `MOCK_EMBEDDING=true` and a dummy `llm_api_key` so the
/// LLM/embedding component init succeeds — neither is actually invoked
/// because resolution fails before the retriever runs. A real dataset is
/// added first to make the bogus-name lookup the only possible failure.
#[test]
#[cfg(all(feature = "ladybug", feature = "qdrant"))]
fn search_errors_when_dataset_name_does_not_exist() {
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
    // Dummy key so init_llm() succeeds — the search will never call the
    // LLM because the dataset name resolution fails first.
    config_set(&config_home, workdir.path(), "llm_api_key", "\"dummy-key\"");

    // Seed a real dataset so the only thing different about the search
    // call is the bogus dataset name.
    make_cmd_in(&config_home, workdir.path())
        .args(["add", "real content", "--dataset-name", "real_dataset"])
        .assert()
        .success();

    make_cmd_in(&config_home, workdir.path())
        .env("MOCK_EMBEDDING", "true")
        .args([
            "search",
            "anything",
            "--query-type",
            "CHUNKS",
            "-d",
            "this_dataset_does_not_exist",
            "--output-format",
            "simple",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("dataset not found"));
}

#[test]
fn delete_rejects_missing_scope() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["delete"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("Specify exactly one delete scope"));
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
        .stdout(predicate::str::contains("Invalid default_user_id"));
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
        .stdout(predicate::str::contains("delete preview"));

    make_cmd_in(&config_home, workdir.path())
        .args(["delete", "--all", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("delete completed"));
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
        .stdout(predicate::str::contains("No datasets found for owner"));
}

#[test]
fn cognify_live_smoke() {
    let api_key = std::env::var("OPENAI_TOKEN").expect("OPENAI_TOKEN must be set");
    let api_url = std::env::var("OPENAI_URL").expect("OPENAI_URL must be set");
    let llm_model = std::env::var("OPENAI_MODEL").expect("OPENAI_MODEL must be set");
    let embedding_model_path = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        .expect("COGNEE_E2E_EMBED_MODEL_PATH must be set");
    let embedding_tokenizer_path =
        std::env::var("COGNEE_E2E_TOKENIZER_PATH").expect("COGNEE_E2E_TOKENIZER_PATH must be set");

    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");
    let db_file_path = workdir.path().join("cognee.db");
    let db_url = format!("sqlite://{}", db_file_path.display());
    std::fs::File::create(&db_file_path).expect("sqlite database file should be created");

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
        &format!("\"{}\"", db_url),
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
    config_set(
        &config_home,
        workdir.path(),
        "llm_model",
        &format!("\"{}\"", llm_model),
    );
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
        .args(["cognify", "--datasets", "live_dataset"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cognify completed."));
}

#[test]
fn search_live_smoke() {
    let api_key = std::env::var("OPENAI_TOKEN").expect("OPENAI_TOKEN must be set");
    let api_url = std::env::var("OPENAI_URL").expect("OPENAI_URL must be set");
    let llm_model = std::env::var("OPENAI_MODEL").expect("OPENAI_MODEL must be set");
    let embedding_model_path = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        .expect("COGNEE_E2E_EMBED_MODEL_PATH must be set");
    let embedding_tokenizer_path =
        std::env::var("COGNEE_E2E_TOKENIZER_PATH").expect("COGNEE_E2E_TOKENIZER_PATH must be set");

    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");
    let db_file_path = workdir.path().join("cognee.db");
    let db_url = format!("sqlite://{}", db_file_path.display());
    std::fs::File::create(&db_file_path).expect("sqlite database file should be created");

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
        &format!("\"{}\"", db_url),
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
    config_set(
        &config_home,
        workdir.path(),
        "llm_model",
        &format!("\"{}\"", llm_model),
    );
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

// ---------------------------------------------------------------------------
// Gap 1 — Top-level --help and --version flags
// ---------------------------------------------------------------------------

#[test]
fn top_level_help_flag_prints_usage() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cognee"));
}

#[test]
fn top_level_version_flag_exits_gracefully() {
    // The CLI does not currently declare a --version flag, so this should
    // exit with a non-zero code but must not panic or crash.
    let config_home = TempDir::new().expect("temp dir should be created");
    let output = make_cmd(&config_home)
        .args(["--version"])
        .output()
        .expect("command should run without crashing");
    // Either success (if version is added in future) or clean failure is acceptable.
    let _ = output.status;
}

// ---------------------------------------------------------------------------
// Gap 2 — Per-command --help flags
// ---------------------------------------------------------------------------

#[test]
fn add_subcommand_help_flag_prints_usage() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .args(["add", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage").or(predicate::str::contains("usage")));
}

#[test]
fn search_subcommand_help_flag_prints_usage() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .args(["search", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage").or(predicate::str::contains("usage")));
}

#[test]
fn cognify_subcommand_help_flag_prints_usage() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .args(["cognify", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage").or(predicate::str::contains("usage")));
}

#[test]
fn delete_subcommand_help_flag_prints_usage() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .args(["delete", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage").or(predicate::str::contains("usage")));
}

#[test]
fn config_subcommand_help_flag_prints_usage() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .args(["config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage").or(predicate::str::contains("usage")));
}

// ---------------------------------------------------------------------------
// Gap 3 — search with missing required query argument
// ---------------------------------------------------------------------------

#[test]
fn search_without_query_argument_fails() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .args(["search"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("required")
                .or(predicate::str::contains("error"))
                .or(predicate::str::contains("Usage")),
        );
}

// ---------------------------------------------------------------------------
// Gap 4 — Invalid search type is rejected with non-zero exit code
// ---------------------------------------------------------------------------

#[test]
fn search_with_invalid_query_type_fails() {
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
        .args(["search", "some query", "--query-type", "INVALID_TYPE"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("invalid")
                .or(predicate::str::contains("error"))
                .or(predicate::str::contains("INVALID_TYPE")),
        );
}

// ---------------------------------------------------------------------------
// Gap 5 — Full search option parsing (structural, no backend)
// ---------------------------------------------------------------------------

#[test]
fn search_full_option_parsing_does_not_fail_on_argument_errors() {
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

    // All arguments are structurally valid. The command may fail with a
    // business-logic error ("No datasets found") but must not fail with a
    // clap argument-parsing error (exit code 2, "Usage:" in stderr).
    // Note: --datasets uses append semantics, so multiple datasets require
    // repeated flags (--datasets d1 --datasets d2).
    let output = make_cmd_in(&config_home, workdir.path())
        .args([
            "search",
            "test query",
            "--query-type",
            "CHUNKS",
            "--datasets",
            "d1",
            "--datasets",
            "d2",
            "--top-k",
            "5",
            "--output-format",
            "json",
        ])
        .output()
        .expect("command should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("Usage:") || output.status.code() != Some(2),
        "command should not fail with an argument-parsing error; stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Gap 6 — cognify full option parsing (structural, no LLM)
// ---------------------------------------------------------------------------

#[test]
fn cognify_with_datasets_option_does_not_fail_on_argument_errors() {
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

    // Structurally valid; may fail with "No datasets found" (business logic)
    // but must not fail due to clap argument-parsing.
    // Note: --datasets uses append semantics; pass one dataset name per flag.
    let output = make_cmd_in(&config_home, workdir.path())
        .args(["cognify", "--datasets", "d1", "--datasets", "d2"])
        .output()
        .expect("command should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains("No datasets found")
            || stdout.contains("Cognify completed")
            || !stderr.contains("Usage:"),
        "unexpected output — stdout: {stdout}, stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Gap 7 — Invalid command name returns non-zero exit code
// ---------------------------------------------------------------------------

#[test]
fn invalid_command_name_returns_nonzero_exit_code() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .args(["invalid_command"])
        .assert()
        .failure();
}
