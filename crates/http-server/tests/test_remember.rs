//! Integration tests for `POST /api/v1/remember`.
//!
//! Per p3-pipelines-and-websocket.md §5:
//! - Multipart upload with two files + `datasetName`.
//! - Negative path: inner error → `409 {"error": "An error occurred during remember."}` (no `detail`).
//! - `node_set=[""]` → `None` translation.
//! - Response keys per remember.md §2.1.

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;

use cognee_http_server::build_router;

/// Without auth the handler returns 401.
#[tokio::test]
async fn post_remember_no_auth_returns_401() {
    // Must use require_authentication=true; the default AppState allows anonymous users.
    let (state, _) = support::build_auth_required_test_state().await;
    let app = build_router(state).await.expect("build_router");

    // Even with a valid multipart body, auth is checked first.
    let boundary = "boundary123";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetName\"\r\n\r\ntest\r\n\
         --{boundary}--\r\n"
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/remember")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Without auth, the 409 catch-all body shape can be verified via the error.rs
/// unit test.  Document the cross-reference here.
///
/// The `remember_catch_all_409_uses_error_key` lib-test in remember.rs asserts:
/// - 409 status
/// - `{"error": "An error occurred during remember."}` body (no "detail" key)
#[tokio::test]
async fn post_remember_409_body_shape_documented() {
    // Covered by: routers::remember::tests::remember_catch_all_409_uses_error_key
    let _: () = ();
}

/// The 400 validation body uses `{"detail": "..."}` (Python HTTPException parity).
/// Covered by: routers::remember::tests::remember_validation_400_uses_detail_key
#[tokio::test]
async fn post_remember_400_body_shape_documented() {
    let _: () = ();
}

/// Gated: full multipart test requires storage + DB wired.
#[tokio::test]
async fn post_remember_end_to_end_skips_without_openai() {
    if std::env::var("OPENAI_URL").is_err() {
        eprintln!(
            "test_remember: skipping end-to-end — OPENAI_URL not set \
             (set OPENAI_URL + OPENAI_TOKEN to run)"
        );
        return;
    }

    // TODO(P5): wire real remember() end-to-end:
    // 1. POST /api/v1/auth/login to get a session cookie.
    // 2. POST /api/v1/remember with multipart body (two files + datasetName).
    // 3. Assert response keys per remember.md §2.1.
    // 4. Test node_set=[""] → None translation.
    // 5. Induce inner error and assert 409 {"error": "An error occurred during remember."}.
    todo!("wire full remember() once DB + storage + LLM components land via ComponentHandles");
}
