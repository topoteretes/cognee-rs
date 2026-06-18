#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P5 §5: HTTP-level coverage of the roles surface in
//! `/api/v1/permissions`.
//!
//! Focus: the **owner-vs-admin asymmetry** documented in
//! `routers/permissions.md §2.13`. The repository test verifies
//! `is_tenant_admin` arithmetic; here we confirm the HTTP layer plumbs the
//! correct gate to each route:
//!
//! - `POST /roles?role_name=` (§2.7) — owner-only. An admin-role caller is
//!   rejected with the canonical `403 {detail: ...}` envelope.
//! - `POST /users/{user_id}/roles?role_id=` (§2.10) — owner-only. Same gate.
//! - `GET /tenants/{t}/roles/{r}/users` (§2.3) — admin-allowed; the assigned
//!   user shows up in the response.
//! - Cross-tenant isolation: tenant-A's owner cannot create roles in tenant B
//!   (their *current tenant* gate routes the call to A, not B; even if they
//!   had a path-param target, the `is_tenant_admin(B)` check would reject
//!   them).

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use uuid::Uuid;

use support::{
    bearer_header, build_permissions_state, oneshot_request, permissions_db, seed_perm_user,
    seed_role, seed_tenant, seed_user_role, seed_user_tenant_membership, set_current_tenant,
    test_router,
};

#[tokio::test]
async fn create_role_owner_succeeds() {
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "owner1@example.com", "Str0ng!Pass#1").await;

    // Owner owns a tenant, current tenant = that tenant.
    let tenant_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-A").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    set_current_tenant(permissions_db(&state), owner.id, Some(tenant_id)).await;

    let auth = bearer_header(&owner, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/roles?role_name=editor")
        .header("Authorization", auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    assert_eq!(
        body["message"],
        serde_json::json!("Role created for tenant")
    );
    assert!(body["role_id"].is_string());
    assert_eq!(body["tenant_id"].as_str().unwrap(), tenant_id.to_string());
}

#[tokio::test]
async fn create_role_admin_role_caller_returns_403() {
    // Owner-only — even a tenant admin cannot create roles
    // (`routers/permissions.md §2.7` + §6.3). The caller has the `admin` role
    // in the tenant but is not `tenants.owner_id`.
    let state = build_permissions_state().await;

    let owner = seed_perm_user(&state, "owner2@example.com", "Str0ng!Pass#1").await;
    let admin_user = seed_perm_user(&state, "admin@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-B").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), admin_user.id, tenant_id).await;

    // Give admin_user the canonical "admin" role.
    let admin_role_id = Uuid::new_v4();
    seed_role(permissions_db(&state), admin_role_id, tenant_id, "admin").await;
    seed_user_role(permissions_db(&state), admin_user.id, admin_role_id).await;

    set_current_tenant(permissions_db(&state), admin_user.id, Some(tenant_id)).await;

    let auth = bearer_header(&admin_user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/permissions/roles?role_name=editor")
        .header("Authorization", auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "admin role caller must be rejected — POST /roles is owner-only"
    );
    let body = support::body_json(resp).await;
    assert!(
        body.get("detail").is_some(),
        "expected canonical {{detail}} envelope, got: {body}"
    );
}

#[tokio::test]
async fn assign_role_owner_only_admin_caller_returns_403() {
    // `POST /users/{u}/roles?role_id=` is owner-only per §2.10.
    let state = build_permissions_state().await;

    let owner = seed_perm_user(&state, "owner3@example.com", "Str0ng!Pass#1").await;
    let admin_user = seed_perm_user(&state, "admin3@example.com", "Str0ng!Pass#1").await;
    let target = seed_perm_user(&state, "target3@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-C").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), admin_user.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), target.id, tenant_id).await;

    // admin_user has the admin role.
    let admin_role_id = Uuid::new_v4();
    seed_role(permissions_db(&state), admin_role_id, tenant_id, "admin").await;
    seed_user_role(permissions_db(&state), admin_user.id, admin_role_id).await;

    // Another role to assign to `target`.
    let editor_role_id = Uuid::new_v4();
    seed_role(permissions_db(&state), editor_role_id, tenant_id, "editor").await;

    let auth = bearer_header(&admin_user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/permissions/users/{}/roles?role_id={}",
            target.id, editor_role_id
        ))
        .header("Authorization", auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "POST /users/{{u}}/roles is owner-only — admin must be 403"
    );
}

#[tokio::test]
async fn list_users_in_role_returns_assigned_user() {
    // Round-trip via owner: seed the role, assign a user, then GET should
    // surface them via the §2.3 wire shape.
    let state = build_permissions_state().await;

    let owner = seed_perm_user(&state, "owner4@example.com", "Str0ng!Pass#1").await;
    let assigned = seed_perm_user(&state, "assigned@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-D").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), assigned.id, tenant_id).await;

    let role_id = Uuid::new_v4();
    seed_role(permissions_db(&state), role_id, tenant_id, "editor").await;
    seed_user_role(permissions_db(&state), assigned.id, role_id).await;

    let auth = bearer_header(&owner, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/v1/permissions/tenants/{tenant_id}/roles/{role_id}/users"
        ))
        .header("Authorization", auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = support::body_json(resp).await;
    let arr = body.as_array().expect("array");
    assert_eq!(arr.len(), 1, "expected 1 user in role, got {body}");
    // Per §2.3, the wire field name is `name` populated from `user.email`.
    assert_eq!(arr[0]["id"].as_str().unwrap(), assigned.id.to_string());
    assert_eq!(arr[0]["name"].as_str().unwrap(), "assigned@example.com");
}

#[tokio::test]
async fn cross_tenant_admin_cannot_list_other_tenant_roles() {
    // Tenant-A admin attempting `GET /tenants/{tenantB}/roles` → 403.
    // Cross-tenant isolation per `routers/permissions.md §3` (tenant-scoping).
    let state = build_permissions_state().await;

    let owner_a = seed_perm_user(&state, "ownerA@example.com", "Str0ng!Pass#1").await;
    let admin_a = seed_perm_user(&state, "adminA@example.com", "Str0ng!Pass#1").await;
    let owner_b = seed_perm_user(&state, "ownerB@example.com", "Str0ng!Pass#1").await;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_a, owner_a.id, "tenant-A").await;
    seed_tenant(permissions_db(&state), tenant_b, owner_b.id, "tenant-B").await;

    seed_user_tenant_membership(permissions_db(&state), owner_a.id, tenant_a).await;
    seed_user_tenant_membership(permissions_db(&state), admin_a.id, tenant_a).await;
    seed_user_tenant_membership(permissions_db(&state), owner_b.id, tenant_b).await;

    let admin_role_in_a = Uuid::new_v4();
    seed_role(permissions_db(&state), admin_role_in_a, tenant_a, "admin").await;
    seed_user_role(permissions_db(&state), admin_a.id, admin_role_in_a).await;

    let auth = bearer_header(&admin_a, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/permissions/tenants/{tenant_b}/roles"))
        .header("Authorization", auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "tenant-A admin must not be able to list tenant-B roles"
    );
}
