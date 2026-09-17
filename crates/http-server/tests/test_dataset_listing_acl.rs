#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression tests for `GET /api/v1/datasets` agreeing with `POST /v1/search`
//! about who can read what (SDK-637).
//!
//! The handler used to answer from two sources at once. It queried the ACL,
//! and then — guarded on `if datasets.is_empty()`, not on whether an `AclDb`
//! was wired at all — fell back to `list_datasets_by_owner`. Two consequences,
//! both in the permissive direction:
//!
//! 1. A caller whose `read` grants were revoked or never written got the full
//!    ownership listing here, while `SearchOrchestrator::readable_dataset_ids`
//!    (which consults the ACL and nothing else, per PR 214) denied them the
//!    same datasets. Python has no fallback either: `get_datasets_router.py`
//!    returns whatever `get_all_user_permission_datasets(user, "read")` gives,
//!    the empty list included.
//! 2. The fallback carried no tenant predicate, so it listed the caller's rows
//!    across every tenant — where search filters its equivalent fallback on the
//!    requester's tenant, mirroring Python's `dataset.tenant_id ==
//!    user.tenant_id`.
//!
//! The tenant tests drive the no-ACL path on purpose: that is the only path
//! that consults `user.tenant_id`. The ACL path is *not* tenant-filtered here,
//! which is a known divergence from Python — `get_all_user_permission_datasets`
//! applies `dataset.tenant_id == user.tenant_id` to the deduplicated union of
//! direct, tenant and role grants — and it is shared with
//! `SearchOrchestrator::readable_dataset_ids`. Closing it in this handler alone
//! would make the listing stricter than search and re-open the same
//! disagreement in the opposite direction, so it is tracked separately.
//!
//! Every test here is `#[serial]`. One of them overrides the process-global
//! `REQUIRE_AUTHORIZATION`, and `serial_test` only serializes against *other*
//! `#[serial]` tests — leaving the rest parallel let that override leak into a
//! concurrently-running case and flip its result.

mod support;

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::request::Parts;
use cognee_database::{AclDb, DatabaseConnection, IngestDb};
use cognee_http_server::auth::AuthenticatedUser;
use cognee_http_server::auth_resolver::AuthResolver;
use cognee_http_server::components::ComponentHandles;
use cognee_http_server::{AppState, build_router};
use cognee_models::Dataset;
use cognee_search::orchestration::{SearchOrchestrator, SearchTypeRegistry};
use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::SearchType;
use cognee_test_utils::MockAclDb;
use support::{
    RecordingRetriever, body_json, build_component_handles, build_search_db, build_state_with_acl,
    default_test_user_id, oneshot_get,
};
use uuid::Uuid;

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Resolver that authenticates every request as one fixed user.
///
/// The OSS default user is built by `default_user_from_state` and always
/// carries `tenant_id: None`, which makes the tenant predicate a no-op. A
/// tenant-scoped caller can only be produced through an `AuthResolver`, the
/// same seam the closed/cloud build injects its real one through.
struct FixedUser(AuthenticatedUser);

#[async_trait]
impl AuthResolver for FixedUser {
    async fn resolve(&self, _parts: &mut Parts) -> Option<AuthenticatedUser> {
        Some(self.0.clone())
    }
}

fn user_in_tenant(id: Uuid, tenant_id: Option<Uuid>) -> AuthenticatedUser {
    AuthenticatedUser {
        id,
        email: "tenant-user@example.com".into(),
        is_superuser: false,
        is_verified: true,
        is_active: true,
        tenant_id,
        auth_method: cognee_http_server::auth::AuthMethod::DefaultUser,
    }
}

/// Insert a dataset row with an explicit tenant and return its id.
///
/// `support::seed_dataset` hard-codes `tenant_id: None`, which is precisely the
/// value under test here.
async fn seed_dataset_in_tenant(
    db: &DatabaseConnection,
    name: &str,
    owner: Uuid,
    tenant_id: Option<Uuid>,
) -> Uuid {
    let dataset = Dataset::new(name.to_string(), owner, tenant_id, Uuid::new_v4());
    IngestDb::create_dataset(db, dataset)
        .await
        .expect("seed dataset")
        .id
}

/// Build a state with no `AclDb` (the OSS single-user path) whose every request
/// authenticates as `user`.
async fn build_oss_state_as(user: AuthenticatedUser) -> (AppState, Arc<DatabaseConnection>) {
    let db = build_search_db().await;
    let handles = build_component_handles(Arc::clone(&db), None, None, None);
    let mut state = AppState::build(cognee_http_server::HttpServerConfig::default())
        .await
        .expect("build state");
    state.lib = Some(handles);
    state.auth_resolver = Some(Arc::new(FixedUser(user)));
    (state, db)
}

