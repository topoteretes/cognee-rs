#![cfg(any())] // cognee-http-server gated on oss-split branch (T2-move §4 S2).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! HTTP-level tests for the three new DELETE (revoke) endpoints in
//! `/api/v1/permissions`:
//!
//! - `DELETE /datasets/{principal_id}?permission_name=` — revoke ACL grant
//!   (`revoke_dataset_permission`). Per-dataset `share` gate, silent-skip,
//!   empty-body, unknown-permission → 400.
//! - `DELETE /users/{user_id}/roles?role_id=` — remove a user from a role
//!   (`remove_user_from_role`). Admin-or-owner gate
//!   (`has_user_management_permission`); unknown role → 404; non-admin → 403.
//! - `DELETE /roles/{role_id}` — delete a role + cascade
//!   (`delete_role`). Admin-or-owner gate; cascade verifies
//!   user_roles + role-principal ACLs + role + principal rows are gone.
//!
//! All tests use in-memory SQLite (no external services).

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use uuid::Uuid;

use support::{
    bearer_header, build_permissions_state, oneshot_request, permissions_db, permissions_repo,
    seed_dataset, seed_perm_user, seed_role, seed_tenant, seed_user_role,
    seed_user_tenant_membership, test_router,
};

// ── revoke ACL ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn revoke_acl_round_trip() {
    // Grant read to grantee, then revoke it; confirm user_can(grantee, ds, "read") = false.
    let state = build_permissions_state().await;
    let granter = seed_perm_user(&state, "rv-granter@example.com", "Str0ng!Pass#1").await;
    let grantee = seed_perm_user(&state, "rv-grantee@example.com", "Str0ng!Pass#1").await;

    let dataset_id = Uuid::new_v4();
    seed_dataset(
        permissions_db(&state),
        dataset_id,
        granter.id,
        None,
        "ds-rv1",
    )
    .await;

    let repo = permissions_repo(&state);

    // Grant first (directly via repo so we don't depend on the grant HTTP endpoint).
    repo.grant_acl(grantee.id, dataset_id, "read")
        .await
        .expect("grant_acl");

    let can_before = repo
        .user_can(grantee.id, dataset_id, "read")
        .await
        .expect("user_can");
    assert!(
        can_before,
        "precondition: grantee must have read before revoke"
    );

    let auth = bearer_header(&granter, &state);
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("DELETE")
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
        serde_json::json!({"message": "Permission revoked from principal"}),
        "Python-parity revoke success body must match exactly"
    );

    // Round-trip: grantee no longer has read.
    let can_after = repo
        .user_can(grantee.id, dataset_id, "read")
        .await
        .expect("user_can after revoke");
    assert!(!can_after, "grantee must NOT have read after ACL revoke");
}

#[tokio::test]
async fn revoke_acl_unknown_permission_400() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "rv-badperm@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let some_principal = Uuid::new_v4();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/permissions/datasets/{some_principal}?permission_name=bogus"
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
async fn revoke_acl_empty_body_200() {
    // Empty dataset list → 200 success (Python parity).
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "rv-empty@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let some_principal = Uuid::new_v4();
    let req = Request::builder()
        .method("DELETE")
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
        serde_json::json!({"message": "Permission revoked from principal"})
    );
}

#[tokio::test]
async fn revoke_acl_silently_skips_when_caller_lacks_share() {
    // Caller without share on a dataset → 200 but ACL unchanged (silent-skip parity).
    let state = build_permissions_state().await;
    let caller = seed_perm_user(&state, "rv-noshare@example.com", "Str0ng!Pass#1").await;
    let owner = seed_perm_user(&state, "rv-owner@example.com", "Str0ng!Pass#1").await;
    let grantee = seed_perm_user(&state, "rv-grantee2@example.com", "Str0ng!Pass#1").await;

    // Dataset owned by `owner`; `caller` has no `share` on it.
    let dataset_id = Uuid::new_v4();
    seed_dataset(
        permissions_db(&state),
        dataset_id,
        owner.id,
        None,
        "ds-noshare",
    )
    .await;

    let repo = permissions_repo(&state);
    // Pre-grant read to grantee so we can verify nothing changes.
    repo.grant_acl(grantee.id, dataset_id, "read")
        .await
        .expect("pre-grant");

    let auth = bearer_header(&caller, &state);
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/permissions/datasets/{}?permission_name=read",
            grantee.id
        ))
        .header("Authorization", &auth)
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"["{dataset_id}"]"#)))
        .expect("request");
    let resp = oneshot_request(app, req).await;
    // Silent-skip: still 200, not 403.
    assert_eq!(resp.status(), StatusCode::OK);

    // ACL must be unchanged — grantee still has read.
    let still_can = repo
        .user_can(grantee.id, dataset_id, "read")
        .await
        .expect("user_can");
    assert!(
        still_can,
        "ACL must be unchanged when caller lacks share (silent-skip parity)"
    );
}

