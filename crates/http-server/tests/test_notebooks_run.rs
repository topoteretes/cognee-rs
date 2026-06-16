#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Stage-B integration tests for `POST /api/v1/notebooks/{id}/{cell_id}/run`
//! that actually exercise the `SubprocessRunner` against a real `python3`.
//!
//! These tests are skip-gated on `python3` availability: if `python3 --version`
//! fails, the test logs an `eprintln!` skip notice and returns.

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use tokio::process::Command;
use tower::ServiceExt;
use uuid::Uuid;

use cognee_http_server::notebook_runner::{NotebookRunner, SubprocessRunner};

use support::{bearer_header, build_notebooks_state, seed_perm_user, test_router};

/// Returns true when `python3 --version` exits 0.
async fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Wire a real `SubprocessRunner` with a short wall-clock timeout for the
/// timeout test.
fn wire_runner(state: &mut cognee_http_server::AppState, runner: Arc<dyn NotebookRunner>) {
    if let Some(handles) = state.lib.as_ref() {
        let mut new_handles = (**handles).clone();
        new_handles.notebook_runner = Some(runner);
        state.lib = Some(Arc::new(new_handles));
    }
}

async fn create_notebook_and_get_id(app: axum::Router, auth: &str) -> String {
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"PyExec","cells":[]}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let v: Value = serde_json::from_slice(&bytes).expect("json");
    v["id"].as_str().expect("id").to_owned()
}

// ─── Happy path: print(1+1) → result == "2", error == null ───────────────────

#[tokio::test]
async fn run_cell_with_python3_returns_print_output() {
    if !python3_available().await {
        eprintln!("SKIP run_cell_with_python3_returns_print_output: python3 not on PATH");
        return;
    }

    let (mut state, _) = build_notebooks_state().await;
    let runner: Arc<dyn NotebookRunner> = Arc::new(SubprocessRunner::new());
    wire_runner(&mut state, runner);

    let user = seed_perm_user(&state, "py_ok@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    let nb_id = create_notebook_and_get_id(test_router(state.clone()).await, &auth).await;

    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"print(1+1)"}"#))
        .expect("request");

    let resp = test_router(state).await.oneshot(req).await.expect("resp");
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let v: Value = serde_json::from_slice(&bytes).expect("json");
    let arr = v["result"].as_array().expect("result array");
    assert!(!arr.is_empty(), "expected at least one print_output entry");
    // `print(1+1)` writes `2` — our wrapper captures it as one entry.
    assert!(
        arr.iter().any(|x| x.as_str() == Some("2")),
        "expected '2' in result; got {arr:?}"
    );
    assert!(
        v["error"].is_null(),
        "expected null error; got {}",
        v["error"]
    );
}

// ─── Timeout: sleep(60) is killed within the configured timeout ──────────────

#[tokio::test]
async fn run_cell_timeout_returns_error() {
    if !python3_available().await {
        eprintln!("SKIP run_cell_timeout_returns_error: python3 not on PATH");
        return;
    }

    let (mut state, _) = build_notebooks_state().await;
    // 1 second wall-clock cap.
    let runner: Arc<dyn NotebookRunner> = Arc::new(SubprocessRunner::new());
    wire_runner(&mut state, runner);
    // Override the config's timeout via the state's config Arc — replace
    // the entire Arc<HttpServerConfig> with a clone that has a smaller
    // timeout.
    {
        use cognee_http_server::HttpServerConfig;
        let mut cfg = (*state.config).clone();
        cfg.notebook_run_timeout = Duration::from_millis(750);
        state.config = Arc::new(HttpServerConfig { ..cfg });
    }

    let user = seed_perm_user(&state, "py_timeout@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    let nb_id = create_notebook_and_get_id(test_router(state.clone()).await, &auth).await;

    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"import time; time.sleep(60)"}"#))
        .expect("request");

    let started = std::time::Instant::now();
    let resp = test_router(state).await.oneshot(req).await.expect("resp");
    let elapsed = started.elapsed();
    assert_eq!(resp.status(), StatusCode::OK);
    // Should NOT block for the full 60s.
    assert!(
        elapsed < Duration::from_secs(10),
        "request blocked too long: {elapsed:?}"
    );

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let v: Value = serde_json::from_slice(&bytes).expect("json");
    let err = v["error"].as_str().expect("expected error string");
    assert!(
        err.contains("timed out"),
        "expected timeout error message, got: {err}"
    );
}
