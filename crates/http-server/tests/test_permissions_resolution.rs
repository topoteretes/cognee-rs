#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! P5 §5: HTTP-level end-to-end resolution test for the 8-step `user_can`
//! algorithm (`tenants.md §5.1`).
//!
//! The repository test (`crates/database/tests/permissions_repository.rs`)
//! covers each branch of the truth table at the SQL layer. This file does the
//! complementary check: a user whose **only** path to a dataset's `write`
//! permission is the **role-default** branch (step 7 in the resolver) can
//! actually call a permission-gated write endpoint over HTTP and have it
//! succeed.
//!
//! That HTTP-layer round-trip is the part the repository test cannot exercise.
//! The endpoint we target is `PUT /api/v1/datasets/{dataset_id}/schema` — the
//! cheapest write-gated route in the read-path / write-path surface (handler
//! checks `check_permission_via_handles(write)` then returns
//! `{"status": "ok"}` without further side effects). Authentication flows via
//! a bearer JWT issued for a user seeded into the unified DB.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use uuid::Uuid;

use support::{
    bearer_header, build_permissions_state, oneshot_request, permissions_db, seed_dataset,
    seed_perm_user, seed_role, seed_role_default_permission, seed_tenant, seed_user_role,
    seed_user_tenant_membership, test_router,
};

#[tokio::test]
async fn role_default_permission_allows_gated_write_endpoint() {
    // Fixture: tenant T owned by `owner`. User `caller` is a member of T and
    // has role `editor`. Role `editor` has a `write` row in
    // `role_default_permissions`. The dataset lives in T.
    //
    // The 8-step resolver at `tenants.md §5.1` ranks step 7 (role default)
    // *after* steps 1–6 — none of which apply to `caller` here — so a
    // successful HTTP write on this dataset *exclusively* exercises the role
    // default branch through the wire.
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "tenant-owner@example.com", "Str0ng!Pass#1").await;
    let caller = seed_perm_user(&state, "role-caller@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-R").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), caller.id, tenant_id).await;

    // Role + role-default-permission(write).
    let role_id = Uuid::new_v4();
    seed_role(permissions_db(&state), role_id, tenant_id, "editor").await;
    seed_user_role(permissions_db(&state), caller.id, role_id).await;
    seed_role_default_permission(permissions_db(&state), role_id, "write").await;

    // Dataset owned by tenant-owner, in tenant T (note: NOT owned by caller —
    // the ownership branch step 8 must NOT match for this test to exclusively
    // hit the role-default branch).
    let dataset_id = Uuid::new_v4();
    seed_dataset(
        permissions_db(&state),
        dataset_id,
        owner.id,
        Some(tenant_id),
        "ds-r",
    )
    .await;

    let auth = bearer_header(&caller, &state);
    let app = test_router(state.clone()).await;

    // Hit a write-gated endpoint (`PUT /api/v1/datasets/{id}/schema`). It
    // calls `check_permission_via_handles(user, dataset, "write")`, which
    // delegates to `PermissionsRepository::user_can` — the 8-step resolver.
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", &auth)
        .header("content-type", "application/json")
        .body(Body::from(r#"{}"#))
        .expect("request");
    let resp = oneshot_request(app.clone(), req).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "role-default `write` must allow the gated endpoint over HTTP"
    );
    let body = support::body_json(resp).await;
    assert_eq!(body, serde_json::json!({"status": "ok"}));
}

#[tokio::test]
async fn no_permission_path_denies_gated_write_endpoint() {
    // Negative complement: same fixture but the caller has NO role and is NOT
    // the dataset owner. Resolver returns false → handler maps the 403 from
    // `check_permission_via_handles` to a 404 "Dataset not found" envelope
    // (this is how `update_dataset_schema` masks ACL denials per the existing
    // datasets-router behavior).
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "owner-X@example.com", "Str0ng!Pass#1").await;
    let caller = seed_perm_user(&state, "no-perm@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-X").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), caller.id, tenant_id).await;

    let dataset_id = Uuid::new_v4();
    seed_dataset(
        permissions_db(&state),
        dataset_id,
        owner.id,
        Some(tenant_id),
        "ds-x",
    )
    .await;

    let auth = bearer_header(&caller, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/datasets/{dataset_id}/schema"))
        .header("Authorization", &auth)
        .header("content-type", "application/json")
        .body(Body::from(r#"{}"#))
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "no-permission caller must be denied (router masks 403 → 404 'Dataset not found')"
    );
}