/// The `name` field of every dataset in a `GET /api/v1/datasets` body.
async fn listed_names(resp: axum::response::Response) -> Vec<String> {
    body_json(resp)
        .await
        .as_array()
        .expect("listing is a JSON array")
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(|v| v.as_str())
                .expect("each entry carries a name")
                .to_string()
        })
        .collect()
}

// ─── 1. the ownership fallback no longer fires through a live ACL ────────────

/// Scenario: an `AclDb` is wired, the caller owns a dataset, and the ACL holds
/// no `read` grant for it — the state a revoked grant, or a row written before
/// grants were reliable, leaves behind.
/// Expected: `[]`. Search denies this caller the dataset, so the listing must
/// too; Python returns the empty list here as well.
/// Verification: seed an owned dataset, grant nothing, assert the body is
/// empty rather than listing the row from ownership.
#[tokio::test]
#[serial_test::serial]
async fn listing_is_empty_when_the_acl_holds_no_grant() {
    let acl: Arc<dyn AclDb> = Arc::new(MockAclDb::new());
    let state = build_state_with_acl(Arc::clone(&acl)).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    let owner = default_test_user_id();
    seed_dataset_in_tenant(&db, "ungranted", owner, None).await;

    let app = build_router(state).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;
    assert_eq!(resp.status(), 200, "the listing itself must still succeed");

    assert_eq!(
        listed_names(resp).await,
        Vec::<String>::new(),
        "an AclDb is wired and grants no read on this dataset, so it must not \
         be listed — the old `if datasets.is_empty()` guard read through to \
         ownership and listed a dataset POST /v1/search answers 403 for"
    );
}

/// Scenario: an `AclDb` is wired, the caller holds `read` on one of two owned
/// datasets, and that grant is then revoked.
/// Expected: the dataset drops out of the listing on revocation.
/// Verification: list once with the grant (one entry), revoke, list again
/// (empty) — the `is_empty()` guard made the second listing return *both*
/// datasets, which is more than the caller ever had access to.
#[tokio::test]
#[serial_test::serial]
async fn revoking_read_removes_the_dataset_from_the_listing() {
    let mock = Arc::new(MockAclDb::new());
    let acl: Arc<dyn AclDb> = Arc::clone(&mock) as Arc<dyn AclDb>;
    let state = build_state_with_acl(acl).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    let owner = default_test_user_id();
    let granted = seed_dataset_in_tenant(&db, "granted", owner, None).await;
    seed_dataset_in_tenant(&db, "never_granted", owner, None).await;

    AclDb::ensure_principal(mock.as_ref(), owner, "user")
        .await
        .expect("principal");
    AclDb::grant_permission(mock.as_ref(), owner, granted, "read")
        .await
        .expect("grant read");

    let app = build_router(state.clone()).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;
    assert_eq!(
        listed_names(resp).await,
        vec!["granted".to_string()],
        "with one live grant the listing is exactly that dataset — not the \
         other owned row, which was never granted"
    );

    AclDb::revoke_permission(mock.as_ref(), owner, granted, "read")
        .await
        .expect("revoke read");

    let app = build_router(state).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;
    assert_eq!(
        listed_names(resp).await,
        Vec::<String>::new(),
        "after the revoke the caller has no read grant at all, so the listing \
         is empty — the old guard fell back to ownership here and listed both \
         datasets, including the one that was never granted"
    );
}

/// Scenario: one `AppState` wiring both an `AclDb` and a search orchestrator
/// that consults it, a dataset owned by the caller, and no `read` grant.
/// Expected: `GET /v1/datasets` and `POST /v1/search` give the *same* answer —
/// the listing is empty and the search is a 403. This is the divergence
/// SDK-637 exists to close, asserted end to end rather than one side at a
/// time: before the fix the same state produced a dataset the caller could see
/// listed and was refused by id.
/// Verification: seed an ungranted owned dataset, assert `[]` from the
/// listing and 403 from a search naming that id.
#[tokio::test]
#[serial_test::serial]
async fn the_listing_and_search_agree_when_the_grant_is_missing() {
    let db = build_search_db().await;
    let owner = default_test_user_id();
    let dataset_id = seed_dataset_in_tenant(&db, "ungranted", owner, None).await;

    let acl: Arc<dyn AclDb> = Arc::new(MockAclDb::new());
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(RecordingRetriever::new(SearchType::Chunks)) as Arc<dyn SearchRetriever>);
    let orchestrator = Arc::new(
        SearchOrchestrator::new(registry)
            .with_database(Arc::clone(&db) as Arc<dyn cognee_database::SearchHistoryDb>)
            .with_dataset_resolver(Arc::clone(&db) as Arc<dyn IngestDb>)
            .with_acl_db(Arc::clone(&acl)),
    );

    let base = build_component_handles(Arc::clone(&db), Some(orchestrator), None, None);
    let handles = Arc::new(ComponentHandles {
        acl_db: Some(acl),
        database: Arc::clone(&base.database),
        storage: Arc::clone(&base.storage),
        delete_service: Arc::clone(&base.delete_service),
        ontology_manager: Arc::clone(&base.ontology_manager),
        search_orchestrator: base.search_orchestrator.clone(),
        cloud_client: None,
        llm: None,
        graph_db: None,
        vector_db: None,
        thread_pool: None,
        embedding_engine: None,
        ontology_resolver: None,
        session_store: None,
        session_manager: None,
        checkpoint_store: None,
        responses_client: None,
        transcriber: None,
        notebook_runner: None,
    });
    let mut state = AppState::build(cognee_http_server::HttpServerConfig::default())
        .await
        .expect("build state");
    state.lib = Some(handles);

    let app = build_router(state.clone()).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;
    assert_eq!(
        listed_names(resp).await,
        Vec::<String>::new(),
        "the listing must not offer a dataset the ACL grants no read on"
    );

    let app = build_router(state).await.expect("router");
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({
                "search_type": "CHUNKS",
                "query": "x",
                "dataset_ids": [dataset_id],
            })
            .to_string(),
        ))
        .expect("request");
    let resp = support::oneshot_request(app, req).await;
    assert_eq!(
        resp.status(),
        403,
        "search refuses the same dataset — which is the answer the listing \
         now agrees with, and did not before SDK-637"
    );
}

