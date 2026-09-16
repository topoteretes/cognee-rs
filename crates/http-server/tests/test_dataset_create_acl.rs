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

/// Scenario: the grant helper gets `read` and `write` in before failing on
/// `delete`, and the revokes succeed.
/// Expected: no grant survives and no row survives. `failing_grants` fails
/// *every* grant, so nothing partial is ever written and the revoke loop is a
/// no-op under it — this is the case that actually needs the cleanup.
/// Verification: POST with a mock that fails only `delete`, assert the error,
/// then assert both the ACL and the metadata row are empty.
#[tokio::test]
async fn a_partial_grant_is_revoked_before_the_row_is_dropped() {
    let mock = Arc::new(MockAclDb::failing_grant_of("delete"));
    let acl: Arc<dyn AclDb> = Arc::clone(&mock) as Arc<dyn AclDb>;
    let state = build_state_with_acl(acl).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    let owner = default_test_user_id();

    let app = build_router(state).await.expect("router");
    let resp = oneshot_request(app, create_request("partial")).await;
    assert!(!resp.status().is_success(), "the create must fail");

    assert_eq!(
        mock.grant_count(),
        0,
        "the grants written before the failure must be revoked — the dataset id is \
         deterministic, so a later create of the same name would inherit them"
    );
    let leftover = IngestDb::get_dataset_by_name(db.as_ref(), "partial", owner, None)
        .await
        .expect("lookup");
    assert!(leftover.is_none(), "the row must be rolled back too");
}

