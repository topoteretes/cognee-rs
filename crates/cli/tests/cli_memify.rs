#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

fn make_cmd(config_home: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"));
    command.env("XDG_CONFIG_HOME", config_home.path());
    command
}

#[test]
fn test_memify_help() {
    let config_home = TempDir::new().expect("temp dir should be created");
    make_cmd(&config_home)
        .arg("memify")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage").or(predicate::str::contains("usage")));
}

// -----------------------------------------------------------------------------
// Functional CLI E2E tests below
//
// These follow the `cognify_live_smoke` pattern from cli_e2e.rs:
// - Isolated TempDir per test with its own XDG_CONFIG_HOME and workdir.
// - `config_set` writes settings via the `cognee-cli config set` subcommand.
// - `MOCK_EMBEDDING=true` forces zero-vector embeddings (no network for the
//   embedding path). Memify itself never calls the LLM.
// - LLM-gated tests short-circuit with a printed skip message when
//   OPENAI_TOKEN / OPENAI_URL / OPENAI_MODEL are not all set, matching the
//   existing pattern in cli_e2e.rs.
// -----------------------------------------------------------------------------

#[derive(Clone)]
struct LlmEnv {
    api_key: String,
    api_url: String,
    llm_model: String,
}

/// Returns the LLM env vars required for cognify seed steps, or `None` if any
/// is missing. The caller should early-return in the `None` branch.
fn skip_if_no_llm(test_name: &str) -> Option<LlmEnv> {
    let api_key = std::env::var("OPENAI_TOKEN").ok();
    let api_url = std::env::var("OPENAI_URL").ok();
    let llm_model = std::env::var("OPENAI_MODEL").ok();

    match (api_key, api_url, llm_model) {
        (Some(api_key), Some(api_url), Some(llm_model))
            if !api_key.is_empty() && !api_url.is_empty() && !llm_model.is_empty() =>
        {
            Some(LlmEnv {
                api_key,
                api_url,
                llm_model,
            })
        }
        _ => {
            eprintln!(
                "[{test_name}] skipping: no LLM env configured \
                 (set OPENAI_TOKEN, OPENAI_URL, OPENAI_MODEL to run)"
            );
            None
        }
    }
}

fn config_set(config_home: &TempDir, workdir: &Path, key: &str, json_value: &str) {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"));
    cmd.env("XDG_CONFIG_HOME", config_home.path())
        .current_dir(workdir)
        .args(["config", "set", key, json_value])
        .assert()
        .success();
}

/// Holds the per-test temp directories and knows how to build a pre-configured
/// `Command` for invoking `cognee-cli` subcommands.
struct Workspace {
    config_home: TempDir,
    workdir: TempDir,
}

impl Workspace {
    /// Returns a `Command` with `XDG_CONFIG_HOME`, `current_dir`, and
    /// `MOCK_EMBEDDING=true` already applied. The binary never talks to a
    /// real embedding backend in these tests.
    fn cognee_cli_cmd(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("cognee-cli"));
        command
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("MOCK_EMBEDDING", "true")
            .current_dir(self.workdir.path());
        command
    }
}