/// Scenario: an `AclDb` is wired and holds no grant, but the operator set
/// `REQUIRE_AUTHORIZATION=false` — this server's documented "do not enforce the
/// ACL" switch, Python's `ENABLE_BACKEND_ACCESS_CONTROL=false` parity.
/// Expected: the owned dataset is listed. `check_permission_via_handles`
/// honours that switch on every other dataset route (status, data, graph, raw
/// file, write, both deletes), so a listing that ignored it would hide
/// datasets those routes still serve — the same two-endpoints-disagree bug this
/// file exists to prevent, arriving from the other side. Gating the listing on
/// the ACL alone regressed exactly this case, because the old `is_empty()`
/// fallback happened to cover it.
/// Verification: seed an ungranted owned dataset, disable authorization, and
/// assert it is listed.
#[tokio::test]
#[serial_test::serial]
async fn disabling_authorization_restores_the_ownership_listing() {
    let acl: Arc<dyn AclDb> = Arc::new(MockAclDb::new());
    let state = build_state_with_acl(Arc::clone(&acl)).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    seed_dataset_in_tenant(&db, "ungranted", default_test_user_id(), None).await;

    // Process-global, hence `#[serial]`. Restored before the assertions so a
    // panicking assert cannot leak the override into another test.
    // SAFETY: `#[serial]` guarantees no other test in this binary runs
    // concurrently, and nothing here spawns a thread that reads the env.
    unsafe { std::env::set_var("REQUIRE_AUTHORIZATION", "false") };
    let app = build_router(state).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;
    let names = listed_names(resp).await;
    // SAFETY: as above.
    unsafe { std::env::remove_var("REQUIRE_AUTHORIZATION") };

    assert_eq!(
        names,
        vec!["ungranted".to_string()],
        "with REQUIRE_AUTHORIZATION=false the ACL is not enforced, so the \
         listing must fall back to ownership — every other dataset route \
         serves this dataset in that configuration"
    );
}

/// Scenario: the ACL holds a `read` grant whose dataset row no longer exists —
/// a stale grant, not a missing one.
/// Expected: an empty list (there is nothing to list), and the diagnostic must
/// not misreport it. The two states reach the same empty body but need opposite
/// fixes: no grants means the ACL needs a backfill, stale grants mean the ACL
/// needs sweeping, so the handler distinguishes them by grant count.
/// Verification: grant `read` on an id with no row, plus a real owned dataset
/// the caller was never granted, and assert the listing is empty rather than
/// falling back.
#[tokio::test]
#[serial_test::serial]
async fn a_grant_on_a_deleted_dataset_lists_nothing() {
    let mock = Arc::new(MockAclDb::new());
    let acl: Arc<dyn AclDb> = Arc::clone(&mock) as Arc<dyn AclDb>;
    let state = build_state_with_acl(acl).await;
    let db = state
        .components()
        .expect("components are wired")
        .database
        .clone();
    let owner = default_test_user_id();
    seed_dataset_in_tenant(&db, "owned_but_ungranted", owner, None).await;

    let vanished = Uuid::new_v4();
    AclDb::ensure_principal(mock.as_ref(), owner, "user")
        .await
        .expect("principal");
    AclDb::grant_permission(mock.as_ref(), owner, vanished, "read")
        .await
        .expect("grant read on a dataset with no row");

    let app = build_router(state).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;

    assert_eq!(
        listed_names(resp).await,
        Vec::<String>::new(),
        "a grant pointing at no row lists nothing, and must not re-open the \
         ownership fallback for the dataset that was never granted"
    );
}

