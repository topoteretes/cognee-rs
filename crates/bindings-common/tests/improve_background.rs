#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `improve` with `runInBackground` returns the running result at once; the
//! run finishes as a process-wide background task that
//! `waitForBackgroundTasks` waits on. The pending count is process-global, so
//! this file holds one test.

use serde_json::json;
use tempfile::TempDir;

use cognee::config::Settings;
use cognee_bindings_common::{HandleState, SdkError};

fn settings_under(dir: &TempDir) -> Settings {
    let root = dir.path();
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
            ("llm.api_key".to_string(), json!("sk-test")),
        ]
        .into(),
        data_root_directory: root.join("data").to_string_lossy().into_owned(),
        system_root_directory: root.join("sys").to_string_lossy().into_owned(),
        ..Settings::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn background_improve_returns_running_and_is_awaited_by_wait() {
    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));

    let result = cognee_bindings_common::ops::memory::run_improve(
        &state,
        &json!({
            "datasetName": "bg_ds",
            "sessionIds": ["", "s1"],
            "runInBackground": true,
            "buildGlobalContextIndex": true,
            "buildTruthSubspace": true,
        }),
    )
    .await
    .expect("a background improve returns its running result");
    assert_eq!(result["status"], "running", "{result}");
    assert_eq!(result["background"], true, "{result}");
    assert_eq!(result["finished"], false, "{result}");
    assert_eq!(result["datasetName"], "bg_ds", "{result}");
    assert_eq!(result["sessionIds"], json!(["s1"]), "{result}");
    assert_eq!(result["stages"], json!([]), "{result}");
    assert!(result["datasetId"].is_string(), "{result}");

    let waited = cognee_bindings_common::ops::lifecycle::wait_for_background_tasks(
        &json!({ "timeoutSeconds": 60 }),
    )
    .await
    .expect("wait");
    assert_eq!(waited["finished"], true, "{waited}");
    assert_eq!(waited["pending"], 0, "{waited}");

    // A bad `feedbackAlpha` or an unknown `datasetId` is refused before
    // anything is spawned, as the synchronous path refuses them.
    let err = cognee_bindings_common::ops::memory::run_improve(
        &state,
        &json!({ "datasetName": "bg_ds", "runInBackground": true, "feedbackAlpha": 5.0 }),
    )
    .await
    .expect_err("an out-of-range alpha is rejected");
    assert!(matches!(err, SdkError::Validation(_)), "{err:?}");
    let err = cognee_bindings_common::ops::memory::run_improve(
        &state,
        &json!({
            "datasetName": "bg_ds",
            "runInBackground": true,
            "datasetId": "00000000-0000-0000-0000-0000000000aa",
        }),
    )
    .await
    .expect_err("an unknown dataset id is rejected");
    assert!(matches!(err, SdkError::Validation(_)), "{err:?}");
    assert_eq!(cognee::pending_background_tasks(), 0);

    // The same flags run synchronously reach the library: neither build stage
    // is skipped for want of an opt-in.
    let sync = cognee_bindings_common::ops::memory::run_improve(
        &state,
        &json!({
            "datasetName": "bg_ds",
            "sessionIds": ["s1"],
            "buildGlobalContextIndex": true,
            "buildTruthSubspace": true,
        }),
    )
    .await;
    if let Ok(sync) = sync {
        for stage in sync["stages"].as_array().expect("stages") {
            let name = stage["stage"].as_str().unwrap_or_default();
            if name == "global_context_index" || name == "build_truth_subspace" {
                let reason = stage["reason"].as_str().unwrap_or_default();
                assert!(!reason.contains("opt"), "{name} not enabled: {stage}");
            }
        }
    }

    // An invalid flag is refused before anything is spawned.
    let err = cognee_bindings_common::ops::memory::run_improve(
        &state,
        &json!({ "datasetName": "bg_ds", "runInBackground": "yes" }),
    )
    .await
    .expect_err("a non-boolean flag is rejected");
    assert!(matches!(err, SdkError::Validation(_)), "{err:?}");
}
