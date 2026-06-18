#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration tests for `/api/v1/users` (me, by-id CRUD, get-user-id).
//!
//! Covers:
//! - GET /api/v1/users/me
//! - PATCH /api/v1/users/me (safe=True)
//! - GET /api/v1/users/{id} (superuser only)
//! - PATCH /api/v1/users/{id} (superuser only)
//! - DELETE /api/v1/users/{id} (superuser only, 204)
//! - POST /api/v1/users/get-user-id

mod support;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use support::{
    bearer_header, body_json, build_auth_test_state, seed_superuser, seed_user, test_router,
};

// ─── GET /api/v1/users/me ────────────────────────────────────────────────────

#[tokio::test]
async fn get_users_me_returns_full_user_read() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "usersme@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/users/me")
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    // Full UserReadDTO: must include id, email, is_active, is_superuser, is_verified
    assert_eq!(body["email"], "usersme@example.com");
    assert!(body["id"].is_string());
    assert_eq!(body["is_active"], true);
    assert_eq!(body["is_superuser"], false);
    assert_eq!(body["is_verified"], true);
}

// ─── PATCH /api/v1/users/me (safe=True) ──────────────────────────────────────

#[tokio::test]
async fn patch_users_me_updates_email() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "patchme@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    let payload = serde_json::json!({"email": "patchme_new@example.com"});
    let req = Request::builder()
        .method("PATCH")
        .uri("/api/v1/users/me")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["email"], "patchme_new@example.com");
}

/// safe=True: even if the payload includes is_superuser=true, it must be ignored.
#[tokio::test]
async fn patch_users_me_ignores_is_superuser() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "safepatch@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    // Try to escalate privileges via /users/me — safe=True must strip it
    let payload = serde_json::json!({"is_superuser": true});
    let req = Request::builder()
        .method("PATCH")
        .uri("/api/v1/users/me")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    // The request may succeed (200) but is_superuser must remain false
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(
        body["is_superuser"], false,
        "safe=True must not allow privilege escalation via /users/me"
    );
}

// ─── GET /api/v1/users/{id} ──────────────────────────────────────────────────

#[tokio::test]
async fn get_user_by_id_requires_superuser() {
    let (state, _events) = build_auth_test_state().await;
    let regular = seed_user(&state, "regular_get@example.com", "Str0ng!Pass#1").await;
    let target = seed_user(&state, "target_get@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&regular, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/users/{}", target.id))
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_user_by_id_superuser_succeeds() {
    let (state, _events) = build_auth_test_state().await;
    let admin = seed_superuser(&state, "admin_get@example.com", "Str0ng!Pass#1").await;
    let target = seed_user(&state, "getbyid@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&admin, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/users/{}", target.id))
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["email"], "getbyid@example.com");
}

