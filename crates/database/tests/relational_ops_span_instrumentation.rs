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

// `RoleDb`/`TenantDb`/`UserDb` moved to the closed `cognee-access-control`
// crate as part of T2-move (oss-split-plan §4 S2). The smoke tests that
// asserted spans for their direct-DB impls (`role_list_roles_in_tenant_*`,
// `tenant_list_tenants_for_user_*`, `user_list_users_*`,
// `acl_authorized_dataset_ids_*`) moved with them. The OSS surface here
// retains spans for the remaining ops modules only.
use cognee_database::{
    CostByModelRow, DatabaseConnection, NotebookDb, SearchHistoryDb, SessionLifecycleDb, connect,
    initialize, ops, seed_tutorials_if_first_call,
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

// ─── ops/acl.rs: direct-DB span coverage moved to the closed
//     cognee-access-control crate's tests (T2-move). The trait-only
//     helper `grant_all_permissions_on_dataset_via_trait` retains no
//     own span (it wraps trait methods whose spans are emitted by the
//     concrete impl).

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

// ─── ops/role.rs: moved to cognee-access-control (T2-move).

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

// ─── ops/tenant.rs: moved to cognee-access-control (T2-move).

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

// ─── ops/user.rs: moved to cognee-access-control (T2-move).

// ─── compile-time export sanity ──────────────────────────────────────────────

#[allow(dead_code)]
fn _exports_compile(_db: &DatabaseConnection, _user: Uuid, _ds: Uuid, _perm: &str) {
    // Touch a remaining OSS trait so the import is recognised as used.
    let _: &dyn NotebookDb = _db;
}
