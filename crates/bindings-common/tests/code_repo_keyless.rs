//! SDK-1053 O4: with no LLM key, `add(<project dir>)` then `cognify()` builds
//! the code graph through the shared binding ops.
//!
//! The handle is keyless and nothing serves the `auto` extractor's GLiNER
//! choice, which refuses a standard-route cognify. A dataset of code-repo
//! items needs no extractor, so cognify does not resolve one. The project's
//! README is left out at add (its route needs an LLM). enola is the fake
//! script of the `cognee-code-graph` tests.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::path::Path;

use cognee::config::Settings;
use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::pipeline;
use serde_json::{Value, json};
use tempfile::TempDir;

fn settings_under(dir: &TempDir) -> Settings {
    let root = dir.path();
    let enola =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../code-graph/tests/fixtures/bin/enola");
    Settings {
        sections: [
            (
                "relational.url".to_string(),
                json!(format!(
                    "sqlite://{}?mode=rwc",
                    root.join("cognee.db").display()
                )),
            ),
            ("embedding.provider".to_string(), json!("mock")),
            ("llm.api_key".to_string(), json!("")),
            (
                "code_graph.enola_path".to_string(),
                json!(enola.to_string_lossy()),
            ),
        ]
        .into(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        system_root_directory: root.join("sys").to_string_lossy().into_owned(),
        ..Settings::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_keyless_handle_cognifies_a_code_project() {
    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path().join("shop");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("pyproject.toml"), "[project]\nname = \"shop\"\n").unwrap();
    std::fs::write(repo.join("main.py"), "def main():\n    pass\n").unwrap();
    std::fs::write(repo.join("README.md"), "Shop.\n").unwrap();
    std::fs::write(repo.join(".fake-enola"), "snapshot:basic").unwrap();

    let state = HandleState::from_settings(settings_under(&dir));

    let added = pipeline::add(
        &state,
        json!({ "type": "file", "path": repo.to_string_lossy() }),
        "code",
        &Value::Null,
    )
    .await
    .expect("add the project");
    let rows = added["added"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "only the repo item without an LLM: {rows:?}");

    let result = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .expect("a code-only dataset cognifies without an LLM or extractor");
    assert_eq!(result["chunks"], 0);
    assert_eq!(result["alreadyCompleted"], false);

    let again = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .expect("second cognify");
    assert_eq!(again["alreadyCompleted"], true);
}

/// A code-only cognify builds no extractor, but a selection the caller
/// passed is still validated: a misspelled or malformed `graphBackend` or
/// `extractor` is refused, while the keyless default and an explicit `llm`
/// are not.
#[tokio::test(flavor = "multi_thread")]
async fn a_code_only_cognify_still_validates_an_explicit_extractor() {
    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path().join("shop");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("go.mod"), "module shop\n").unwrap();
    std::fs::write(repo.join("main.go"), "package main\n").unwrap();
    std::fs::write(repo.join(".fake-enola"), "snapshot:basic").unwrap();

    let state = HandleState::from_settings(settings_under(&dir));
    pipeline::add(
        &state,
        json!({ "type": "file", "path": repo.to_string_lossy() }),
        "code",
        &Value::Null,
    )
    .await
    .expect("add the project");

    for opts in [
        json!({ "graphBackend": "no-such-backend" }),
        json!({ "graphBackend": { "kind": "no-such-backend" } }),
        json!({ "extractor": "no-such-extractor" }),
    ] {
        let error = pipeline::cognify(&state, "code", &opts)
            .await
            .expect_err("an unregistered explicit selection is refused");
        assert!(error.to_string().contains("no-such-"), "{opts}: {error}");
    }
    for opts in [json!({ "graphBackend": 5 }), json!({ "extractor": 5 })] {
        pipeline::cognify(&state, "code", &opts)
            .await
            .expect_err("a malformed selection is refused");
    }

    // Blank and `llm` selections resolve to no extractor at all.
    for opts in [json!({ "extractor": "" }), json!({ "extractor": "llm" })] {
        pipeline::cognify(&state, "code", &opts)
            .await
            .unwrap_or_else(|error| panic!("{opts}: {error}"));
    }
}

async fn rows(state: &HandleState, dataset_id: &str) -> Vec<Value> {
    cognee_bindings_common::ops::datasets::list_data(state, dataset_id)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

/// The manifest's data id is pinned to (user, dataset, repo path), as in
/// Python: re-adding the repository updates its one row. Unchanged, the row
/// keeps its cognify marker and the next cognify has nothing to do; with
/// changed code, the row takes the new content, its marker is cleared and
/// the next cognify loads the code graph again.
#[tokio::test(flavor = "multi_thread")]
async fn re_adding_a_repository_updates_its_one_pinned_row() {
    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path().join("shop");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("go.mod"), "module shop\n").unwrap();
    std::fs::write(repo.join("main.go"), "package main\n").unwrap();
    std::fs::write(repo.join(".fake-enola"), "snapshot:basic").unwrap();
    let input = json!({ "type": "file", "path": repo.to_string_lossy() });

    let state = HandleState::from_settings(settings_under(&dir));
    let first = pipeline::add(&state, input.clone(), "code", &Value::Null)
        .await
        .expect("add the project");
    let row = &first["added"][0];
    let (data_id, dataset_id) = (
        row["id"].as_str().unwrap().to_string(),
        row["dataset_id"].as_str().unwrap().to_string(),
    );
    let owner_id = state.owner_id().await.unwrap();
    let canonical = std::fs::canonicalize(&repo).unwrap();
    assert_eq!(
        data_id,
        cognee::add::code_repo::code_repo_data_id(
            &canonical.to_string_lossy(),
            owner_id,
            None,
            dataset_id.parse().unwrap(),
        )
        .to_string(),
        "the pinned id of Python's get_unique_data_id"
    );
    let cognified = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .unwrap();
    assert_eq!(cognified["alreadyCompleted"], false);

    // Unchanged: the same row, still cognified.
    let again = pipeline::add(&state, input.clone(), "code", &Value::Null)
        .await
        .unwrap();
    assert_eq!(again["addedCount"], 0);
    assert_eq!(again["deduplicated"][0]["id"], json!(data_id));
    let stored = rows(&state, &dataset_id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0]["content_hash"], row["content_hash"]);
    let unchanged = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .unwrap();
    assert_eq!(unchanged["alreadyCompleted"], true);

    // Changed code: the same row, new content, cognified again.
    std::fs::write(repo.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();
    let changed = pipeline::add(&state, input, "code", &Value::Null)
        .await
        .unwrap();
    assert_eq!(changed["addedCount"], 0);
    assert_eq!(changed["deduplicated"][0]["id"], json!(data_id));
    let stored = rows(&state, &dataset_id).await;
    assert_eq!(stored.len(), 1, "one manifest row: {stored:?}");
    assert_eq!(stored[0]["id"], json!(data_id));
    assert_ne!(stored[0]["content_hash"], row["content_hash"]);
    let reloaded = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .unwrap();
    assert_eq!(reloaded["alreadyCompleted"], false, "{reloaded}");
}

/// `addAndCognify` of a changed repository cognifies its refreshed row,
/// although the row existed before the call (its cognify marker was
/// cleared); an unchanged one has nothing to cognify. (A key is set so the
/// `auto` extractor resolves; the code route never calls the LLM.)
#[tokio::test(flavor = "multi_thread")]
async fn add_and_cognify_reloads_a_changed_repository() {
    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path().join("shop");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("go.mod"), "module shop\n").unwrap();
    std::fs::write(repo.join("main.go"), "package main\n").unwrap();
    std::fs::write(repo.join(".fake-enola"), "snapshot:basic").unwrap();
    let input = json!({ "type": "file", "path": repo.to_string_lossy() });

    let mut settings = settings_under(&dir);
    settings
        .sections
        .insert("llm.api_key".to_string(), json!("sk-test"));
    let state = HandleState::from_settings(settings);

    let first = pipeline::add_and_cognify(&state, input.clone(), "code", &Value::Null)
        .await
        .expect("first addAndCognify");
    assert_eq!(first["add"]["addedCount"], 1);
    assert_eq!(first["cognify"]["alreadyCompleted"], false);

    let unchanged = pipeline::add_and_cognify(&state, input.clone(), "code", &Value::Null)
        .await
        .unwrap();
    assert_eq!(unchanged["add"]["deduplicatedCount"], 1);
    assert_eq!(unchanged["cognify"]["chunks"], 0);
    assert_eq!(unchanged["cognify"]["priorPipelineRunId"], Value::Null);

    std::fs::write(repo.join("main.go"), "package main\n\nfunc main() {}\n").unwrap();
    let changed = pipeline::add_and_cognify(&state, input, "code", &Value::Null)
        .await
        .unwrap();
    assert_eq!(changed["add"]["deduplicatedCount"], 1);
    assert_eq!(changed["cognify"]["alreadyCompleted"], false, "{changed}");
    // It was cognified by that call: a cognify now has nothing left to do.
    let after = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .unwrap();
    assert_eq!(after["alreadyCompleted"], true, "{after}");
}

/// SDK-1053 O9: with no LLM key, `add(<code file>)` then `cognify()` loads the
/// file's code graph down the per-file CODE route: the file is a code item,
/// so cognify resolves no extractor.
#[tokio::test(flavor = "multi_thread")]
async fn a_keyless_handle_cognifies_a_code_file() {
    let dir = TempDir::new().expect("tempdir");
    let file = dir.path().join("payments.py");
    std::fs::write(&file, "def pay():\n    pass\n").unwrap();

    let state = HandleState::from_settings(settings_under(&dir));
    let added = pipeline::add(
        &state,
        json!({ "type": "file", "path": file.to_string_lossy() }),
        "code",
        &Value::Null,
    )
    .await
    .expect("add the code file");
    let rows = added["added"].as_array().expect("rows");
    assert_eq!(rows.len(), 1, "{rows:?}");

    let result = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .expect("a code file cognifies without an LLM or extractor");
    assert_eq!(result["chunks"], 0);
    assert_eq!(result["alreadyCompleted"], false);

    let again = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .expect("second cognify");
    assert_eq!(again["alreadyCompleted"], true);
}
