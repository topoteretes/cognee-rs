//! Integration tests for `GET /api/v1/datasets/{id}/schema` and
//! `PUT /api/v1/datasets/{id}/schema`.
//!
//! These tests exercise the real dataset-configuration persistence path.

mod support;

use axum::{body::Body, http::Request, http::StatusCode};
use cognee_database::AclDb;
use tower::ServiceExt;

use support::{
    bearer_header, body_json, build_auth_required_test_state, build_permissions_state,
    ensure_principal, permissions_db, seed_dataset, seed_perm_user, test_router,
};

// ─── GET /schema ──────────────────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_get_schema_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state.clone()).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Authenticated without a config row → `{"graph_schema": null, "custom_prompt": null}`.
#[tokio::test]
async fn test_get_schema_returns_nulls_when_no_config_row() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "schema_get@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let db = permissions_db(&state);

    let dataset_id = uuid::Uuid::new_v4();
    seed_dataset(db, dataset_id, user.id, None, "schema-get").await;
    ensure_principal(db, user.id, "user").await;
    AclDb::grant_permission(db, user.id, dataset_id, "read")
        .await
        .expect("grant read");
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", auth_header.clone())
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["graph_schema"].is_null(), "unexpected body: {body}");
    assert!(body["custom_prompt"].is_null(), "unexpected body: {body}");
}

/// Unknown dataset or missing ACL → 404.
#[tokio::test]
async fn test_get_schema_returns_404_when_dataset_not_accessible() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "schema_get_404@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let app = test_router(state.clone()).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", &auth_header)
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ─── PUT /schema ──────────────────────────────────────────────────────────────

/// No auth → 401.
#[tokio::test]
async fn test_put_schema_no_auth_returns_401() {
    let (state, _) = build_auth_required_test_state().await;
    let app = test_router(state.clone()).await;

    let dataset_id = uuid::Uuid::new_v4();
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"graph_schema":{}}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// PUT persists the row, returns Python-parity `{"status": "ok"}`, and the
/// follow-up GET returns the saved fields.
#[tokio::test]
async fn test_put_schema_round_trip_persists_fields() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "schema_put@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let db = permissions_db(&state);

    let dataset_id = uuid::Uuid::new_v4();
    seed_dataset(db, dataset_id, user.id, None, "schema-put").await;
    ensure_principal(db, user.id, "user").await;
    AclDb::grant_permission(db, user.id, dataset_id, "read")
        .await
        .expect("grant read");
    AclDb::grant_permission(db, user.id, dataset_id, "write")
        .await
        .expect("grant write");
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", &auth_header)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"graphSchema":{"nodes":[{"name":"Person"}]},"customPrompt":"Extract people."}"#,
        ))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "ok", "unexpected body: {body}");

    let get_req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", &auth_header)
        .body(Body::empty())
        .expect("request");
    let get_resp = test_router(state.clone())
        .await
        .oneshot(get_req)
        .await
        .expect("response");
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = body_json(get_resp).await;
    assert_eq!(
        get_body["graph_schema"],
        serde_json::json!({"nodes":[{"name":"Person"}]})
    );
    assert_eq!(get_body["custom_prompt"], "Extract people.");
}

/// A second PUT that omits one field preserves the previously stored value.
#[tokio::test]
async fn test_put_schema_preserves_omitted_fields_on_update() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "schema_patch@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let db = permissions_db(&state);

    let dataset_id = uuid::Uuid::new_v4();
    seed_dataset(db, dataset_id, user.id, None, "schema-patch").await;
    ensure_principal(db, user.id, "user").await;
    AclDb::grant_permission(db, user.id, dataset_id, "read")
        .await
        .expect("grant read");
    AclDb::grant_permission(db, user.id, dataset_id, "write")
        .await
        .expect("grant write");
    let app = test_router(state.clone()).await;

    let first_req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", &auth_header)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"graphSchema":{"type":"object"},"customPrompt":"Extract people."}"#,
        ))
        .expect("request");

    let first_resp = app.clone().oneshot(first_req).await.expect("response");
    assert_eq!(first_resp.status(), StatusCode::OK);

    let second_req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", &auth_header)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"customPrompt":"Extract people carefully."}"#,
        ))
        .expect("request");

    let second_resp = app.oneshot(second_req).await.expect("response");
    assert_eq!(second_resp.status(), StatusCode::OK);

    let get_req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", &auth_header)
        .body(Body::empty())
        .expect("request");
    let get_resp = test_router(state)
        .await
        .oneshot(get_req)
        .await
        .expect("response");
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = body_json(get_resp).await;
    assert_eq!(
        get_body["graph_schema"],
        serde_json::json!({"type":"object"})
    );
    assert_eq!(get_body["custom_prompt"], "Extract people carefully.");
}

/// Invalid JSON types in the payload are rejected by the extractor.
#[tokio::test]
async fn test_put_schema_rejects_invalid_payload_returns_422() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "schema_invalid@example.com", "Str0ng!Pass#1").await;
    let auth_header = bearer_header(&user, &state);
    let db = permissions_db(&state);

    let dataset_id = uuid::Uuid::new_v4();
    seed_dataset(db, dataset_id, user.id, None, "schema-invalid").await;
    ensure_principal(db, user.id, "user").await;
    AclDb::grant_permission(db, user.id, dataset_id, "write")
        .await
        .expect("grant write");
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", auth_header)
        .header("content-type", "application/json")
        .body(Body::from(r#"{"customPrompt":42}"#))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

/// Missing write ACL is masked as 404.
#[tokio::test]
async fn test_put_schema_without_write_acl_returns_404() {
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "schema_owner@example.com", "Str0ng!Pass#1").await;
    let caller = seed_perm_user(&state, "schema_no_write@example.com", "Str0ng!Pass#1").await;
    let owner_auth = bearer_header(&owner, &state);
    let caller_auth = bearer_header(&caller, &state);
    let db = permissions_db(&state);

    let dataset_id = uuid::Uuid::new_v4();
    seed_dataset(db, dataset_id, owner.id, None, "schema-no-write").await;
    ensure_principal(db, owner.id, "user").await;
    ensure_principal(db, caller.id, "user").await;
    AclDb::grant_permission(db, owner.id, dataset_id, "read")
        .await
        .expect("grant owner read");
    AclDb::grant_permission(db, owner.id, dataset_id, "write")
        .await
        .expect("grant owner write");
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", caller_auth)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"graphSchema":{"nodes":[]},"customPrompt":"x"}"#,
        ))
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let owner_req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", owner_auth)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"graphSchema":{"nodes":[]},"customPrompt":"x"}"#,
        ))
        .expect("request");
    let owner_resp = test_router(state.clone())
        .await
        .oneshot(owner_req)
        .await
        .expect("response");
    assert_eq!(owner_resp.status(), StatusCode::OK);
}
