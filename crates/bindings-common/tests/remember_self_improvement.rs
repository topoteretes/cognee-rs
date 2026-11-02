#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The binding-level `remember` runs its automatic improve by default.
//!
//! Python's `remember(self_improvement=True)` improves unless told not to, and
//! the bindings forward `opts.selfImprovement` with the same default. The
//! improve itself needs an LLM, so these tests observe it through the host
//! admission check, which `remember` consults exactly when an improve is about
//! to start. The check is process-global, so this file holds one test.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tempfile::TempDir;

use cognee::api::{clear_auto_improve_admission, register_auto_improve_admission};
use cognee::config::Settings;
use cognee_bindings_common::HandleState;

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
async fn remember_improves_by_default_and_not_when_switched_off() {
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    register_auto_improve_admission(Arc::new(move |_ctx| {
        counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Some("test_veto".to_string()) })
    }));

    let dir = TempDir::new().expect("tempdir");
    let state = HandleState::from_settings(settings_under(&dir));
    let input = json!({ "type": "text", "text": "Alice knows Bob." });

    // A session-scoped remember stores into the session and does not cognify,
    // so the only thing left to run is the improve.
    let default = cognee_bindings_common::ops::memory::run_remember(
        &state,
        input.clone(),
        "si_ds",
        &json!({ "sessionId": "si-session" }),
    )
    .await
    .expect("remember");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "no `selfImprovement` option: the improve must be attempted"
    );
    assert_eq!(default["improve_skipped"], "test_veto", "{default}");

    let off = cognee_bindings_common::ops::memory::run_remember(
        &state,
        input,
        "si_ds",
        &json!({ "sessionId": "si-session", "selfImprovement": false }),
    )
    .await
    .expect("remember");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "`selfImprovement: false` must not attempt an improve"
    );
    assert!(off.get("improve_skipped").is_none(), "{off}");
    assert!(off.get("improve").is_none(), "{off}");

    clear_auto_improve_admission();
}
