//! SDK-1053 O5: `remember` with `contentType: "code"` through the shared
//! binding ops, on a keyless handle.
//!
//! Nothing serves an LLM or the `auto` extractor's GLiNER choice here, which
//! a plain remember would need. The code route resolves neither. enola is the
//! fake script of the `cognee-code-graph` tests.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::path::{Path, PathBuf};

use cognee::config::Settings;
use cognee_bindings_common::ops::{memory, pipeline};
use cognee_bindings_common::{HandleState, SdkError};
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

fn project(dir: &TempDir) -> PathBuf {
    let repo = dir.path().join("shop");
    std::fs::create_dir(&repo).unwrap();
    std::fs::write(repo.join("pyproject.toml"), "[project]\nname = \"shop\"\n").unwrap();
    std::fs::write(repo.join("main.py"), "def main():\n    pass\n").unwrap();
    std::fs::write(repo.join("README.md"), "Shop.\n").unwrap();
    std::fs::write(repo.join(".fake-enola"), "snapshot:basic").unwrap();
    repo.canonicalize().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_keyless_handle_remembers_a_code_repository() {
    let dir = TempDir::new().expect("tempdir");
    let repo = project(&dir);
    let state = HandleState::from_settings(settings_under(&dir));

    let result = memory::run_remember(
        &state,
        json!({ "type": "file", "path": repo.to_string_lossy() }),
        "code",
        // nodeSet is accepted and ignored, as in Python.
        &json!({ "contentType": "code", "nodeSet": ["team"] }),
    )
    .await
    .expect("remember the repository without an LLM");

    assert_eq!(result["status"], "PipelineRunCompleted", "{result}");
    assert_eq!(result["items_processed"], 1);
    let items = result["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "code_repository");
    assert_eq!(items[0]["path"], repo.to_string_lossy().as_ref());
    assert_eq!(items[0]["pipeline_run_id"], result["pipeline_run_id"]);
    assert!(items[0]["id"].is_string());

    // The row is stamped cognified: a cognify of the dataset has nothing to
    // do (the README was never stored, or it would need an extractor).
    let again = pipeline::cognify(&state, "code", &Value::Null)
        .await
        .expect("cognify after remember");
    assert_eq!(again["alreadyCompleted"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn index_vectors_embeds_through_the_handles_engine() {
    let dir = TempDir::new().expect("tempdir");
    let repo = project(&dir);
    let state = HandleState::from_settings(settings_under(&dir));

    let result = memory::run_remember(
        &state,
        json!({ "type": "file", "path": repo.to_string_lossy() }),
        "code",
        &json!({ "contentType": "code", "indexVectors": true }),
    )
    .await
    .expect("remember the repository with its vectors");
    assert_eq!(result["status"], "PipelineRunCompleted", "{result}");

    let svc = state.services().await.unwrap();
    let collections = svc.vector_db.list_collections().await.unwrap();
    assert!(
        collections.contains(&("CodeRepository".to_string(), "name".to_string())),
        "{collections:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn content_type_options_are_validated_before_anything_runs() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));
    let data = json!({ "type": "text", "text": "hello" });

    for (opts, expected) in [
        (
            json!({ "contentType": "nope" }),
            "Unsupported remember content_type. Supported values: 'code'.",
        ),
        (
            json!({ "contentType": "skills" }),
            "content_type='skills' is not supported by this SDK yet",
        ),
        (
            json!({ "indexVectors": true }),
            "index_vectors is supported only for content_type='code'.",
        ),
        (
            json!({ "repoCredentials": "token" }),
            "repo_credentials is supported only for content_type='code'.",
        ),
        (
            json!({ "contentType": "code", "branch": "dev" }),
            "branch is supported only without content_type",
        ),
        (
            json!({ "contentType": "code", "sessionId": "s1" }),
            "session_id is not applicable to content_type='code'",
        ),
        (
            json!({ "contentType": "code", "repoCredentials": 7 }),
            "`repoCredentials` must be a string",
        ),
        (
            json!({ "branch": "dev" }),
            "`branch` is not supported by remember() in this SDK yet",
        ),
        (
            json!({ "contentType": "code" }),
            "content_type='code' expects a repository path or git URL",
        ),
    ] {
        let data = if opts.get("contentType") == Some(&json!("code"))
            && opts.as_object().unwrap().len() == 1
        {
            json!({ "type": "binary", "bytes": "aGk=", "name": "x.py" })
        } else {
            data.clone()
        };
        match memory::run_remember(&state, data, "code", &opts).await {
            Err(SdkError::Validation(message)) => {
                assert!(message.starts_with(expected), "{opts}: {message}");
            }
            other => panic!("{opts}: expected a validation error, got {other:?}"),
        }
    }
}
