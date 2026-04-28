//! Integration tests for `POST /api/v1/sync` and `GET /api/v1/sync/status`.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use cognee_database::{SeaOrmSyncOperationRepository, SyncOperationRepository};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn sync_status_no_running_returns_zero() {
    let state = support::build_permissions_state().await;
    let user = support::seed_perm_user(&state, "user@x.com", "pw").await;
    let bearer = support::bearer_header(&user, &state);
    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sync/status")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["has_running_sync"], false);
    assert_eq!(body["running_sync_count"], 0);
}

#[tokio::test]
async fn sync_status_reflects_db_running_row() {
    let state = support::build_permissions_state().await;
    let user = support::seed_perm_user(&state, "user2@x.com", "pw").await;
    let db = state.components().expect("components").database.clone();
    let repo = SeaOrmSyncOperationRepository::new(Arc::new((*db).clone()));
    let run_id = Uuid::new_v4().to_string();
    repo.create_operation(&run_id, &[Uuid::new_v4()], &["alpha".to_string()], user.id)
        .await
        .expect("create");
    repo.mark_started(&run_id).await.expect("started");
    repo.update_progress(&run_id, 33).await.expect("progress");

    let bearer = support::bearer_header(&user, &state);
    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/sync/status")
        .header("Authorization", bearer)
        .body(Body::empty())
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["has_running_sync"], true);
    assert_eq!(body["running_sync_count"], 1);
    let latest = &body["latest_running_sync"];
    assert_eq!(latest["run_id"], run_id);
    assert_eq!(latest["progress_percentage"], 33);
}

#[tokio::test]
async fn post_sync_with_no_writable_datasets_returns_400_error_envelope() {
    let state = support::build_permissions_state().await;
    let user = support::seed_perm_user(&state, "user3@x.com", "pw").await;
    let bearer = support::bearer_header(&user, &state);
    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/sync")
        .header("Authorization", bearer)
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = support::body_json(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("At least one dataset")
    );
    // Strict parity: no `detail` field on this envelope.
    assert!(body.get("detail").is_none());
}

#[tokio::test]
async fn second_post_returns_409_conflict_with_existing_run_id() {
    let state = support::build_permissions_state().await;
    let user = support::seed_perm_user(&state, "user4@x.com", "pw").await;
    let db = state.components().expect("components").database.clone();
    let repo = SeaOrmSyncOperationRepository::new(Arc::new((*db).clone()));

    let existing_run = Uuid::new_v4().to_string();
    repo.create_operation(&existing_run, &[Uuid::new_v4()], &["alpha".into()], user.id)
        .await
        .expect("create");

    let bearer = support::bearer_header(&user, &state);
    let app = support::test_router(state).await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/sync")
        .header("Authorization", bearer)
        .header("Content-Type", "application/json")
        .body(Body::from("{}"))
        .expect("req");
    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = support::body_json(resp).await;
    assert_eq!(body["error"], "Sync operation already in progress");
    assert_eq!(body["details"]["run_id"], existing_run);
    assert_eq!(body["details"]["status"], "already_running");
}
