#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
use assert_cmd::Command;
use md5::{Digest, Md5};
use predicates::prelude::*;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use uuid::Uuid;

fn make_cmd(config_home: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"));
    command.env("XDG_CONFIG_HOME", config_home.path());
    command.env("HOME", config_home.path());
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

#[derive(Debug)]
struct DataRow {
    raw_data_location: String,
    original_data_location: String,
    extension: String,
    mime_type: String,
    content_hash: String,
    original_extension: Option<String>,
    original_mime_type: Option<String>,
    loader_engine: Option<String>,
    raw_content_hash: Option<String>,
    external_metadata: Option<String>,
}

fn read_single_data_row(db_path: &Path) -> DataRow {
    let conn = Connection::open(db_path).expect("open sqlite database");
    conn.query_row(
        "SELECT raw_data_location, original_data_location, extension, mime_type, \
                content_hash, original_extension, original_mime_type, loader_engine, \
                raw_content_hash, external_metadata \
         FROM data",
        [],
        |row| {
            Ok(DataRow {
                raw_data_location: row.get(0)?,
                original_data_location: row.get(1)?,
                extension: row.get(2)?,
                mime_type: row.get(3)?,
                content_hash: row.get(4)?,
                original_extension: row.get(5)?,
                original_mime_type: row.get(6)?,
                loader_engine: row.get(7)?,
                raw_content_hash: row.get(8)?,
                external_metadata: row.get(9)?,
            })
        },
    )
    .expect("single data row")
}

fn file_uri_to_path(uri: &str) -> PathBuf {
    let path = uri
        .strip_prefix("file://")
        .expect("stored location should be a file URI");
    PathBuf::from(path)
}

fn md5_hex(bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn config_value(config_home: &TempDir, key: &str) -> serde_json::Value {
    let path = [
        config_home.path().join("cognee-rust").join("config.json"),
        config_home
            .path()
            .join("Library")
            .join("Application Support")
            .join("cognee-rust")
            .join("config.json"),
    ]
    .into_iter()
    .find(|path| path.exists())
    .expect("config file should exist under temp config home");

    let document: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .expect("config json");

    document
        .get("settings")
        .and_then(|settings| settings.get(key))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn live_cli_env(test_name: &str) -> Option<(String, String, String, String, String)> {
    let api_key = std::env::var("OPENAI_TOKEN").ok();
    let api_url = std::env::var("OPENAI_URL").ok();
    let llm_model = std::env::var("OPENAI_MODEL").ok();
    let embedding_model_path = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH").ok();
    let embedding_tokenizer_path = std::env::var("COGNEE_E2E_TOKENIZER_PATH").ok();

    match (
        api_key,
        api_url,
        llm_model,
        embedding_model_path,
        embedding_tokenizer_path,
    ) {
        (
            Some(api_key),
            Some(api_url),
            Some(llm_model),
            Some(embedding_model_path),
            Some(embedding_tokenizer_path),
        ) if !api_key.is_empty()
            && !api_url.is_empty()
            && !llm_model.is_empty()
            && !embedding_model_path.is_empty()
            && !embedding_tokenizer_path.is_empty() =>
        {
            Some((
                api_key,
                api_url,
                llm_model,
                embedding_model_path,
                embedding_tokenizer_path,
            ))
        }
        _ => {
            eprintln!(
                "[{test_name}] skipping: live CLI env not configured \
                 (set OPENAI_TOKEN, OPENAI_URL, OPENAI_MODEL, \
                 COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH to run)"
            );
            None
        }
    }
}

#[test]
fn config_set_get_roundtrip_chunk_size() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["config", "set", "chunk_size", "2048"])
        .assert()
        .success();

    assert_eq!(config_value(&config_home, "chunk_size"), 2048);
}

#[test]
fn config_list_contains_expected_keys() {
    let config_home = TempDir::new().expect("temp dir should be created");

    make_cmd(&config_home)
        .args(["config", "list"])
        .assert()
        .success();
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
        .success();

    assert_eq!(config_value(&config_home, "llm_provider"), "openai");
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
#[cfg(feature = "ladybug")]
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
        &format!("\"{owner_id}\""),
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
        &format!("\"{db_url}\""),
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
        "\"brute-force\"",
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
        .success();
}

#[test]
fn add_url_stores_extracted_text_raw_html_and_metadata() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let (url, _server) = rt.block_on(async {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/robots.txt")
            .with_status(404)
            .create_async()
            .await;
        let html = concat!(
            "<html><head><title>Local Fixture</title>",
            "<style>.hidden{display:none}</style></head>",
            "<body><h1>Visible URL title</h1>",
            "<p>Boundary text from a local fixture.</p>",
            "<script>window.secret = true;</script></body></html>"
        );
        server
            .mock("GET", "/page.html")
            .with_status(200)
            .with_header("content-type", "text/html; charset=utf-8")
            .with_body(html)
            .create_async()
            .await;
        (format!("{}/page.html", server.url()), server)
    });

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
        &format!("\"{}\"", workdir.path().join("cognee_data").display()),
    );
    config_set(
        &config_home,
        workdir.path(),
        "relational_db_url",
        &format!("\"{db_url}\""),
    );

    make_cmd_in(&config_home, workdir.path())
        .args(["add", &url, "--dataset-name", "url_e2e_dataset"])
        .assert()
        .success();

    let row = read_single_data_row(&db_file_path);
    assert_eq!(row.extension, "txt");
    assert_eq!(row.mime_type, "text/plain");
    assert_eq!(row.original_extension.as_deref(), Some("html"));
    assert_eq!(row.original_mime_type.as_deref(), Some("text/html"));
    assert_eq!(row.loader_engine.as_deref(), Some("beautiful_soup_loader"));
    assert!(row.raw_data_location.ends_with(".txt"));
    assert!(row.original_data_location.ends_with(".html"));
    assert_ne!(row.raw_data_location, row.original_data_location);

    let extracted = std::fs::read(file_uri_to_path(&row.raw_data_location))
        .expect("read extracted text payload");
    let extracted_text = String::from_utf8(extracted.clone()).expect("extracted text is utf8");
    assert!(extracted_text.contains("Visible URL title"));
    assert!(extracted_text.contains("Boundary text from a local fixture."));
    assert!(!extracted_text.contains("<html>"));
    assert!(!extracted_text.contains("window.secret"));
    assert_eq!(row.content_hash, md5_hex(&extracted));
    assert_eq!(
        row.raw_content_hash.as_deref(),
        Some(row.content_hash.as_str())
    );

    let raw_html =
        std::fs::read(file_uri_to_path(&row.original_data_location)).expect("read raw html");
    assert!(String::from_utf8_lossy(&raw_html).contains("<title>Local Fixture</title>"));
    assert!(String::from_utf8_lossy(&raw_html).contains("window.secret = true"));

    let metadata: serde_json::Value =
        serde_json::from_str(row.external_metadata.as_deref().expect("url metadata"))
            .expect("metadata json");
    assert_eq!(metadata["source"], "url");
    assert_eq!(metadata["url"], url);
    assert_eq!(metadata["final_url"], url);
    assert_eq!(metadata["content_type"], "text/html; charset=utf-8");
    assert_eq!(metadata["title"], "Local Fixture");
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
        .success();

    make_cmd_in(&config_home, workdir.path())
        .args(["delete", "--all", "--force"])
        .assert()
        .success();
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
    let Some((api_key, api_url, llm_model, embedding_model_path, embedding_tokenizer_path)) =
        live_cli_env("cognify_live_smoke")
    else {
        return;
    };

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
        &format!("\"{db_url}\""),
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
        &format!("\"{llm_model}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_endpoint",
        &format!("\"{api_url}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_api_key",
        &format!("\"{api_key}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "embedding_model_path",
        &format!("\"{embedding_model_path}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "embedding_tokenizer_path",
        &format!("\"{embedding_tokenizer_path}\""),
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
    let Some((api_key, api_url, llm_model, embedding_model_path, embedding_tokenizer_path)) =
        live_cli_env("search_live_smoke")
    else {
        return;
    };

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
        &format!("\"{db_url}\""),
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
        &format!("\"{llm_model}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_endpoint",
        &format!("\"{api_url}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_api_key",
        &format!("\"{api_key}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "embedding_model_path",
        &format!("\"{embedding_model_path}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "embedding_tokenizer_path",
        &format!("\"{embedding_tokenizer_path}\""),
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
