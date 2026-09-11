#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for the owner ACL grant in `POST /api/v1/datasets`.
//!
//! The handler used to create the dataset row and then grant the owner's four
//! permissions best-effort — `let _ = acl.ensure_principal(..)` and a
//! `tracing::warn!` on a failed `grant_permission` — returning 200 either way.
//!
//! That is not a cosmetic difference. Once an `AclDb` is wired,
//! `SearchOrchestrator::readable_dataset_ids` consults
//! `authorized_dataset_ids_with_roles(requester, "read")` and nothing else
//! (correctly — Python requires a live grant too), while `GET /v1/datasets`
//! falls back to ownership. A swallowed grant therefore produced a dataset the
//! owner could see in the listing and got a 403 for from `POST /v1/search`.
//!
//! The grant now goes through the same
//! `cognee_database::ops::acl::grant_all_permissions_on_dataset_via_trait`
//! helper `cognee::api::datasets::create_authorized_dataset` uses, and its
//! error propagates.

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use cognee_database::{AclDb, IngestDb};
use cognee_http_server::build_router;
use cognee_test_utils::MockAclDb;
use support::{body_json, build_state_with_acl, default_test_user_id, oneshot_request};

fn create_request(name: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/datasets")
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"name":"{name}"}}"#)))
        .expect("request")
}

/// Scenario: an `AclDb` is wired and every `grant_permission` call fails.
/// Expected: the request fails instead of answering 200 with a dataset the
/// owner cannot read.
/// Verification: POST a new dataset name, assert the status is not 2xx.
#[tokio::test]
async fn create_dataset_fails_when_the_owner_grant_fails() {
    let acl: Arc<dyn AclDb> = Arc::new(MockAclDb::failing_grants("acl backend is down"));
    let state = build_state_with_acl(Arc::clone(&acl)).await;
    let app = build_router(state).await.expect("router");

    let resp = oneshot_request(app, create_request("grant_fails")).await;

    assert!(
        !resp.status().is_success(),
        "a failed owner grant must fail the create; got {} — the old handler \
         returned 200 and left the dataset unreadable",
        resp.status()
    );
}

/// Scenario: an `AclDb` is wired and grants succeed.
/// Expected: 200, and the owner holds all four permissions on the new dataset
/// — the same set `create_authorized_dataset` grants, since both now call the
/// one shared helper.
/// Verification: POST, read the returned id back, assert every permission in
/// `PERMISSION_NAMES` is present for the caller.
#[tokio::test]
async fn create_dataset_grants_the_owner_all_four_permissions() {
    let mock = Arc::new(MockAclDb::new());
    let acl: Arc<dyn AclDb> = Arc::clone(&mock) as Arc<dyn AclDb>;
    let state = build_state_with_acl(acl).await;
    let app = build_router(state).await.expect("router");

    let resp = oneshot_request(app, create_request("grant_ok")).await;
    assert_eq!(resp.status(), 200, "create must succeed");

    let body = body_json(resp).await;
    let dataset_id: uuid::Uuid = body
        .get("id")
        .and_then(|v| v.as_str())
        .expect("response carries the dataset id")
        .parse()
        .expect("dataset id is a uuid");

    let owner = default_test_user_id();
    for perm in cognee_database::ops::acl::PERMISSION_NAMES {
        assert!(
            mock.has_grant(owner, dataset_id, perm),
            "owner must hold '{perm}' on the created dataset"
        );
    }
}

/// Scenario: the grant fails, so the handler errors — but the dataset row was
/// already written.
/// Expected: the row is rolled back. The row and its ACL rows cannot share a
/// transaction, so without a compensating delete the failed create would leave
/// an orphan, and because the handler short-circuits on an existing name every
/// later POST would answer 200 while the ACL rows stayed missing — permanently
/// re-creating the exact state this fix exists to prevent.
/// Verification: POST with a failing ACL, assert the error, then assert no row
/// with that name survives.
#[tokio::test]
async fn a_failed_grant_rolls_the_dataset_row_back() {
    let acl: Arc<dyn AclDb> = Arc::new(MockAclDb::failing_grants("acl backend is down"));
    let state = build_state_with_acl(acl).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    let owner = default_test_user_id();

    let app = build_router(state).await.expect("router");
    let resp = oneshot_request(app, create_request("rolled_back")).await;
    assert!(!resp.status().is_success(), "the create must fail");

    let leftover = IngestDb::get_dataset_by_name(db.as_ref(), "rolled_back", owner, None)
        .await
        .expect("lookup");
    assert!(
        leftover.is_none(),
        "a failed grant must not leave the dataset row behind — an orphan here is \
         unreachable by every later POST, which short-circuits on the name"
    );
}

/// Scenario: an owner's `read` grant was deliberately revoked; they POST the
/// same dataset name again.
/// Expected: the existing row comes back untouched and the revocation stands.
/// Re-granting on the already-exists arm would turn an idempotent create into
/// an ACL reset — a privilege-restoration path, and a real one now that the
/// orchestrator denies an ungranted owner by name as well as by id.
/// Verification: create normally, revoke `read`, POST again, assert 200 and
/// that `read` is still absent.
#[tokio::test]
async fn re_creating_an_existing_dataset_does_not_restore_a_revoked_grant() {
    let mock = Arc::new(MockAclDb::new());
    let acl: Arc<dyn AclDb> = Arc::clone(&mock) as Arc<dyn AclDb>;
    let state = build_state_with_acl(Arc::clone(&acl)).await;
    let app = build_router(state).await.expect("router");
    let owner = default_test_user_id();

    let resp = oneshot_request(app.clone(), create_request("revoked")).await;
    assert_eq!(resp.status(), 200);
    let dataset_id: uuid::Uuid = body_json(resp)
        .await
        .get("id")
        .and_then(|v| v.as_str())
        .expect("dataset id")
        .parse()
        .expect("uuid");

    acl.revoke_permission(owner, dataset_id, "read")
        .await
        .expect("revoke");
    assert!(!mock.has_grant(owner, dataset_id, "read"));

    let resp = oneshot_request(app, create_request("revoked")).await;
    assert_eq!(resp.status(), 200, "the idempotent create still succeeds");
    assert!(
        !mock.has_grant(owner, dataset_id, "read"),
        "POSTing an existing dataset must not silently restore a revoked grant"
    );
}

/// Scenario: no `AclDb` is wired — OSS single-user mode.
/// Expected: the create still succeeds. `create_authorized_dataset` would
/// return `AclNotConfigured` here, which is why the handler branches on
/// whether an ACL is wired rather than always taking the authorized path.
/// Verification: build the default (ACL-less) state, POST, assert 200 and that
/// the row landed in the metadata DB.
#[tokio::test]
async fn create_dataset_succeeds_without_an_acl_db() {
    let state = support::build_p4_state(None, None, None).await;
    let db = Arc::clone(
        &state
            .components()
            .expect("components are wired")
            .database
            .clone(),
    );
    let app = build_router(state).await.expect("router");

    let resp = oneshot_request(app, create_request("oss_no_acl")).await;
    assert_eq!(
        resp.status(),
        200,
        "OSS single-user mode wires no AclDb; creating a dataset must still work"
    );

    let owner = default_test_user_id();
    let found = IngestDb::get_dataset_by_name(db.as_ref(), "oss_no_acl", owner, None)
        .await
        .expect("lookup");
    assert!(found.is_some(), "the dataset row must have been written");
}
