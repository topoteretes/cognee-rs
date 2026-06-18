#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `POST /api/v1/notebooks/{id}/{cell_id}/run`.
//!
//! Stage A returned a hard-coded 501. Stage B (this file) covers:
//! - 401 when unauthenticated
//! - 404 when the notebook does not exist (404 beats 501)
//! - 400 when the request body is missing the required `content` field
//! - 200 + populated `RunCodeOutcomeDTO` when a runner is wired into
//!   `ComponentHandles` (mocked here so the test doesn't depend on python3)
//! - 501 regression-guard: a request that reaches the runner MUST NOT return
//!   the legacy `NOTEBOOK_RUN_NOT_IMPLEMENTED` envelope
//!
//! The 501 envelope still surfaces when the runner is left unwired — that's
//! intentionally preserved as a backwards-compat opt-out for embedders that
//! do NOT want to expose code execution.

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

use cognee_http_server::notebook_runner::{NotebookRunner, RunCellOutcome, RunnerError};

use support::{
    bearer_header, build_auth_required_test_state, build_notebooks_state, seed_perm_user,
    test_router,
};

// ─── Test-only mock runner (does NOT spawn python3) ──────────────────────────

struct MockOkRunner;

#[async_trait]
impl NotebookRunner for MockOkRunner {
    async fn run_cell(
        &self,
        code: &str,
        _timeout: Duration,
    ) -> Result<RunCellOutcome, RunnerError> {
        // Echo back the code on the print_output channel so tests can
        // verify the handler actually forwarded `body.content`.
        Ok(RunCellOutcome {
            print_output: vec![format!("EXEC:{}", code)],
            error: None,
        })
    }
}

/// Mutate `state.lib.notebook_runner` to inject the mock. Safe because
/// `ComponentHandles` is wrapped in `Arc` and we only call this on a fresh
/// state built within a test.
fn wire_mock_runner(state: &mut cognee_http_server::AppState) {
    // Replace the `Arc<ComponentHandles>` with a clone where `notebook_runner`
    // is set. `ComponentHandles` derives `Clone`.
    if let Some(handles) = state.lib.as_ref() {
        let mut new_handles = (**handles).clone();
        new_handles.notebook_runner = Some(Arc::new(MockOkRunner) as Arc<dyn NotebookRunner>);
        state.lib = Some(Arc::new(new_handles));
    }
}

// ─── Auth guard ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_cell_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state).await;

    let nb_id = Uuid::new_v4();
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"print(1)"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ─── 404 beats 501 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn run_cell_nonexistent_notebook_returns_404() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "run_404@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let fake_nb = Uuid::new_v4();
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{fake_nb}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"print(1)"}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body: Value = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    };
    assert_eq!(body["error"], "Notebook not found");
}

// ─── Stage B: success path (200 + populated DTO) ─────────────────────────────

#[tokio::test]
async fn run_cell_existing_notebook_with_runner_returns_200() {
    let (mut state, _) = build_notebooks_state().await;
    wire_mock_runner(&mut state);

    let user = seed_perm_user(&state, "run_200@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    // Create a notebook first.
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Runnable","cells":[]}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let created: Value = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    };
    let nb_id = created["id"].as_str().expect("id").to_owned();

    // Now run a cell.
    let app = test_router(state).await;
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"print(42)"}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body: Value = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    };
    assert!(body["result"].is_array());
    let arr = body["result"].as_array().expect("result array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0], "EXEC:print(42)");
    assert!(body["error"].is_null());
}

// ─── Stage B 501 regression guard ───────────────────────────────────────────

/// When a runner IS wired, the handler MUST NOT return the legacy 501
/// stub envelope — that's the Tier-3 headline requirement.
#[tokio::test]
async fn run_cell_with_runner_does_not_return_501() {
    let (mut state, _) = build_notebooks_state().await;
    wire_mock_runner(&mut state);

    let user = seed_perm_user(&state, "run_no501@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Runnable","cells":[]}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    let created: Value = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    };
    let nb_id = created["id"].as_str().expect("id").to_owned();

    let app = test_router(state).await;
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"anything"}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::NOT_IMPLEMENTED,
        "wired runner must not surface the Stage-A 501 envelope"
    );
}

// ─── Stage B: 501 still surfaces when runner is unwired ──────────────────────
//
// Embedders that intentionally do NOT wire a notebook runner get the legacy
// Stage-A envelope back — opt-out for builds that disable code execution.

#[tokio::test]
async fn run_cell_without_runner_still_returns_501() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "run_501@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    // Create a notebook first.
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Runnable","cells":[]}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let created: Value = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    };
    let nb_id = created["id"].as_str().expect("id").to_owned();

    // Now run a cell → 501 because no runner is wired.
    let app = test_router(state).await;
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"content":"print(42)"}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);

    // Verify exact byte output — field order: detail first, then code.
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body_str = std::str::from_utf8(&bytes).expect("utf8");
    assert_eq!(
        body_str,
        r#"{"detail":"Notebook cell execution is not implemented in this build","code":"NOTEBOOK_RUN_NOT_IMPLEMENTED"}"#,
    );
}

#[tokio::test]
async fn run_cell_missing_code_field_returns_400() {
    let (state, _) = build_notebooks_state().await;
    let user = seed_perm_user(&state, "run_400@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);

    // Create a notebook.
    let app = test_router(state.clone()).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/notebooks")
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"name":"Runnable2","cells":[]}"#))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    let nb_id: String = {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let v: Value = serde_json::from_slice(&bytes).expect("json");
        v["id"].as_str().expect("id").to_owned()
    };

    // Run with empty body → 400.
    let app = test_router(state).await;
    let cell_id = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/notebooks/{nb_id}/{cell_id}/run"))
        .header("Authorization", &auth)
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
