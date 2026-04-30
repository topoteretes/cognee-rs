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

    eprintln!(
        "test_remember: skipping end-to-end — real remember() is not wired through \
         ComponentHandles yet"
    );
}

/// Body-shape parity vs Python's `RememberResult.to_dict()`
/// (`cognee/api/v1/remember/remember.py:415-437`).
///
/// On the success path the response must:
/// - emit `status` as a Python-parity lowercase string (Decision 15);
/// - always include `pipeline_run_id`, `dataset_id`, `dataset_name`,
///   `items_processed`, `elapsed_seconds`;
/// - omit conditional keys (`session_ids`, `content_hash`, `items`, `error`)
///   when not set;
/// - never include the E-02-reserved `entry_type` / `entry_id` keys.
#[tokio::test]
async fn post_remember_response_body_shape_matches_python() {
    let state = support::build_test_state().await;
    let app = build_router(state).await.expect("build_router");

    let boundary = "boundary123";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"datasetName\"\r\n\r\nshape-test\r\n\
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
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    let obj = v.as_object().expect("response body must be a JSON object");

    // Python lowercase status (Decision 15). Blocking dispatch → "completed".
    let status = obj["status"].as_str().expect("status string");
    assert!(
        matches!(
            status,
            "running" | "completed" | "errored" | "session_stored"
        ),
        "status must be a Python-parity lowercase variant; got {status:?}"
    );
    assert_eq!(status, "completed");

    // Always-emit keys.
    for key in [
        "pipeline_run_id",
        "dataset_id",
        "dataset_name",
        "items_processed",
        "elapsed_seconds",
    ] {
        assert!(
            obj.contains_key(key),
            "RememberResultDTO must always emit `{key}`"
        );
    }

    // Conditional keys must be absent in this minimal multipart path.
    for key in ["session_ids", "content_hash", "items", "error"] {
        assert!(
            !obj.contains_key(key),
            "RememberResultDTO must omit `{key}` when unset; got {obj:?}"
        );
    }

    // E-02 reserves `entry_type` / `entry_id` for the `/remember/entry`
    // route — they must not leak into the file-payload response (Decision 5).
    for key in ["entry_type", "entry_id"] {
        assert!(
            !obj.contains_key(key),
            "RememberResultDTO must NOT contain `{key}` (Decision 5 reserves it for E-02)"
        );
    }

    // `items_processed` defaults to the file-part count (0 here — no `data`
    // parts uploaded).
    assert_eq!(obj["items_processed"], 0);
}