/// Scenario: the ACL store is unreachable, so the compensating revokes fail
/// for the same reason the grant did.
/// Expected: the row is **kept**, and the error names it for manual cleanup.
/// Deleting it while grants survive is the one outcome that poisons a future
/// request: `uuid5(name, owner, tenant)` hands the same id to the next create
/// of that name, which would silently inherit a partial permission set.
/// Verification: fail `delete` grants and all revokes, assert the error, then
/// assert the row is still present and the stale grants are still visible.
#[tokio::test]
async fn the_row_is_kept_when_cleanup_cannot_succeed() {
    let mock = Arc::new(MockAclDb::failing_grant_of("delete").with_failing_revokes());
    let acl: Arc<dyn AclDb> = Arc::clone(&mock) as Arc<dyn AclDb>;
    let state = build_state_with_acl(acl).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    let owner = default_test_user_id();

    let app = build_router(state).await.expect("router");
    let resp = oneshot_request(app, create_request("stuck")).await;
    assert!(!resp.status().is_success(), "the create must fail");

    assert!(
        mock.grant_count() > 0,
        "precondition: some grants landed before the failure and could not be revoked"
    );
    let leftover = IngestDb::get_dataset_by_name(db.as_ref(), "stuck", owner, None)
        .await
        .expect("lookup");
    assert!(
        leftover.is_some(),
        "the row must NOT be deleted while grants on its deterministic id survive — \
         that state is invisible and would be inherited by the next create"
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

// ─── SDK-636: the window between the insert and the grant ────────────────────
//
// The row and its ACL rows cannot share a transaction, so `create_new_dataset`
// writes the row, grants second, and compensates a failed grant by deleting the
// row again. That leaves a window in which the row exists but is not yet usable
// and may still vanish. The tests above all drive it single-threaded, where the
// window is invisible; these two hold a request inside it —
// `MockAclDb::with_gated_first_grant` parks the first grant until released —
// and ask what a second request sees.
//
// Both are about `POST /v1/datasets` against itself. The `POST /v1/add` half of
// the same window is covered in `crates/ingestion/tests/dataset_create_locking.rs`,
// where the ingest path takes the same lock.

/// Scenario: two `POST /v1/datasets` for the same name overlap — the first is
/// parked mid-grant when the second arrives.
/// Expected: the second does not answer at all until the first has finished.
/// The already-exists arm returns the row as-is and deliberately does not
/// re-grant (re-granting would restore revoked permissions), so a second
/// request that observes the row *during* the window answers 200 for a dataset
/// that has no ACL rows yet — a success the caller cannot act on, and one that
/// the rollback arm can invalidate outright.
/// Verification: park request 1 inside its grant, start request 2, and assert
/// request 2 is *still pending* while the window is open. That is the whole
/// observable difference: unsynchronised, it completes immediately. Then
/// release and assert it answers 200 for a fully-granted dataset.
#[tokio::test]
async fn a_concurrent_create_does_not_answer_from_inside_the_grant_window() {
    let (mock, gate) = MockAclDb::new().with_gated_first_grant();
    let mock = Arc::new(mock);
    let acl: Arc<dyn AclDb> = Arc::clone(&mock) as Arc<dyn AclDb>;
    let state = build_state_with_acl(acl).await;
    let app = build_router(state).await.expect("router");

    let first = tokio::spawn(oneshot_request(app.clone(), create_request("contended")));

    // The first request is now inside the window: its row is written, its
    // grants are not.
    gate.wait_until_parked().await;

    let mut second = tokio::spawn(oneshot_request(app, create_request("contended")));

    let answered_early =
        tokio::time::timeout(std::time::Duration::from_millis(250), &mut second).await;
    assert!(
        answered_early.is_err(),
        "the second create answered while the first was still granting — it read a \
         row whose ACL rows do not exist yet"
    );

    gate.release();

    let first = first.await.expect("first request");
    let second = second.await.expect("second request");
    assert_eq!(first.status(), 200, "the first create must succeed");
    assert_eq!(
        second.status(),
        200,
        "the idempotent second create succeeds"
    );

    let dataset_id: uuid::Uuid = body_json(second)
        .await
        .get("id")
        .and_then(|v| v.as_str())
        .expect("response carries the dataset id")
        .parse()
        .expect("dataset id is a uuid");

    let owner = default_test_user_id();
    for perm in cognee_database::ops::acl::PERMISSION_NAMES {
        assert!(
            mock.has_grant(owner, dataset_id, perm),
            "a 200 must describe a committed dataset, but '{perm}' is missing"
        );
    }
}

/// Scenario: the same overlap, but the first request's grant fails, so it rolls
/// the row back.
/// Expected: the second request never answers 200 for that row. This is the
/// window's damaging arm: the second caller holds a success for a dataset the
/// first request deletes a moment later, and every later request re-derives the
/// same deterministic id and hits the same race.
/// Verification: park request 1 inside a failing grant, start request 2,
/// release, and assert neither succeeds and no row survives.
#[tokio::test]
async fn a_concurrent_create_does_not_succeed_on_a_row_that_rolls_back() {
    let (mock, gate) = MockAclDb::failing_grants("acl backend is down").with_gated_first_grant();
    let acl: Arc<dyn AclDb> = Arc::new(mock);
    let state = build_state_with_acl(acl).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    let owner = default_test_user_id();
    let app = build_router(state).await.expect("router");

    let first = tokio::spawn(oneshot_request(app.clone(), create_request("doomed")));
    gate.wait_until_parked().await;
    let mut second = tokio::spawn(oneshot_request(app, create_request("doomed")));

    // Assert the second request is *blocked*, not merely that it fails in the
    // end. Without this it is not a regression test: released early, the
    // unsynchronised handler also reaches a non-2xx — its own grant fails for
    // the same reason — so `!is_success` alone passes either way. Being
    // blocked here is what proves it never saw the doomed row.
    let answered_early =
        tokio::time::timeout(std::time::Duration::from_millis(250), &mut second).await;
    assert!(
        answered_early.is_err(),
        "the second create answered while the first was still granting — it read the \
         row that is about to be rolled back"
    );

    gate.release();

    let first = first.await.expect("first request");
    let second = second.await.expect("second request");

    assert!(
        !first.status().is_success(),
        "a failed owner grant must fail the create"
    );
    assert!(
        !second.status().is_success(),
        "the second create must not succeed on a row the first one deletes; got {} — \
         it read the row from inside the grant window",
        second.status()
    );

    let leftover = IngestDb::get_dataset_by_name(db.as_ref(), "doomed", owner, None)
        .await
        .expect("lookup");
    assert!(
        leftover.is_none(),
        "both creates failed, so no row may survive"
    );
}