// ── remove user from role ────────────────────────────────────────────────────

#[tokio::test]
async fn remove_user_from_role_round_trip() {
    // Owner creates a role, assigns a user, then removes via DELETE → 200;
    // repo confirms user no longer in role.
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "rmrole-owner@example.com", "Str0ng!Pass#1").await;
    let member = seed_perm_user(&state, "rmrole-member@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    let role_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-rmr1").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), member.id, tenant_id).await;
    seed_role(permissions_db(&state), role_id, tenant_id, "editor-rmr").await;
    seed_user_role(permissions_db(&state), member.id, role_id).await;

    // Confirm precondition: member is in the role.
    let repo = permissions_repo(&state);
    let users_before = repo
        .list_users_in_role(tenant_id, role_id)
        .await
        .expect("list_users_in_role before");
    assert_eq!(
        users_before.len(),
        1,
        "precondition: member should be in role"
    );

    let auth = bearer_header(&owner, &state);
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/permissions/users/{}/roles?role_id={}",
            member.id, role_id
        ))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = support::body_json(resp).await;
    assert_eq!(
        body,
        serde_json::json!({"message": "User removed from role"}),
        "Python-parity remove-from-role success body"
    );

    // Verify: member no longer in role.
    let users_after = repo
        .list_users_in_role(tenant_id, role_id)
        .await
        .expect("list_users_in_role after");
    assert!(
        users_after.is_empty(),
        "user_roles must be empty after remove_user_from_role"
    );
}

#[tokio::test]
async fn remove_user_from_role_non_admin_403() {
    // A caller who is neither tenant owner nor has an admin role → 403.
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "rmrole-owner2@example.com", "Str0ng!Pass#1").await;
    let plain_user = seed_perm_user(&state, "rmrole-plain@example.com", "Str0ng!Pass#1").await;
    let member = seed_perm_user(&state, "rmrole-member2@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    let role_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-rmr2").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), plain_user.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), member.id, tenant_id).await;
    seed_role(permissions_db(&state), role_id, tenant_id, "editor-rmr2").await;
    seed_user_role(permissions_db(&state), member.id, role_id).await;

    // `plain_user` is in the tenant but has no admin role.
    let auth = bearer_header(&plain_user, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/permissions/users/{}/roles?role_id={}",
            member.id, role_id
        ))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "non-admin, non-owner caller must get 403 on remove_user_from_role"
    );
    let body = support::body_json(resp).await;
    assert!(
        body.get("detail").is_some(),
        "expected canonical {{detail}} envelope, got: {body}"
    );
}

#[tokio::test]
async fn remove_user_from_role_admin_role_member_succeeds() {
    // A caller who has the "admin" role (but is not the tenant owner) CAN remove
    // users — Python uses has_user_management_permission (admin-or-owner).
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "rmrole-owner3@example.com", "Str0ng!Pass#1").await;
    let admin_user = seed_perm_user(&state, "rmrole-admin@example.com", "Str0ng!Pass#1").await;
    let member = seed_perm_user(&state, "rmrole-member3@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    let admin_role_id = Uuid::new_v4();
    let target_role_id = Uuid::new_v4();

    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-rmr3").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), admin_user.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), member.id, tenant_id).await;

    seed_role(permissions_db(&state), admin_role_id, tenant_id, "admin").await;
    seed_user_role(permissions_db(&state), admin_user.id, admin_role_id).await;

    seed_role(
        permissions_db(&state),
        target_role_id,
        tenant_id,
        "editor-rmr3",
    )
    .await;
    seed_user_role(permissions_db(&state), member.id, target_role_id).await;

    let auth = bearer_header(&admin_user, &state);
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/permissions/users/{}/roles?role_id={}",
            member.id, target_role_id
        ))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "admin-role caller must be allowed to remove user from role (admin-or-owner parity)"
    );
}

#[tokio::test]
async fn remove_user_from_role_unknown_role_404() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "rmrole-norole@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let random_role_id = Uuid::new_v4();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/api/v1/permissions/users/{}/roles?role_id={}",
            user.id, random_role_id
        ))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "unknown role_id must return 404"
    );
}

