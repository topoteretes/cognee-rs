#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P5 §5: HTTP-level tenant-lifecycle coverage for `/api/v1/permissions`.
//!
//! Repository-level invariants (e.g. `create_tenant` membership, owner-removal
//! validation) live in `crates/database/tests/permissions_repository.rs`. This
//! file pins the wire-level invariants the repository test cannot exercise:
//!
//! - End-to-end lifecycle: `POST /tenants` → caller is owner & current tenant
//!   → `POST /users/{u}/tenants` adds a second user → `GET /tenants/{t}/users`
//!   surfaces them → `POST /tenants/select` switches caller's current →
//!   `GET /tenants/me` reflects membership.
//! - `DELETE /tenants/{t}/users/{u}` against the tenant owner → `400` with the
//!   canonical `{detail: ...}` envelope (`CogneeValidationError` parity).
//! - Duplicate `tenant_name` on `POST /tenants` → `400` (`EntityAlreadyExists`
//!   maps to `BadRequest` per `routers/permissions.md §2.8`).

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use uuid::Uuid;

use support::{
    bearer_header, build_permissions_state, oneshot_request, seed_perm_user, test_router,
};

#[tokio::test]
async fn full_tenant_lifecycle_via_http() {
    let state = build_permissions_state().await;

    let owner = seed_perm_user(&state, "owner@example.com", "Str0ng!Pass#1").await;
    let other = seed_perm_user(&state, "other@example.com", "Str0ng!Pass#1").await;

    let auth_owner = bearer_header(&owner, &state);
    let app = test_router(state.clone()).await;

    // 1. Owner creates a tenant.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/tenants?tenant_name=acme")
        .header("Authorization", &auth_owner)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["message"], serde_json::json!("Tenant created."));
    let tenant_id: Uuid = body["tenant_id"]
        .as_str()
        .expect("tenant_id string")
        .parse()
        .expect("uuid");

    // 2. `GET /tenants/me` reflects the new membership.
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/permissions/tenants/me")
        .header("Authorization", &auth_owner)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_str().unwrap(), tenant_id.to_string());
    assert_eq!(arr[0]["name"].as_str().unwrap(), "acme");

    // 3. Owner adds the second user via `POST /users/{u}/tenants?tenant_id=`.
    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/permissions/users/{}/tenants?tenant_id={}",
            other.id, tenant_id
        ))
        .header("Authorization", &auth_owner)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["message"], serde_json::json!("User added to tenant"));

    // 4. `GET /tenants/{t}/users` lists both members.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/permissions/tenants/{tenant_id}/users"))
        .header("Authorization", &auth_owner)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    let users = body.as_array().expect("array");
    assert_eq!(users.len(), 2, "expected 2 users in tenant, got {body}");
    let emails: Vec<&str> = users.iter().map(|u| u["email"].as_str().unwrap()).collect();
    assert!(emails.contains(&"owner@example.com"));
    assert!(emails.contains(&"other@example.com"));

    // 5. `POST /tenants/select` returns the chosen tenant id back.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/tenants/select")
        .header("Authorization", &auth_owner)
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"tenant_id":"{tenant_id}"}}"#)))
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(body["message"], serde_json::json!("Tenant selected."));
    assert_eq!(body["tenant_id"].as_str().unwrap(), tenant_id.to_string());
}

#[tokio::test]
async fn delete_tenant_owner_returns_400_validation() {
    // §2.12: removing the tenant owner must return 400
    // (`CogneeValidationError` parity).
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "lone-owner@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&owner, &state);
    let app = test_router(state.clone()).await;

    // Create the tenant via HTTP so the owner_id is wired correctly.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/tenants?tenant_name=lonely")
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    let tenant_id: Uuid = body["tenant_id"].as_str().unwrap().parse().unwrap();

    // Now try to delete the owner from their own tenant → must 400.
    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/permissions/tenants/{}/users/{}",
            tenant_id, owner.id
        ))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "removing the tenant owner must return 400 (Python's CogneeValidationError)"
    );
    let body = support::body_json(resp).await;
    assert!(
        body.get("detail").is_some(),
        "expected canonical {{detail}} envelope, got: {body}"
    );
}

#[tokio::test]
async fn duplicate_tenant_name_returns_400() {
    // §2.8: duplicate `tenant_name` → `EntityAlreadyExistsError` → 400.
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "dup@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state.clone()).await;

    // First create succeeds.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/tenants?tenant_name=duplicate-name")
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app.clone(), req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Second attempt with the same name → 400.
    // (Note: `EntityAlreadyExists` maps to `BadRequest` per the router's
    // `map_permissions_error`. The cross-router 409-on-duplicate convention
    // some specs reference does not apply here — Python uses 400 for this
    // class of error.)
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/tenants?tenant_name=duplicate-name")
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "duplicate tenant name must surface as 400"
    );
    let body = support::body_json(resp).await;
    assert!(
        body.get("detail").is_some(),
        "expected canonical {{detail}} envelope, got: {body}"
    );
}

#[tokio::test]
async fn select_unknown_tenant_returns_404() {
    // §2.9: selecting a tenant the caller is not a member of → 404
    // (`TenantNotFoundError("User is not part of the tenant.")` parity).
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "selector@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let unknown = Uuid::new_v4();
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/tenants/select")
        .header("Authorization", &auth)
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"tenant_id":"{unknown}"}}"#)))
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = support::body_json(resp).await;
    assert!(body.get("detail").is_some());
}

// `select_null` already covered separately by `test_permissions_select_null.rs`.
