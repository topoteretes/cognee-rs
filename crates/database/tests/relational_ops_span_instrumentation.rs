#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Smoke tests for span emission across `crates/database/src/ops/*`.
//!
//! Each test drives one representative operation per ops file and
//! asserts that the corresponding `cognee.db.relational.<file>.<fn>`
//! span fires with `cognee.db.system="sqlite"` (in-memory SQLite is
//! used for the test backend).
#![cfg(feature = "sqlite")]

use cognee_database::{
    AclDb, CostByModelRow, DatabaseConnection, NotebookDb, RoleDb, SearchHistoryDb,
    SessionLifecycleDb, TenantDb, UserDb, connect, initialize, ops, seed_tutorials_if_first_call,
};
use cognee_test_utils::{CapturedSpan, SpanCapture};
use uuid::Uuid;

async fn make_db() -> DatabaseConnection {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    db
}

fn assert_relational_span(spans: &[CapturedSpan], expected: &str) {
    let names: Vec<String> = spans.iter().map(|s| s.name.clone()).collect();
    assert!(
        spans
            .iter()
            .any(|s| s.name == expected
                && s.field_str("cognee.db.system").as_deref() == Some("sqlite")),
        "missing span {expected} (system=sqlite); saw spans: {names:?}",
    );
}

// ─── ops/acl.rs ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn acl_authorized_dataset_ids_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;
    let principal = Uuid::new_v4();

    // Read path: empty result is fine and does not require seeding
    // permissions.
    let _ = ops::acl::authorized_dataset_ids(&db, principal, "read").await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.acl.authorized_dataset_ids",
    );
}

// ─── ops/checkpoint.rs ───────────────────────────────────────────────────────

#[tokio::test]
async fn checkpoint_load_checkpoint_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = ops::checkpoint::load_checkpoint(&db, "test-key").await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.checkpoint.load_checkpoint",
    );
}

// ─── ops/data.rs ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn data_get_data_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = ops::data::get_data(&db, Uuid::new_v4()).await;

    assert_relational_span(&capture.spans(), "cognee.db.relational.data.get_data");
}

// ─── ops/datasets.rs ─────────────────────────────────────────────────────────

#[tokio::test]
async fn datasets_list_datasets_by_owner_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = ops::datasets::list_datasets_by_owner(&db, Uuid::new_v4()).await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.datasets.list_datasets_by_owner",
    );
}

// ─── ops/graph_storage.rs ────────────────────────────────────────────────────

#[tokio::test]
async fn graph_storage_get_nodes_by_dataset_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = ops::graph_storage::get_nodes_by_dataset(&db, Uuid::new_v4()).await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.graph_storage.get_nodes_by_dataset",
    );
}

// ─── ops/notebooks.rs (NotebookDb trait impl) ────────────────────────────────

#[tokio::test]
async fn notebooks_list_by_owner_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = NotebookDb::list_by_owner(&db, Uuid::new_v4()).await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.notebooks.list_by_owner",
    );
}

// ─── ops/pipeline_runs.rs ────────────────────────────────────────────────────

#[tokio::test]
async fn pipeline_runs_get_pipeline_run_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = ops::pipeline_runs::get_pipeline_run(&db, Uuid::new_v4()).await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.pipeline_runs.get_pipeline_run",
    );
}

// ─── ops/role.rs (RoleDb trait impl) ─────────────────────────────────────────

#[tokio::test]
async fn role_list_roles_in_tenant_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = RoleDb::list_roles_in_tenant(&db, Uuid::new_v4()).await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.role.list_roles_in_tenant",
    );
}

// ─── ops/search_history.rs ───────────────────────────────────────────────────

#[tokio::test]
async fn search_history_get_history_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    // SearchHistoryDb trait method maps to ops::search_history::get_history.
    let _ = SearchHistoryDb::get_history(&db, None, Some(10)).await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.search_history.get_history",
    );
}

// ─── ops/session_lifecycle.rs (SessionLifecycleDb trait impl) ────────────────

#[tokio::test]
async fn session_lifecycle_aggregate_stats_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    // Read-only path; no rows in DB is fine.
    let _: Result<_, _> = SessionLifecycleDb::aggregate_stats(&db, Uuid::new_v4(), &[], None).await;
    // Compile-time check that CostByModelRow is exported (used elsewhere in
    // this file's imports list to mirror the SessionLifecycleDb facet).
    let _phantom: Option<CostByModelRow> = None;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.session_lifecycle.aggregate_stats",
    );
}

// ─── ops/task_runs.rs ────────────────────────────────────────────────────────

#[tokio::test]
async fn task_runs_update_task_run_status_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    // No rows match — but the span still fires because the instrumented
    // function executes a fire-and-forget UPDATE.
    let _ = ops::task_runs::update_task_run_status(&db, Uuid::new_v4(), "running").await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.task_runs.update_task_run_status",
    );
}

// ─── ops/tenant.rs (TenantDb trait impl) ─────────────────────────────────────

#[tokio::test]
async fn tenant_list_tenants_for_user_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = TenantDb::list_tenants_for_user(&db, Uuid::new_v4()).await;

    assert_relational_span(
        &capture.spans(),
        "cognee.db.relational.tenant.list_tenants_for_user",
    );
}

// ─── ops/tutorial_seeder.rs ──────────────────────────────────────────────────

#[tokio::test]
async fn tutorial_seeder_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;
    let user_id = Uuid::new_v4();

    // Best-effort: this seeds tutorial notebooks and may succeed or fail
    // depending on schema availability — what matters is the span fires.
    let _ = seed_tutorials_if_first_call(&db, user_id).await;

    // The tutorial seeder span itself doesn't record cognee.db.system
    // (it's a higher-level orchestrator that delegates to NotebookDb
    // ops), so we only assert the span name fires.
    let spans = capture.spans();
    assert!(
        spans
            .iter()
            .any(|s| s.name == "cognee.db.relational.tutorial_seeder.seed_tutorials_if_first_call"),
        "expected tutorial seeder span; got: {:?}",
        spans.iter().map(|s| &s.name).collect::<Vec<_>>(),
    );
}

// ─── ops/user.rs (UserDb trait impl) ─────────────────────────────────────────

#[tokio::test]
async fn user_list_users_emits_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;

    let _ = UserDb::list_users(&db, None).await;

    assert_relational_span(&capture.spans(), "cognee.db.relational.user.list_users");
}

// ─── compile-time export sanity ──────────────────────────────────────────────

#[allow(dead_code)]
fn _exports_compile(_db: &DatabaseConnection, _user: Uuid, _ds: Uuid, _perm: &str) {
    // Touch the `AclDb` trait so the import is recognised as used.
    let _: &dyn AclDb = _db;
}