// ── delete role (cascade) ────────────────────────────────────────────────────

#[tokio::test]
async fn delete_role_cascade() {
    // Create a role with a user member and a role-held ACL grant. Delete the role.
    // Assert: role gone, user_roles gone, role-principal ACLs gone.
    use cognee_database::entities::{acl, role, user_role};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "delrole-owner@example.com", "Str0ng!Pass#1").await;
    let member = seed_perm_user(&state, "delrole-member@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    let role_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();

    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-del1").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), member.id, tenant_id).await;
    seed_role(permissions_db(&state), role_id, tenant_id, "to-delete").await;
    seed_user_role(permissions_db(&state), member.id, role_id).await;
    seed_dataset(
        permissions_db(&state),
        dataset_id,
        owner.id,
        None,
        "ds-del1",
    )
    .await;

    // Grant the role an ACL on the dataset (role as principal).
    let repo = permissions_repo(&state);
    repo.grant_acl(role_id, dataset_id, "read")
        .await
        .expect("grant_acl to role");

    let auth = bearer_header(&owner, &state);
    let app = test_router(state.clone()).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/permissions/roles/{role_id}"))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let body = support::body_json(resp).await;
    assert_eq!(
        body,
        serde_json::json!({"message": "Role deleted"}),
        "Python-parity delete-role success body"
    );

    // All cascade assertions go through the raw DB so we don't depend on
    // repo methods that require the role to still exist.
    use sea_orm::PaginatorTrait;
    let db = permissions_db(&state);
    let role_hex = role_id.simple().to_string();

    // Cascade check 1: role row gone (via role_tenant_id convenience method).
    let tenant_lookup = repo
        .role_tenant_id(role_id)
        .await
        .expect("role_tenant_id after delete");
    assert!(
        tenant_lookup.is_none(),
        "role row must be deleted (role_tenant_id should return None)"
    );

    // Cascade check 2: user_roles rows gone (direct DB query).
    let user_roles_count = user_role::Entity::find()
        .filter(user_role::Column::RoleId.eq(role_hex.clone()))
        .count(db)
        .await
        .expect("count user_roles");
    assert_eq!(
        user_roles_count, 0,
        "user_roles rows must be cascade-deleted with the role"
    );

    // Cascade check 3: acl rows where principal_id == role_id are gone.
    let acl_count = acl::Entity::find()
        .filter(acl::Column::PrincipalId.eq(role_hex.clone()))
        .count(db)
        .await
        .expect("count acls");
    assert_eq!(
        acl_count, 0,
        "acl rows for the deleted role must be cascade-deleted"
    );

    // Cascade check 4: role row itself gone (double-check via direct query).
    let role_row = role::Entity::find_by_id(role_hex)
        .one(db)
        .await
        .expect("role find");
    assert!(
        role_row.is_none(),
        "role row must be gone after delete_role"
    );
}

#[tokio::test]
async fn delete_role_non_admin_403() {
    let state = build_permissions_state().await;
    let owner = seed_perm_user(&state, "delrole-owner2@example.com", "Str0ng!Pass#1").await;
    let plain = seed_perm_user(&state, "delrole-plain@example.com", "Str0ng!Pass#1").await;

    let tenant_id = Uuid::new_v4();
    let role_id = Uuid::new_v4();
    seed_tenant(permissions_db(&state), tenant_id, owner.id, "tenant-del2").await;
    seed_user_tenant_membership(permissions_db(&state), owner.id, tenant_id).await;
    seed_user_tenant_membership(permissions_db(&state), plain.id, tenant_id).await;
    seed_role(permissions_db(&state), role_id, tenant_id, "cannot-delete").await;

    let auth = bearer_header(&plain, &state);
    let app = test_router(state).await;

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/permissions/roles/{role_id}"))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "non-admin, non-owner must get 403 on delete_role"
    );
}

#[tokio::test]
async fn delete_role_unknown_role_404() {
    let state = build_permissions_state().await;
    let user = seed_perm_user(&state, "delrole-norole@example.com", "Str0ng!Pass#1").await;
    let auth = bearer_header(&user, &state);
    let app = test_router(state).await;

    let random_role_id = Uuid::new_v4();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/permissions/roles/{random_role_id}"))
        .header("Authorization", &auth)
        .body(Body::empty())
        .expect("request");
    let resp = oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "unknown role_id must return 404"
    );
}
