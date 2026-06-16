#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P5 §5: HTTP-level coverage of `POST /api/v1/permissions/datasets/{principal_id}`.
//!
//! Repository-level invariants for the 8-step `user_can` resolver are exercised
//! in `crates/database/tests/permissions_repository.rs`. Here we focus on the
//! wire-shape contract that the repository test cannot exercise:
//!
//! - Successful grant returns the canonical `{"message": "Permission assigned to principal"}`
//!   body with status 200, and the second user can subsequently read the dataset
//!   per `user_can` (round-trip through HTTP).
//! - Mixed allow/deny dataset list silently skips datasets the caller cannot
//!   `share` (Python parity per `routers/permissions.md §2.6` and §6.2).
//! - Unknown `permission_name` query param → 400 `{detail: ...}`.
//! - Empty body → 200 success (Python parity per §6.1).

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use uuid::Uuid;

use support::{
    bearer_header, build_permissions_state, oneshot_request, permissions_db, permissions_repo,
    seed_dataset, seed_perm_user, test_router,
};

#[tokio::test]
async fn grant_acl_success_response_shape_and_round_trip() {
    let state = build_permissions_state().await;
    let granter = seed_perm_user(&state, "granter@example.com", "Str0ng!Pass#1").await;
    let grantee = seed_perm_user(&state, "grantee@example.com", "Str0ng!Pass#1").await;

    // Granter owns a dataset (ownership branch grants `share`).
    let dataset_id = Uuid::new_v4();
    seed_dataset(permissions_db(&state), dataset_id, granter.id, None, "ds1").await;

    let auth = bearer_header(&granter, &state);
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/permissions/datasets/{}?permission_name=read",
            grantee.id
        ))
        .header("Authorization", &auth)
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"["{dataset_id}"]"#)))
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = support::body_json(resp).await;
    assert_eq!(
        body,
        serde_json::json!({"message": "Permission assigned to principal"}),
        "Python-parity success body must match exactly"
    );

    // Round-trip: grantee can now `read` the dataset via `user_can`.
    let repo = permissions_repo(&state);
    let allowed = repo.user_can(grantee.id, dataset_id, "read").await.unwrap();
    assert!(allowed, "grantee should have read after ACL grant");
}

#[tokio::test]
async fn grant_acl_silently_skips_datasets_caller_cannot_share() {
    let state = build_permissions_state().await;
    let granter = seed_perm_user(&state, "granter2@example.com", "Str0ng!Pass#1").await;
    let grantee = seed_perm_user(&state, "grantee2@example.com", "Str0ng!Pass#1").await;
    let other = seed_perm_user(&state, "other@example.com", "Str0ng!Pass#1").await;

    // Owned by granter (granter has `share` via ownership branch).
    let mine = Uuid::new_v4();
    seed_dataset(permissions_db(&state), mine, granter.id, None, "mine").await;
    // Owned by `other` — granter has *no* `share` on this one.
    let theirs = Uuid::new_v4();
    seed_dataset(permissions_db(&state), theirs, other.id, None, "theirs").await;

    let auth = bearer_header(&granter, &state);
    let app = test_router(state.clone()).await;

    // Grant `read` on both. Per §2.6 silent-skip parity, the response stays 200
    // and the message body is unchanged — but only `mine` actually gets an ACL.
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/permissions/datasets/{}?permission_name=read",
            grantee.id
        ))
        .header("Authorization", &auth)
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"["{mine}", "{theirs}"]"#)))
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let repo = permissions_repo(&state);
    let read_mine = repo.user_can(grantee.id, mine, "read").await.unwrap();
    let read_theirs = repo.user_can(grantee.id, theirs, "read").await.unwrap();
    assert!(
        read_mine,
        "grantee should have read on caller-owned dataset"
    );
    assert!(
        !read_theirs,
        "grantee should NOT have read on dataset the caller cannot share — silent-skip parity"
    );
}

#[tokio::test]
async fn grant_acl_unknown_permission_returns_400() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "u@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let some_principal = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/permissions/datasets/{some_principal}?permission_name=admin"
        ))
        .header("Authorization", &auth)
        .header("content-type", "application/json")
        .body(Body::from(r#"[]"#))
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = support::body_json(resp).await;
    assert!(
        body.get("detail").is_some(),
        "expected canonical {{detail}} envelope, got: {body}"
    );
}

#[tokio::test]
async fn grant_acl_empty_body_returns_200_success() {
    // Python parity per `routers/permissions.md §6.1`: empty list → 200 success,
    // no 400.
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "u2@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let some_principal = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/permissions/datasets/{some_principal}?permission_name=read"
        ))
        .header("Authorization", &auth)
        .header("content-type", "application/json")
        .body(Body::from(r#"[]"#))
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(
        body,
        serde_json::json!({"message": "Permission assigned to principal"})
    );
}