#[tokio::test]
async fn get_user_by_id_not_found_returns_404() {
    let (state, _events) = build_auth_test_state().await;
    let admin = seed_superuser(&state, "admin_notfound@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&admin, &state);
    let app = test_router(state).await;

    let random_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/users/{random_id}"))
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ─── PATCH /api/v1/users/{id} ────────────────────────────────────────────────

#[tokio::test]
async fn patch_user_by_id_superuser_can_change_is_superuser() {
    let (state, _events) = build_auth_test_state().await;
    let admin = seed_superuser(&state, "admin_patch@example.com", "Str0ng!Pass#1").await;
    let target = seed_user(&state, "patchbyid@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&admin, &state);
    let app = test_router(state).await;

    let payload = serde_json::json!({"is_superuser": true});
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/v1/users/{}", target.id))
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert_eq!(body["is_superuser"], true);
}

#[tokio::test]
async fn patch_user_by_id_requires_superuser() {
    let (state, _events) = build_auth_test_state().await;
    let regular = seed_user(&state, "regular_patch@example.com", "Str0ng!Pass#1").await;
    let target = seed_user(&state, "patchbyid_target@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&regular, &state);
    let app = test_router(state).await;

    let payload = serde_json::json!({"email": "hacked@example.com"});
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/v1/users/{}", target.id))
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ─── DELETE /api/v1/users/{id} ───────────────────────────────────────────────

#[tokio::test]
async fn delete_user_by_id_superuser_returns_204() {
    let (state, _events) = build_auth_test_state().await;
    let admin = seed_superuser(&state, "admin_del@example.com", "Str0ng!Pass#1").await;
    let target = seed_user(&state, "deletebyid@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&admin, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/users/{}", target.id))
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_user_by_id_requires_superuser() {
    let (state, _events) = build_auth_test_state().await;
    let regular = seed_user(&state, "regular_del@example.com", "Str0ng!Pass#1").await;
    let target = seed_user(&state, "delbyid_target@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&regular, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/users/{}", target.id))
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ─── POST /api/v1/users/get-user-id ──────────────────────────────────────────

#[tokio::test]
async fn get_user_id_returns_uuid_for_known_email() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "lookup@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    let payload = serde_json::json!({"email": "lookup@example.com"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/users/get-user-id")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_json(resp).await;
    assert!(body["user_id"].is_string());
    // The UUID must parse correctly
    let id_str = body["user_id"].as_str().expect("user_id string");
    uuid::Uuid::parse_str(id_str).expect("valid UUID");
}

#[tokio::test]
async fn get_user_id_returns_404_for_unknown_email() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "caller@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    let payload = serde_json::json!({"email": "nobody@example.com"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/users/get-user-id")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body = body_json(resp).await;
    assert_eq!(body["detail"], "User not found");
}

// ─── PATCH /api/v1/users/me — email conflict ─────────────────────────────────

/// Updating /users/me with an email that already belongs to another user returns 400.
#[tokio::test]
async fn patch_users_me_conflicting_email_returns_400() {
    let (state, _events) = build_auth_test_state().await;
    seed_user(&state, "taken@example.com", "Str0ng!Pass#1").await;
    let user = seed_user(&state, "changer@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    // Try to steal the other user's email
    let payload = serde_json::json!({"email": "taken@example.com"});
    let req = Request::builder()
        .method("PATCH")
        .uri("/api/v1/users/me")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    // String-detail variant (UPDATE_USER_EMAIL_ALREADY_EXISTS)
    assert_eq!(body["detail"], "UPDATE_USER_EMAIL_ALREADY_EXISTS");
}

// ─── PATCH /api/v1/users/me — weak password ──────────────────────────────────

/// Updating /users/me with a password containing the email returns 400 structured-detail.
#[tokio::test]
async fn patch_users_me_invalid_password_returns_400_structured() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "weakpatch@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    // Password that contains the email — invalid per fastapi-users rules
    let payload = serde_json::json!({"password": "weakpatch@example.com_suffix"});
    let req = Request::builder()
        .method("PATCH")
        .uri("/api/v1/users/me")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = body_json(resp).await;
    // Structured-detail variant (UPDATE_USER_INVALID_PASSWORD)
    assert_eq!(body["detail"]["code"], "UPDATE_USER_INVALID_PASSWORD");
    assert!(body["detail"]["reason"].is_string());
}

// ─── DELETE /api/v1/users/{id} — default user guard ─────────────────────────

/// A superuser cannot delete the well-known default user (id = 00000...0).
#[tokio::test]
async fn delete_default_user_returns_403() {
    let (state, _events) = build_auth_test_state().await;
    let admin = seed_superuser(&state, "admin_nodelete@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&admin, &state);
    let app = test_router(state).await;

    // The default user UUID is all-zeros
    let default_id = uuid::Uuid::nil();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/users/{default_id}"))
        .header("authorization", bearer)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "deleting the default user must be forbidden"
    );
}

// ─── GET /api/v1/users/get-user-id — case-sensitive ─────────────────────────

/// Email lookup is case-sensitive — Python uses `WHERE email = ?` without LOWER().
#[tokio::test]
async fn get_user_id_case_mismatch_returns_404() {
    let (state, _events) = build_auth_test_state().await;
    let user = seed_user(&state, "casesensitive@example.com", "Str0ng!Pass#1").await;
    let bearer = bearer_header(&user, &state);
    let app = test_router(state).await;

    // Wrong casing — must not match
    let payload = serde_json::json!({"email": "CaseSensitive@EXAMPLE.COM"});
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/users/get-user-id")
        .header("authorization", bearer)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).expect("serialize")))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "case-mismatched email must return 404"
    );
}
