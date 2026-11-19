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