// ─── 2. the ownership fallback is tenant-scoped ──────────────────────────────

/// Scenario: no `AclDb` (OSS single-user), a caller scoped to tenant A, and
/// two owned datasets — one in tenant A, one in tenant B.
/// Expected: only tenant A's dataset is listed. `list_datasets_by_owner` spans
/// every tenant the owner appears in, and search filters its equivalent
/// fallback on the requester's tenant, so listing tenant B's row here produced
/// a listing entry that search answers 403 for.
/// Verification: seed one row per tenant, assert the listing holds only A's.
#[tokio::test]
#[serial_test::serial]
async fn the_ownership_fallback_excludes_other_tenants() {
    let owner = Uuid::new_v4();
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let (state, db) = build_oss_state_as(user_in_tenant(owner, Some(tenant_a))).await;

    seed_dataset_in_tenant(&db, "in_tenant_a", owner, Some(tenant_a)).await;
    seed_dataset_in_tenant(&db, "in_tenant_b", owner, Some(tenant_b)).await;

    let app = build_router(state).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;

    assert_eq!(
        listed_names(resp).await,
        vec!["in_tenant_a".to_string()],
        "a caller scoped to tenant A must not see tenant B's dataset listed"
    );
}

/// Scenario: the same caller scoped to tenant A, owning a NULL-tenant row —
/// what the CLI and the bindings write into a shared database, and what rows
/// written before a tenant was assigned look like.
/// Expected: not listed. This is the exact asymmetry SDK-637 names: the row is
/// listed by ownership and then refused by search with a 403 by id, or a 422
/// `DatasetNotFound` by name. Python's `dataset.tenant_id == user.tenant_id`
/// excludes it too — `None != Some(tenant_a)`.
/// Verification: seed a NULL-tenant row, assert the listing is empty.
#[tokio::test]
#[serial_test::serial]
async fn the_ownership_fallback_excludes_null_tenant_rows_from_a_tenanted_caller() {
    let owner = Uuid::new_v4();
    let tenant_a = Uuid::new_v4();
    let (state, db) = build_oss_state_as(user_in_tenant(owner, Some(tenant_a))).await;

    seed_dataset_in_tenant(&db, "null_tenant", owner, None).await;

    let app = build_router(state).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;

    assert_eq!(
        listed_names(resp).await,
        Vec::<String>::new(),
        "a caller with a non-null tenant must not see a NULL-tenant row \
         listed, because search refuses that same row"
    );
}

// ─── 3. the OSS path is unchanged ────────────────────────────────────────────

/// Scenario: no `AclDb` and a caller with no tenant — the OSS single-user
/// default, and every row the CLI or bindings write.
/// Expected: every owned dataset is listed regardless of the row's tenant. A
/// `None` tenant means the caller named no tenant, so no predicate applies;
/// this is the case that must not regress, since it is what every OSS install
/// and the whole cross-SDK suite runs as.
/// Verification: seed a NULL-tenant and a tenanted row, assert both list.
#[tokio::test]
#[serial_test::serial]
async fn an_untenanted_caller_still_sees_every_owned_dataset() {
    let owner = Uuid::new_v4();
    let (state, db) = build_oss_state_as(user_in_tenant(owner, None)).await;

    seed_dataset_in_tenant(&db, "null_tenant", owner, None).await;
    seed_dataset_in_tenant(&db, "tenanted", owner, Some(Uuid::new_v4())).await;

    let app = build_router(state).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;

    let mut names = listed_names(resp).await;
    names.sort();
    assert_eq!(
        names,
        vec!["null_tenant".to_string(), "tenanted".to_string()],
        "an untenanted caller applies no tenant predicate — the OSS path must \
         list every dataset it owns"
    );
}

/// Scenario: no `AclDb`, and a dataset owned by somebody else.
/// Expected: not listed. The ownership predicate itself is untouched by this
/// change; this pins that the tenant filter was added *on top of* it rather
/// than in place of it.
/// Verification: seed a row under a different owner, assert it is absent.
#[tokio::test]
#[serial_test::serial]
async fn the_ownership_fallback_still_excludes_other_owners() {
    let owner = Uuid::new_v4();
    let (state, db) = build_oss_state_as(user_in_tenant(owner, None)).await;

    seed_dataset_in_tenant(&db, "mine", owner, None).await;
    seed_dataset_in_tenant(&db, "theirs", Uuid::new_v4(), None).await;

    let app = build_router(state).await.expect("router");
    let resp = oneshot_get(app, "/api/v1/datasets").await;

    assert_eq!(
        listed_names(resp).await,
        vec!["mine".to_string()],
        "another owner's dataset must not be listed"
    );
}