/// Creates a fresh workspace with all backends wired to local files and optional
/// LLM credentials.
///
/// When `llm` is `Some`, the real OpenAI-compatible endpoint is configured so
/// that a subsequent `cognify` invocation can extract entities. When `llm` is
/// `None`, placeholder values are written -- this is fine for commands that
/// don't initialize the LLM (e.g. memify, or memify on an invalid dataset).
fn setup_workspace(llm: Option<&LlmEnv>) -> Workspace {
    let config_home = TempDir::new().expect("temp dir should be created");
    let workdir = TempDir::new().expect("temp dir should be created");
    let db_file_path = workdir.path().join("cognee.db");
    let db_url = format!("sqlite://{}", db_file_path.display());
    std::fs::File::create(&db_file_path).expect("sqlite database file should be created");

    // Fixed test UUID so UUID5-derived IDs remain stable.
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

    // Always write LLM config: real values when available (for cognify seed),
    // otherwise placeholders. Memify does not initialize the LLM, so the
    // placeholders are harmless for memify-only flows.
    let (provider, model, endpoint, api_key) = match llm {
        Some(env) => (
            "openai",
            env.llm_model.as_str(),
            env.api_url.as_str(),
            env.api_key.as_str(),
        ),
        None => ("openai", "gpt-4o-mini", "http://localhost:1", "placeholder"),
    };
    config_set(
        &config_home,
        workdir.path(),
        "llm_provider",
        &format!("\"{provider}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_model",
        &format!("\"{model}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_endpoint",
        &format!("\"{endpoint}\""),
    );
    config_set(
        &config_home,
        workdir.path(),
        "llm_api_key",
        &format!("\"{api_key}\""),
    );

    Workspace {
        config_home,
        workdir,
    }
}

/// Runs `add` + `cognify` to populate the graph so subsequent `memify` calls
/// have real triplets to index. Returns the dataset name used.
fn seed_cognified_dataset(ws: &Workspace, dataset_name: &str) {
    ws.cognee_cli_cmd()
        .args([
            "add",
            "Alice is an engineer who works at TechCorp. Bob is a researcher at TechCorp.",
            "--dataset-name",
            dataset_name,
        ])
        .assert()
        .success();

    ws.cognee_cli_cmd()
        .args(["cognify", "--datasets", dataset_name])
        .assert()
        .success();
}

#[test]
fn test_memify_cli_invalid_dataset() {
    // No LLM needed: memify doesn't call the LLM, and the error path is hit
    // before any LLM call would happen (dataset lookup in the per-dataset loop).
    let ws = setup_workspace(None);

    // Error messages are emitted via tracing, which the CLI routes to stdout
    // by default (see crates/cli/src/main.rs). Check stdout for the validation
    // message; combined output is covered below for robustness.
    let output = ws
        .cognee_cli_cmd()
        .args(["memify", "--datasets", "does-not-exist"])
        .output()
        .expect("memify command should spawn successfully");

    assert!(
        !output.status.success(),
        "expected non-zero exit for missing dataset, got {:?}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("was not found"),
        "expected 'was not found' in CLI output.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

#[test]
fn test_memify_cli_basic_invocation() {
    let Some(llm) = skip_if_no_llm("test_memify_cli_basic_invocation") else {
        return;
    };

    let ws = setup_workspace(Some(&llm));
    let dataset = "memify_basic";
    seed_cognified_dataset(&ws, dataset);

    ws.cognee_cli_cmd()
        .args(["memify", "--datasets", dataset])
        .assert()
        .success()
        .stdout(predicate::str::contains("Memify completed."));
}

#[test]
fn test_memify_cli_with_node_type_filter() {
    let Some(llm) = skip_if_no_llm("test_memify_cli_with_node_type_filter") else {
        return;
    };

    let ws = setup_workspace(Some(&llm));
    let dataset = "memify_node_type";
    seed_cognified_dataset(&ws, dataset);

    ws.cognee_cli_cmd()
        .args(["memify", "--datasets", dataset, "--node-type", "Entity"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Memify completed."));
}

#[test]
fn test_memify_cli_with_node_names() {
    let Some(llm) = skip_if_no_llm("test_memify_cli_with_node_names") else {
        return;
    };

    let ws = setup_workspace(Some(&llm));
    let dataset = "memify_node_names";
    seed_cognified_dataset(&ws, dataset);

    ws.cognee_cli_cmd()
        .args([
            "memify",
            "--datasets",
            dataset,
            "--node-name",
            "Alice",
            "--node-name",
            "Bob",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Memify completed."));
}

#[test]
fn test_memify_cli_with_batch_size() {
    let Some(llm) = skip_if_no_llm("test_memify_cli_with_batch_size") else {
        return;
    };

    let ws = setup_workspace(Some(&llm));
    let dataset = "memify_batch_size";
    seed_cognified_dataset(&ws, dataset);

    ws.cognee_cli_cmd()
        .args(["memify", "--datasets", dataset, "--batch-size", "50"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Memify completed."));
}

#[test]
fn test_memify_cli_output_format() {
    let Some(llm) = skip_if_no_llm("test_memify_cli_output_format") else {
        return;
    };

    let ws = setup_workspace(Some(&llm));
    let dataset = "memify_output_format";
    seed_cognified_dataset(&ws, dataset);

    // Capture combined output and assert the summary log's exact format:
    //   "Memify completed. triplets=<N>, indexed=<N>, batches=<N>"
    let output = ws
        .cognee_cli_cmd()
        .args(["memify", "--datasets", dataset])
        .output()
        .expect("memify command should spawn successfully");

    assert!(
        output.status.success(),
        "memify exited non-zero: status={:?}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // Locate the summary line and pin its structure without pulling in `regex`.
    let marker = "Memify completed. triplets=";
    let start = combined
        .find(marker)
        .unwrap_or_else(|| panic!("summary marker not found in output:\n{combined}"));
    let tail = &combined[start + marker.len()..];

    // Split: "<triplets>, indexed=<indexed>, batches=<batches>..."
    let (triplets_str, rest) = tail
        .split_once(", indexed=")
        .unwrap_or_else(|| panic!("expected ', indexed=' in summary tail:\n{tail}"));
    let (indexed_str, rest) = rest
        .split_once(", batches=")
        .unwrap_or_else(|| panic!("expected ', batches=' in summary tail:\n{rest}"));
    // `batches=<N>` is followed by end-of-line / ANSI codes / newline.
    let batches_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();

    assert!(
        triplets_str.chars().all(|c| c.is_ascii_digit()) && !triplets_str.is_empty(),
        "triplets count was not a bare integer: {triplets_str:?}"
    );
    assert!(
        indexed_str.chars().all(|c| c.is_ascii_digit()) && !indexed_str.is_empty(),
        "indexed count was not a bare integer: {indexed_str:?}"
    );
    assert!(
        !batches_str.is_empty(),
        "batches count was not present as digits in: {rest:?}"
    );
}
