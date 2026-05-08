//! Test utilities for cognee-rust crates.
//!
//! Re-exports mock implementations and provides helpers for constructing
//! [`TaskContext`] in tests without requiring real database backends.
//!
//! Also exposes [`pg_test_url`] for building a PostgreSQL connection URL from
//! the `DB_*` environment variables (mirroring the Python `DB_PROVIDER` /
//! `DB_HOST` / … convention).

pub mod mock_acl_db;
pub mod mock_llm;
pub mod mock_role_db;
pub mod mock_tenant_db;
pub mod mock_transcriber;
pub mod mock_user_db;
pub mod span_capture;

use std::sync::Arc;

use cognee_core::{CancellationHandle, RayonThreadPool, TaskContext, TaskContextBuilder};
use cognee_database::DatabaseConnection;

pub use cognee_graph::MockGraphDB;
pub use cognee_storage::MockStorage;
pub use cognee_vector::MockVectorDB;
pub use mock_acl_db::MockAclDb;
pub use mock_llm::MockLlm;
pub use mock_role_db::MockRoleDb;
pub use mock_tenant_db::MockTenantDb;
pub use mock_transcriber::MockTranscriber;
pub use mock_user_db::MockUserDb;
pub use span_capture::{CapturedSpan, SpanCapture, SpanCaptureGuard};

/// Returns a PostgreSQL connection URL built from environment variables, or `None`
/// if `DB_PROVIDER` is not set to `"postgres"`.
///
/// Reads the following env vars (matching Python's `DB_*` convention):
/// - `DB_PROVIDER` — must equal `"postgres"` to activate
/// - `DB_HOST` — defaults to `"localhost"`
/// - `DB_PORT` — defaults to `"5432"`
/// - `DB_NAME` — defaults to `"cognee_db"`
/// - `DB_USERNAME` — defaults to `"postgres"`
/// - `DB_PASSWORD` — defaults to `""` (empty)
///
/// Tests that call this should skip gracefully when `None` is returned:
/// ```rust,ignore
/// let Some(url) = cognee_test_utils::pg_test_url() else { return };
/// ```
pub fn pg_test_url() -> Option<String> {
    let provider = std::env::var("DB_PROVIDER").unwrap_or_default();
    if provider != "postgres" {
        return None;
    }
    let host = std::env::var("DB_HOST").unwrap_or_else(|_| "localhost".to_string());
    let port = std::env::var("DB_PORT").unwrap_or_else(|_| "5432".to_string());
    let name = std::env::var("DB_NAME").unwrap_or_else(|_| "cognee_db".to_string());
    let user = std::env::var("DB_USERNAME").unwrap_or_else(|_| "postgres".to_string());
    let pass = std::env::var("DB_PASSWORD").unwrap_or_default();
    Some(format!("postgres://{user}:{pass}@{host}:{port}/{name}"))
}

/// Build a [`TaskContext`] with all-mock backends and an in-memory SQLite database.
///
/// Returns `(CancellationHandle, Arc<TaskContext>, Arc<DatabaseConnection>)`.
/// The `DatabaseConnection` is exposed so callers can perform direct DB queries
/// in assertions.
pub async fn test_task_context() -> (
    CancellationHandle,
    Arc<TaskContext>,
    Arc<DatabaseConnection>,
) {
    let db = cognee_database::connect("sqlite::memory:").await.unwrap();
    cognee_database::initialize(&db).await.unwrap();
    let db = Arc::new(db);

    let (handle, ctx) = TaskContextBuilder::new()
        .thread_pool(Arc::new(RayonThreadPool::with_default_threads().unwrap()))
        .database(db.clone())
        .graph_db(Arc::new(MockGraphDB::new()))
        .vector_db(Arc::new(MockVectorDB::new()))
        .build()
        .unwrap();

    (handle, Arc::new(ctx), db)
}
