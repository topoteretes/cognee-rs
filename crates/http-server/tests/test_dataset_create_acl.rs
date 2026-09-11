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

/// Scenario: a create whose grant failed left the dataset row behind; the
/// caller retries the same name once the ACL backend is healthy.
/// Expected: the retry repairs the missing grants. The dataset row and its ACL
/// rows cannot be written in one transaction, so the handler grants on the
/// already-exists arm too — otherwise the short-circuit return would answer
/// 200 forever while the ACL rows stayed missing, which is the state the whole
/// fix exists to make unreachable.
/// Verification: create the row directly (simulating the half-finished create,
/// with no grants), POST the same name, assert 200 and that all four grants
/// now exist.
#[tokio::test]
async fn a_retry_repairs_a_dataset_left_without_grants() {
    let mock = Arc::new(MockAclDb::new());
    let acl: Arc<dyn AclDb> = Arc::clone(&mock) as Arc<dyn AclDb>;
    let state = build_state_with_acl(acl).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    let owner = default_test_user_id();

    // The wreckage of a create whose grant failed: a row, and no ACL for it.
    let dataset_id = cognee_ingestion::generate_dataset_id("half_created", owner, None);
    IngestDb::create_dataset(
        db.as_ref(),
        cognee_models::Dataset::new("half_created".to_string(), owner, None, dataset_id),
    )
    .await
    .expect("seed the half-created dataset");
    assert_eq!(mock.grant_count(), 0, "precondition: no grants exist yet");

    let app = build_router(state).await.expect("router");
    let resp = oneshot_request(app, create_request("half_created")).await;
    assert_eq!(resp.status(), 200, "the retry must succeed");

    for perm in cognee_database::ops::acl::PERMISSION_NAMES {
        assert!(
            mock.has_grant(owner, dataset_id, perm),
            "retrying the create must repair the missing '{perm}' grant"
        );
    }
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
